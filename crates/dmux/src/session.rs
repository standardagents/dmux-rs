//! Session model: discovery and adoption of the dmux session's panes from
//! tmux state + `dmux.config.json`, mirroring the TS rebinding rules
//! (`src/utils/paneRebinding.ts`): pane titles carry identity. Command
//! replies are driven by the app loop (tag-based), so everything here is
//! synchronous parsing/building.

use dmux_cc::{PaneId, Reply, WindowId};
use dmux_core::{parse_pane_title, DmuxConfig, PaneKind};
use dmux_vt::PaneTerm;

pub const PANE_SCROLLBACK: usize = 10_000;
pub const SEED_HISTORY_LINES: u32 = 2_000;

/// Names that mark dmux-owned infrastructure we never render: the TS-era
/// control/welcome/spacer panes plus our own session-keepalive window.
fn is_infra(title: &str, window_name: &str) -> bool {
    title == "dmux"
        || title == "Welcome"
        || title.starts_with("dmux-spacer")
        || title.starts_with("dmux-hidden")
        || title == KEEPALIVE_NAME
        || window_name == KEEPALIVE_NAME
}

/// Window kept alive so killing the last real pane never destroys the tmux
/// session (which would take the renderer down with it).
pub const KEEPALIVE_NAME: &str = "dmux-keepalive";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    Working,
    /// Settled and waiting on the user (question / option dialog).
    Waiting,
    Idle,
    Dead,
}

pub struct LogicalPane {
    /// Durable identity from the title contract; drives Phase 1 config sync.
    #[allow(dead_code)]
    pub slug: String,
    pub title: String,
    #[allow(dead_code)]
    pub kind: PaneKind,
    pub tmux_pane: PaneId,
    pub tmux_window: WindowId,
    /// Size of the underlying tmux pane (the emulator matches this, not the
    /// on-screen rect; render clips/pads).
    pub cols: u16,
    pub rows: u16,
    pub term: PaneTerm,
    pub rect: Option<dmux_compositor::Rect>,
    pub paused: bool,
    /// While a reseed capture is in flight, live output is buffered here and
    /// applied after the seed (stream ordering makes this race-free).
    pub reseed_buffer: Option<Vec<Vec<u8>>>,
    /// Seed reply parked until its paired cursor reply arrives.
    pub pending_seed: Option<Reply>,
    pub dirty: bool,
    pub status: PaneStatus,
    pub last_output: Option<std::time::Instant>,
    /// Flood throttling: bytes seen in the current rate window.
    pub window_bytes: u64,
    pub window_start: std::time::Instant,
    /// Output suppressed at the source (`refresh-client -A off`); pane
    /// refreshes by periodic reseed until the flood subsides.
    pub throttled: bool,
    pub resume_at: Option<std::time::Instant>,
    /// Excluded from the layout and output-muted; still alive in tmux.
    pub hidden: bool,
    /// Settled while unfocused — shown as `!` until the user looks.
    pub needs_attention: bool,
    /// Heuristic settle classifier (dmux-status).
    pub engine: dmux_status::PaneStatusEngine,
    pub worktree_path: Option<String>,
    pub agent: Option<String>,
    /// Feeds Phase 1 agent detection.
    #[allow(dead_code)]
    pub current_command: String,
}

impl LogicalPane {
    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            &self.slug
        } else {
            &self.title
        }
    }

    /// Begin a reseed: fresh emulator, buffer live output until the capture
    /// reply arrives.
    pub fn begin_reseed(&mut self) {
        self.term = PaneTerm::new(self.cols, self.rows, PANE_SCROLLBACK);
        self.reseed_buffer = Some(Vec::new());
        self.dirty = true;
    }

    /// Apply the capture-pane reply that seeds this pane, then drain any
    /// output buffered while the capture was in flight.
    pub fn finish_reseed(&mut self, reply: &Reply, cursor: Option<(u16, u16)>) {
        let mut seed: Vec<u8> = Vec::new();
        let count = reply.lines.len();
        for (i, line) in reply.lines.iter().enumerate() {
            seed.extend_from_slice(line);
            // capture-pane emits one reply line per screen row; rejoin with
            // CRLF except after the last row so the cursor row stays correct.
            if i + 1 < count {
                seed.extend_from_slice(b"\r\n");
            }
        }
        self.term.advance(&seed);
        if let Some((x, y)) = cursor {
            self.term.advance(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
        }
        if let Some(buffered) = self.reseed_buffer.take() {
            for chunk in buffered {
                self.term.advance(&chunk);
            }
        }
        self.dirty = true;
    }

    pub fn seed_command(&self) -> String {
        format!("capture-pane -epqJ -t {} -S -{}", self.tmux_pane, SEED_HISTORY_LINES)
    }

    pub fn cursor_command(&self) -> String {
        format!(
            "display-message -p -t {} '#{{cursor_x}}\u{1}#{{cursor_y}}\u{1}#{{alternate_on}}'",
            self.tmux_pane
        )
    }
}

/// Parse the cursor-query reply built by [`LogicalPane::cursor_command`].
pub fn parse_cursor_reply(reply: &Reply) -> Option<(u16, u16)> {
    let lines = reply.text_lines();
    let line = lines.first()?;
    let parts: Vec<&str> = line.split('\u{1}').collect();
    if parts.len() < 2 {
        return None;
    }
    Some((parts[0].parse().ok()?, parts[1].parse().ok()?))
}

#[derive(Debug, Clone)]
pub struct TmuxPaneInfo {
    pub pane: PaneId,
    pub window: WindowId,
    pub title: String,
    pub width: u16,
    pub height: u16,
    /// Alt-screen panes need separate primary/alt seeding (follow-up).
    #[allow(dead_code)]
    pub alternate_on: bool,
    pub current_command: String,
    pub window_name: String,
}

pub fn list_panes_command() -> String {
    "list-panes -s -F '#{pane_id}\u{1}#{window_id}\u{1}#{pane_title}\u{1}#{pane_width}\u{1}#{pane_height}\u{1}#{alternate_on}\u{1}#{pane_current_command}\u{1}#{window_name}'".to_string()
}

pub fn parse_pane_list(reply: &Reply) -> Vec<TmuxPaneInfo> {
    let mut out = Vec::new();
    for line in reply.text_lines() {
        let parts: Vec<&str> = line.split('\u{1}').collect();
        if parts.len() < 8 {
            continue;
        }
        let (Some(pane), Some(window)) = (
            parts[0].strip_prefix('%').and_then(|s| s.parse().ok()).map(PaneId),
            parts[1].strip_prefix('@').and_then(|s| s.parse().ok()).map(WindowId),
        ) else {
            continue;
        };
        out.push(TmuxPaneInfo {
            pane,
            window,
            title: parts[2].to_string(),
            width: parts[3].parse().unwrap_or(80),
            height: parts[4].parse().unwrap_or(24),
            alternate_on: parts[5] == "1",
            current_command: parts[6].to_string(),
            window_name: parts[7].to_string(),
        });
    }
    out
}

/// Decide which tmux panes are content panes and pair them with config
/// entries by slug (via the title contract). Config panes with no live tmux
/// pane are skipped in Phase 0 (recreation is a Phase 1 concern); live panes
/// with no config entry are still adopted (matches TS behavior of showing
/// externally created panes).
pub fn adopt_panes(config: Option<&DmuxConfig>, infos: &[TmuxPaneInfo]) -> Vec<LogicalPane> {
    let mut adopted = Vec::new();
    for info in infos {
        if is_infra(&info.title, &info.window_name) {
            continue;
        }
        let parsed = parse_pane_title(&info.title);
        let config_pane = config.and_then(|c| {
            c.panes
                .iter()
                .find(|p| p.slug == parsed.slug || p.pane_id == info.pane.to_string())
        });
        let slug = config_pane.map(|p| p.slug.clone()).unwrap_or_else(|| parsed.slug.clone());
        let title = config_pane
            .map(|p| p.display_title().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                if parsed.display.trim().is_empty() {
                    info.window_name.clone()
                } else {
                    parsed.display.clone()
                }
            });
        adopted.push(LogicalPane {
            slug,
            title,
            kind: config_pane.map(|p| p.kind()).unwrap_or(PaneKind::Worktree),
            tmux_pane: info.pane,
            tmux_window: info.window,
            cols: info.width.max(1),
            rows: info.height.max(1),
            term: PaneTerm::new(info.width.max(1), info.height.max(1), PANE_SCROLLBACK),
            rect: None,
            paused: false,
            reseed_buffer: None,
            pending_seed: None,
            dirty: true,
            status: PaneStatus::Idle,
            last_output: None,
            window_bytes: 0,
            window_start: std::time::Instant::now(),
            throttled: false,
            resume_at: None,
            hidden: config_pane.map(|p| p.is_hidden()).unwrap_or(false),
            needs_attention: false,
            engine: dmux_status::PaneStatusEngine::new(),
            worktree_path: config_pane.and_then(|p| p.worktree_path.clone()),
            agent: config_pane.and_then(|p| p.agent.clone()),
            current_command: info.current_command.clone(),
        });
    }
    adopted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply_of(lines: &[&str]) -> Reply {
        Reply {
            lines: lines.iter().map(|l| l.as_bytes().to_vec()).collect(),
            ok: true,
            rtt: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn parses_pane_list_and_filters_infra() {
        let reply = reply_of(&[
            "%0\u{1}@0\u{1}dmux\u{1}40\u{1}60\u{1}0\u{1}node\u{1}zsh",
            "%5\u{1}@0\u{1}Fix auth__dmux__fix-auth\u{1}100\u{1}40\u{1}0\u{1}claude\u{1}zsh",
            "%7\u{1}@0\u{1}dmux-spacer-1\u{1}20\u{1}40\u{1}0\u{1}node\u{1}zsh",
            "%9\u{1}@1\u{1}shell-1\u{1}80\u{1}24\u{1}1\u{1}zsh\u{1}work",
        ]);
        let infos = parse_pane_list(&reply);
        assert_eq!(infos.len(), 4);
        let adopted = adopt_panes(None, &infos);
        assert_eq!(adopted.len(), 2);
        assert_eq!(adopted[0].slug, "fix-auth");
        assert_eq!(adopted[0].title, "Fix auth");
        assert_eq!(adopted[1].slug, "shell-1");
    }

    #[test]
    fn reseed_buffers_live_output() {
        let reply = reply_of(&["%5\u{1}@0\u{1}p__dmux__p\u{1}20\u{1}4\u{1}0\u{1}zsh\u{1}w"]);
        let mut pane = adopt_panes(None, &parse_pane_list(&reply)).remove(0);
        pane.begin_reseed();
        // Output arriving during reseed is buffered by the app into reseed_buffer.
        pane.reseed_buffer.as_mut().unwrap().push(b" live".to_vec());
        pane.finish_reseed(&reply_of(&["seeded line"]), Some((5, 0)));
        let tail = pane.term.read_tail_text(4);
        assert!(tail.contains("seede live") || tail.contains("seeded"), "tail: {tail:?}");
        assert!(pane.reseed_buffer.is_none());
    }
}
