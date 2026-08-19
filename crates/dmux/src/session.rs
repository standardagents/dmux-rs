//! Session model: discovery and adoption of the dmux session's panes from
//! tmux state + `dmux.config.json`, mirroring the TS rebinding rules
//! (`src/utils/paneRebinding.ts`): pane titles carry identity. Command
//! replies are driven by the app loop (tag-based), so everything here is
//! synchronous parsing/building.

use dmux_cc::{PaneId, Reply, WindowId};
use dmux_core::{parse_pane_title, DmuxConfig, DmuxPane, PaneKind};
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

/// The keepalive pane's start command — the durable identity. Window NAMES
/// are not reliable: automatic-rename setups rename the window to "sleep",
/// after which name-based detection misses it, reconciles re-create it, and
/// keepalives leak until the system runs out of PTYs (#10).
pub const KEEPALIVE_CMD: &str = "sleep 2147483647";

/// Keepalive detection by durable identity (start command), with the window
/// name as fallback for panes created by older builds where
/// `pane_start_command` may be absent from the listing.
pub fn is_keepalive(info: &TmuxPaneInfo) -> bool {
    info.window_name == KEEPALIVE_NAME
        || info.title == KEEPALIVE_NAME
        || info
            .start_command
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            == KEEPALIVE_CMD
}

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
    /// A confirmed close is in flight (#29): row shows a closing state,
    /// duplicate close commands are ignored, and the pane is removed only
    /// when tmux confirms the kill (or restored if it fails).
    pub closing: bool,
    /// Settled while unfocused — shown as `!` until the user looks.
    pub needs_attention: bool,
    /// Title follows the pane's own title reports (shell panes without a
    /// human-chosen name; cleared by rename).
    pub auto_name: bool,
    /// The current title came from LLM naming; shell title reports no longer
    /// overwrite it, and re-naming happens on a relaxed cadence.
    pub llm_named: bool,
    pub llm_named_at: Option<std::time::Instant>,
    /// Heuristic settle classifier (dmux-status).
    pub engine: dmux_status::PaneStatusEngine,
    /// An LLM classification is in flight for this pane.
    pub analysis_inflight: bool,
    /// Record the pane's byte stream for the shadow verifier (set when
    /// DMUX_VERIFY is on). The recording is anchored at the last seed so a
    /// replay from empty state reproduces the live grid deterministically.
    pub record_stream: bool,
    /// Seed-anchored raw byte recording (verify mode only; empty otherwise).
    pub recent_output: Vec<u8>,
    /// The recording overflowed its cap since the last seed — replay is no
    /// longer deterministic from the start.
    pub ring_truncated: bool,
    /// Last shadow verification of this pane.
    pub last_verify: Option<std::time::Instant>,
    /// An issue was auto-filed for this pane; no more reports until the
    /// process reloads (which is also when a fixed build arrives).
    pub issue_filed: bool,
    pub worktree_path: Option<String>,
    /// The tmux pane was on the alternate screen at adoption time.
    pub alt_screen: bool,
    pub pane_pid: u32,
    /// Owning project root (None = the main project).
    pub project_root: Option<String>,
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
        // A pane that tmux reports on the alternate screen must seed onto
        // the alternate grid. Seeding it onto the primary grid left the
        // emulator with scrollback the real pane doesn't have: every
        // full-screen repaint scrolled a stale frame into history, and
        // wheel-scrolling showed overlapping frame fragments (#12).
        if self.alt_screen {
            self.term.advance(b"\x1b[?1049h");
        }
        self.reseed_buffer = Some(Vec::new());
        // A seed resets the stream recording anchor: from here, replaying
        // the recording from an empty grid reproduces the live grid.
        self.recent_output.clear();
        self.ring_truncated = false;
        self.dirty = true;
    }

    /// Apply the capture-pane reply that seeds this pane, then drain any
    /// output buffered while the capture was in flight.
    pub fn finish_reseed(&mut self, reply: &Reply, cursor: Option<(u16, u16)>) {
        let seed = seed_bytes(reply);
        self.advance_recorded(&seed);
        if let Some((x, y)) = cursor {
            self.advance_recorded(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
        }
        if let Some(buffered) = self.reseed_buffer.take() {
            for chunk in buffered {
                self.advance_recorded(&chunk);
            }
        }
        self.dirty = true;
    }

    /// Feed bytes to the emulator, mirroring them into the seed-anchored
    /// recording when the shadow verifier is on.
    pub fn advance_recorded(&mut self, bytes: &[u8]) -> Vec<dmux_vt::TermSideEffect> {
        if self.record_stream {
            self.recent_output.extend_from_slice(bytes);
            if self.recent_output.len() > crate::verify::RING_CAP {
                let cut = self.recent_output.len() - crate::verify::RING_CAP;
                self.recent_output.drain(..cut);
                self.ring_truncated = true;
            }
        }
        self.term.advance(bytes)
    }

    pub fn seed_command_visible(&self) -> String {
        // Visible screen only — the shadow verifier's oracle (history is
        // irrelevant for grid comparison).
        format!("capture-pane -epqN -t {}", self.tmux_pane)
    }

    pub fn seed_command(&self) -> String {
        // -N (preserve trailing spaces) is what keeps background runs alive:
        // BCE-filled cells (agent composer bands, banded padding rows)
        // serialize as real spaces under the open SGR. Never add -J here —
        // it implies -T, which throws exactly those trailing positions away
        // (and joining is unnecessary anyway: our emulator is sized to the
        // tmux pane, so wrapped rows land identically).
        if self.alt_screen {
            // Alt-screen apps (vim, TUIs) have no meaningful history; capture
            // just the visible screen so the seed matches what tmux shows.
            format!("capture-pane -epqN -t {}", self.tmux_pane)
        } else {
            format!(
                "capture-pane -epqN -t {} -S -{}",
                self.tmux_pane, SEED_HISTORY_LINES
            )
        }
    }

    pub fn cursor_command(&self) -> String {
        format!(
            "display-message -p -t {} '#{{cursor_x}}\u{1}#{{cursor_y}}\u{1}#{{alternate_on}}'",
            self.tmux_pane
        )
    }
}

/// Turn a `capture-pane -epqN` reply into the byte stream that reconstructs
/// tmux's grid exactly when fed to a fresh emulator. Shared by the reseed
/// path and the shadow verifier so both replay with identical semantics.
///
/// Per line: erase the row under a default pen first (rows revealed by
/// scrolling in long seeds BCE-fill with the carried SGR background —
/// residue tmux's grid doesn't have), save/restoring the cursor AND the
/// carried SGR around it (DECSC/DECRC) so tmux's lazy cross-line SGR
/// continuity — which the -N capture format relies on — is preserved.
/// No trailing-cell reconstruction: -N captures are faithful (BCE blanks
/// arrive as real spaces under their SGR). No CRLF after the last row (it
/// would scroll the grid), and the pen is reset at the end — the capture's
/// final SGR is not the app's real pen state, and later pen-dependent
/// output (BCE 2J) must not fill with a stale color.
pub fn seed_bytes(reply: &Reply) -> Vec<u8> {
    let mut seed: Vec<u8> = Vec::new();
    let count = reply.lines.len();
    for (i, line) in reply.lines.iter().enumerate() {
        seed.extend_from_slice(b"\x1b7\x1b[0m\x1b[2K\x1b8");
        seed.extend_from_slice(line);
        if i + 1 < count {
            seed.extend_from_slice(b"\r\n");
        }
    }
    seed.extend_from_slice(b"\x1b[0m");
    seed
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
    pub alternate_on: bool,
    pub current_command: String,
    pub window_name: String,
    pub pane_pid: u32,
    /// `#{pane_start_command}` — survives window renames (keepalive identity).
    pub start_command: String,
}

pub fn list_panes_command() -> String {
    "list-panes -s -F '#{pane_id}\u{1}#{window_id}\u{1}#{pane_title}\u{1}#{pane_width}\u{1}#{pane_height}\u{1}#{alternate_on}\u{1}#{pane_current_command}\u{1}#{window_name}\u{1}#{pane_pid}\u{1}#{pane_start_command}'".to_string()
}

pub fn parse_pane_list(reply: &Reply) -> Vec<TmuxPaneInfo> {
    let mut out = Vec::new();
    for line in reply.text_lines() {
        let parts: Vec<&str> = line.split('\u{1}').collect();
        if parts.len() < 8 {
            continue;
        }
        let pane_pid = parts.get(8).and_then(|s| s.parse().ok()).unwrap_or(0);
        let (Some(pane), Some(window)) = (
            parts[0]
                .strip_prefix('%')
                .and_then(|s| s.parse().ok())
                .map(PaneId),
            parts[1]
                .strip_prefix('@')
                .and_then(|s| s.parse().ok())
                .map(WindowId),
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
            pane_pid,
            start_command: parts.get(9).unwrap_or(&"").to_string(),
        });
    }
    out
}

/// One recoverable pane from a persisted config (#20): what to recreate
/// after `tmux kill-server` wiped the live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestorePlan {
    /// Worktree agent pane: resume via the agent's session-resume path
    /// (exact `agentSessionId` when recorded — the executor reads it from
    /// the config record by slug).
    Agent {
        slug: String,
        display: String,
        path: String,
        agent: String,
    },
    /// Terminal tab: reopen as a fresh shell (scrollback/process state died
    /// with the server) in the saved cwd, or the project root fallback.
    Shell {
        slug: String,
        display: String,
        cwd: String,
        project_root: Option<String>,
    },
}

impl RestorePlan {
    pub fn slug(&self) -> &str {
        match self {
            RestorePlan::Agent { slug, .. } | RestorePlan::Shell { slug, .. } => slug,
        }
    }
}

/// Build the recovery manifest from a TS-compatible config after the tmux
/// server was reset (#20). Returns (plans, per-record skip notes). Legacy
/// infra records are excluded; a missing worktree skips that record with a
/// note instead of blocking the rest; a missing shell cwd falls back to the
/// project root. `path_exists` is injected so tests need no filesystem.
pub fn plan_session_restore(
    config: &DmuxConfig,
    project_root: &str,
    path_exists: &dyn Fn(&str) -> bool,
) -> (Vec<RestorePlan>, Vec<String>) {
    let mut plans = Vec::new();
    let mut skipped = Vec::new();
    for rec in &config.panes {
        let slug = rec.slug.clone();
        if slug.is_empty()
            || slug == "dmux"
            || slug == "Welcome"
            || slug.starts_with("dmux-spacer")
            || slug.starts_with("dmux-hidden")
        {
            continue;
        }
        let display = rec.display_name.clone().unwrap_or_else(|| slug.clone());
        match (rec.kind(), rec.agent.clone()) {
            (PaneKind::Worktree, Some(agent)) => match rec.worktree_path.clone() {
                Some(path) if path_exists(&path) => {
                    plans.push(RestorePlan::Agent {
                        slug,
                        display,
                        path,
                        agent,
                    });
                }
                Some(path) => skipped.push(format!("{slug}: worktree missing ({path})")),
                None => skipped.push(format!("{slug}: agent record without a worktree path")),
            },
            _ => {
                let saved = rec.shell_cwd.clone().or_else(|| rec.worktree_path.clone());
                let cwd = match saved {
                    Some(dir) if path_exists(&dir) => dir,
                    _ => project_root.to_string(),
                };
                let project_root_field = rec
                    .project_root
                    .clone()
                    .filter(|r| r != project_root && path_exists(r));
                plans.push(RestorePlan::Shell {
                    slug,
                    display,
                    cwd,
                    project_root: project_root_field,
                });
            }
        }
    }
    (plans, skipped)
}

/// Move a pane to `dst`'s position in the display order (#26). Refuses
/// cross-project moves (reordering must not silently change ownership) and
/// out-of-range indices; returns whether the order changed. tmux window
/// order is untouched — display order is an application-level concept.
pub fn move_pane(panes: &mut Vec<LogicalPane>, src: usize, dst: usize) -> bool {
    if src == dst || src >= panes.len() || dst >= panes.len() {
        return false;
    }
    if panes[src].project_root != panes[dst].project_root {
        return false;
    }
    let pane = panes.remove(src);
    panes.insert(dst, pane);
    true
}

/// Stable-order config records to match the live display order (#26):
/// records for live slugs sort into that order, everything else keeps its
/// relative position after them.
pub fn order_records(records: &mut [DmuxPane], slug_order: &[String]) {
    records.sort_by_key(|r| {
        slug_order
            .iter()
            .position(|s| *s == r.slug)
            .unwrap_or(usize::MAX)
    });
}

/// Stable-order live panes by the persisted record order (#26): slugs the
/// config knows sort first in config order; unknown panes keep adoption
/// order after them.
pub fn order_panes(panes: &mut [LogicalPane], slug_order: &[String]) {
    panes.sort_by_key(|p| {
        slug_order
            .iter()
            .position(|s| *s == p.slug)
            .unwrap_or(usize::MAX)
    });
}

/// Decide which tmux panes are content panes and pair them with config
/// entries by slug (via the title contract). Config panes with no live tmux
/// pane are skipped in Phase 0 (recreation is a Phase 1 concern); live panes
/// with no config entry are still adopted (matches TS behavior of showing
/// externally created panes).
pub fn adopt_panes(config: Option<&DmuxConfig>, infos: &[TmuxPaneInfo]) -> Vec<LogicalPane> {
    let mut adopted = Vec::new();
    for info in infos {
        if is_infra(&info.title, &info.window_name) || is_keepalive(info) {
            continue;
        }
        let parsed = parse_pane_title(&info.title);
        let config_pane = config.and_then(|c| {
            c.panes
                .iter()
                .find(|p| p.slug == parsed.slug || p.pane_id == info.pane.to_string())
        });
        let slug = config_pane
            .map(|p| p.slug.clone())
            .unwrap_or_else(|| parsed.slug.clone());
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
            closing: false,
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
            auto_name: config_pane
                .map(|p| p.kind() == PaneKind::Shell && p.display_name.is_none())
                .unwrap_or(true),
            llm_named: false,
            llm_named_at: None,
            engine: dmux_status::PaneStatusEngine::new(),
            analysis_inflight: false,
            record_stream: false,
            recent_output: Vec::new(),
            ring_truncated: false,
            last_verify: None,
            issue_filed: false,
            worktree_path: config_pane.and_then(|p| p.worktree_path.clone()),
            alt_screen: info.alternate_on,
            pane_pid: info.pane_pid,
            project_root: config_pane.and_then(|p| p.project_root.clone()),
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
    fn seed_restores_background_bands_from_dash_n_capture() {
        // seed_command captures with -N, so BCE-filled cells (composer bands,
        // banded padding rows) arrive as real spaces under their SGR. The
        // replay must reproduce them exactly — and must NOT invent bands on
        // rows whose trailing cells the capture left out (default blanks).
        let reply = reply_of(&["%5\u{1}@0\u{1}p__dmux__p\u{1}30\u{1}5\u{1}0\u{1}zsh\u{1}w"]);
        let infos = parse_pane_list(&reply);
        let mut pane = adopt_panes(None, &infos).remove(0);
        pane.begin_reseed();
        let band_pad = format!("\u{1b}[48;5;236m{}", " ".repeat(30));
        let band_text = format!("> say hello to me{}", " ".repeat(13));
        let seed = reply_of(&[
            &band_pad,                          // banded blank padding row (row 0)
            &band_text,                         // banded text row, SGR carried over (row 1)
            "",                                 // default blank row (row 2)
            "\u{1b}[49mplain\u{1b}[48;5;236mX", // row 3: default text, one banded X, rest default
        ]);
        pane.finish_reseed(&seed, None);

        let mut buf = dmux_compositor::CellBuffer::new(30, 5);
        pane.term
            .render_into(&mut buf, dmux_compositor::Rect::new(0, 0, 30, 5));
        let band = dmux_compositor::Color::Indexed(236);
        let default = dmux_compositor::Color::Default;
        // Padding row and text row: banded edge to edge.
        assert_eq!(buf.get(0, 0).bg, band, "padding row must be banded");
        assert_eq!(
            buf.get(29, 0).bg,
            band,
            "padding row must span the full width"
        );
        assert_eq!(buf.get(5, 1).bg, band);
        assert_eq!(
            buf.get(29, 1).bg,
            band,
            "text row band must span the full width"
        );
        // Default blank row stays default despite band rows around it.
        assert_eq!(buf.get(29, 2).bg, default, "blank row must not be banded");
        // Open SGR at end-of-line must not band unused trailing cells.
        assert_eq!(buf.get(5, 3).bg, band, "the X itself is banded");
        assert_eq!(
            buf.get(29, 3).bg,
            default,
            "unused trailing cells stay default"
        );
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
        assert!(
            tail.contains("seede live") || tail.contains("seeded"),
            "tail: {tail:?}"
        );
        assert!(pane.reseed_buffer.is_none());
    }

    #[test]
    fn alt_screen_pane_seeds_onto_alt_grid() {
        // #12: a pane tmux reports as alternate_on must seed onto the alt
        // grid. On the primary grid, every full-screen repaint scrolled a
        // stale frame into scrollback the real pane doesn't have, and
        // wheel-scrolling rendered overlapping frame fragments.
        let reply = reply_of(&["%7\u{1}@1\u{1}p__cc__p\u{1}30\u{1}5\u{1}1\u{1}node\u{1}w"]);
        let infos = parse_pane_list(&reply);
        assert!(infos[0].alternate_on);
        let mut pane = adopt_panes(None, &infos).remove(0);
        assert!(pane.alt_screen);
        pane.begin_reseed();
        pane.finish_reseed(&reply_of(&["transcript row"]), None);
        assert!(
            pane.term.input_modes().alt_screen,
            "seed must land on the alt grid"
        );
        // Repaint churn must not accumulate history…
        for i in 0..50 {
            pane.advance_recorded(format!("frame {i}\r\n").as_bytes());
        }
        assert_eq!(pane.term.history_len(), 0, "alt grid has no scrollback");
        // …and the local view can't scroll into stale frames.
        assert_eq!(pane.term.scroll_view(3), 0);
    }

    #[test]
    fn keepalive_detected_after_automatic_rename() {
        // #10: automatic-rename configs rename the keepalive window to
        // "sleep"; identity must survive via the start command, or every
        // reconcile re-creates the keepalive until PTYs run out.
        let mk = |window_name: &str, start: &str| TmuxPaneInfo {
            pane: PaneId(1),
            window: WindowId(1),
            title: "host".into(),
            width: 80,
            height: 24,
            alternate_on: false,
            current_command: "sleep".into(),
            window_name: window_name.into(),
            pane_pid: 42,
            start_command: start.into(),
        };
        // Renamed by automatic-rename: still a keepalive.
        assert!(is_keepalive(&mk("sleep", KEEPALIVE_CMD)));
        // tmux may quote the start command in formats.
        assert!(is_keepalive(&mk("sleep", "'sleep 2147483647'")));
        // Legacy builds: name only, no start_command field.
        assert!(is_keepalive(&mk(KEEPALIVE_NAME, "")));
        // A user's own sleep is NOT a keepalive (different duration)…
        assert!(!is_keepalive(&mk("sleep", "sleep 30")));
        // …and neither is an ordinary shell window.
        assert!(!is_keepalive(&mk("zsh", "")));
    }

    #[test]
    fn reorder_moves_within_project_and_persists() {
        // Three panes: two in the main project, one owned by another
        // project; a hidden pane reorders like any other (#26).
        let reply = reply_of(&[
            "%1\u{1}@1\u{1}p__aa__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
            "%2\u{1}@2\u{1}p__bb__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
            "%3\u{1}@3\u{1}p__cc__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
        ]);
        let mut panes = adopt_panes(None, &parse_pane_list(&reply));
        assert_eq!(panes.len(), 3);
        panes[1].hidden = true;
        panes[2].project_root = Some("/other".into());
        let slugs = |p: &[LogicalPane]| p.iter().map(|x| x.slug.clone()).collect::<Vec<_>>();

        // Hidden pane moves fine within its project.
        assert!(move_pane(&mut panes, 1, 0));
        assert_eq!(slugs(&panes), ["p__bb__p", "p__aa__p", "p__cc__p"]);
        assert!(panes[0].hidden, "hidden state rides along");

        // Cross-project moves are refused and change nothing.
        assert!(!move_pane(&mut panes, 2, 0));
        assert_eq!(slugs(&panes), ["p__bb__p", "p__aa__p", "p__cc__p"]);
        // Out-of-range and no-op moves are refused.
        assert!(!move_pane(&mut panes, 0, 9));
        assert!(!move_pane(&mut panes, 1, 1));

        // Persistence round trip: records follow the live order; unknown
        // records keep their relative order at the end; adoption ordering
        // restores the live order from records.
        let mut records: Vec<dmux_core::DmuxPane> = ["p__aa__p", "p__bb__p", "zz", "p__cc__p"]
            .iter()
            .map(|slug| {
                serde_json::from_value(serde_json::json!({
                    "id": *slug, "slug": *slug, "prompt": "", "paneId": "%9"
                }))
                .unwrap()
            })
            .collect();
        let live_order: Vec<String> = slugs(&panes);
        order_records(&mut records, &live_order);
        let rec_slugs: Vec<&str> = records.iter().map(|r| r.slug.as_str()).collect();
        assert_eq!(rec_slugs, ["p__bb__p", "p__aa__p", "p__cc__p", "zz"]);

        // A fresh adoption (tmux order) re-sorts to the persisted order.
        let mut readopted = adopt_panes(None, &parse_pane_list(&reply));
        let record_order: Vec<String> = records.iter().map(|r| r.slug.clone()).collect();
        order_panes(&mut readopted, &record_order);
        assert_eq!(slugs(&readopted), ["p__bb__p", "p__aa__p", "p__cc__p"]);
    }

    #[test]
    fn restore_plan_covers_representative_ts_config() {
        // #20: agent + shell + hidden + missing-path + multi-project +
        // legacy-infra records from a TS-written config.
        let config: DmuxConfig = serde_json::from_str(
            r#"{
              "projectName": "app",
              "projectRoot": "/main",
              "panes": [
                {"id":"1","slug":"fix-auth","prompt":"","paneId":"%9","type":"worktree",
                 "worktreePath":"/main/.wt/fix-auth","agent":"claude","agentSessionId":"sess-123"},
                {"id":"2","slug":"gone-wt","prompt":"","paneId":"%10","type":"worktree",
                 "worktreePath":"/main/.wt/deleted","agent":"claude"},
                {"id":"3","slug":"terminal-1","prompt":"","paneId":"%11","type":"shell",
                 "displayName":"logs","shellCwd":"/main/logs","hidden":true},
                {"id":"4","slug":"terminal-2","prompt":"","paneId":"%12","type":"shell",
                 "shellCwd":"/tmp/gone-dir"},
                {"id":"5","slug":"other-term","prompt":"","paneId":"%13","type":"shell",
                 "shellCwd":"/other","projectRoot":"/other"},
                {"id":"6","slug":"dmux","prompt":"","paneId":"%1"},
                {"id":"7","slug":"dmux-spacer-1","prompt":"","paneId":"%2"}
              ]
            }"#,
        )
        .unwrap();
        let exists =
            |p: &str| matches!(p, "/main/.wt/fix-auth" | "/main/logs" | "/other" | "/main");
        let (plans, skipped) = plan_session_restore(&config, "/main", &exists);
        assert_eq!(
            plans,
            vec![
                RestorePlan::Agent {
                    slug: "fix-auth".into(),
                    display: "fix-auth".into(),
                    path: "/main/.wt/fix-auth".into(),
                    agent: "claude".into(),
                },
                RestorePlan::Shell {
                    slug: "terminal-1".into(),
                    display: "logs".into(),
                    cwd: "/main/logs".into(),
                    project_root: None,
                },
                // Saved cwd is gone: falls back to the project root.
                RestorePlan::Shell {
                    slug: "terminal-2".into(),
                    display: "terminal-2".into(),
                    cwd: "/main".into(),
                    project_root: None,
                },
                // Other project's terminal keeps its project association.
                RestorePlan::Shell {
                    slug: "other-term".into(),
                    display: "other-term".into(),
                    cwd: "/other".into(),
                    project_root: Some("/other".into()),
                },
            ]
        );
        // The missing worktree is reported, not fatal; infra records vanish.
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("gone-wt"));
    }

    #[test]
    fn escaped_pane_list_reply_decodes_to_records() {
        // #19: raw control-mode bytes from tmux 3.5a — the 0x01 field
        // separators arrive octal-escaped as the four bytes \001, and the
        // start command is double-quoted. Feed the actual wire bytes through
        // the parser, decode, and expect a keepalive record.
        let wire: &[u8] = b"%begin 1755600000 3 1\n%0\\001@0\\001Mac-Studio.local\\00180\\00124\\0010\\001sleep\\001dmux-keepalive\\0012555\\001\"sleep 2147483647\"\n%end 1755600000 3 1\n";
        let mut parser = dmux_cc::Parser::new();
        let mut events = Vec::new();
        parser.feed(wire, &mut events);
        let lines: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|e| match e {
                dmux_cc::CcEvent::ReplyLine(l) => Some(l.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 1, "one payload line, got {events:?}");
        let mut reply = Reply {
            lines,
            ok: true,
            rtt: std::time::Duration::ZERO,
        };
        // Undecoded, the reply yields no records (the pre-fix failure that
        // blinded the keepalive guards and re-leaked #10).
        assert!(parse_pane_list(&reply).is_empty());
        reply.unescape_lines();
        let infos = parse_pane_list(&reply);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].pane, PaneId(0));
        assert_eq!(infos[0].width, 80);
        assert_eq!(infos[0].window_name, "dmux-keepalive");
        assert!(is_keepalive(&infos[0]));
    }

    #[test]
    fn pane_list_parses_start_command() {
        let line =
            "%3\u{1}@2\u{1}t\u{1}80\u{1}24\u{1}0\u{1}sleep\u{1}sleep\u{1}9\u{1}sleep 2147483647";
        let reply = Reply {
            lines: vec![line.as_bytes().to_vec()],
            ok: true,
            rtt: std::time::Duration::ZERO,
        };
        let infos = parse_pane_list(&reply);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].start_command, "sleep 2147483647");
        assert!(is_keepalive(&infos[0]));
        // Older 9-field listings still parse (start_command empty).
        let line9 = "%3\u{1}@2\u{1}t\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w\u{1}9";
        let reply9 = Reply {
            lines: vec![line9.as_bytes().to_vec()],
            ok: true,
            rtt: std::time::Duration::ZERO,
        };
        assert_eq!(parse_pane_list(&reply9)[0].start_command, "");
    }
}
