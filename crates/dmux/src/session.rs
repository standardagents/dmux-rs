//! Session model: the tmux-facing protocol layer — pane-list parsing,
//! keepalive identity, seeding, restore planning, and the `LogicalPane`
//! type. Identity matching, adoption, record persistence, and ordering
//! policy live in `crate::registry` (#81).

use dmux_cc::{PaneId, Reply, WindowId};
use dmux_core::{DmuxConfig, PaneKind};
use dmux_vt::PaneTerm;

pub const PANE_SCROLLBACK: usize = 10_000;
pub const SEED_HISTORY_LINES: u32 = 2_000;

pub fn configure_extended_keys<T>(client: &dmux_cc::Client<T>) {
    let _ = client.send("set -g extended-keys on");
    let _ = client.send("set -g extended-keys-format csi-u");
    let _ = client.send("refresh-client -B 'dmux-key-mode:%*:#{pane_key_mode}'");
}

/// Names that mark dmux-owned infrastructure we never render: the TS-era
/// control/welcome/spacer panes plus our own session-keepalive window.
pub(crate) fn is_infra(title: &str, window_name: &str) -> bool {
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
    /// tmux's `pane_key_mode` is `Ext 2`, which requires every modified key
    /// to use the configured extended-key format.
    pub extended_keys_mode2: bool,
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

pub fn pane_input_modes(panes: &[LogicalPane], focused: usize) -> dmux_vt::InputModes {
    panes
        .get(focused)
        .map_or_else(dmux_vt::InputModes::default, |pane| {
            let mut modes = pane.term.input_modes();
            modes.extended_keys_mode2 |= pane.extended_keys_mode2;
            modes
        })
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
    pub extended_keys_mode2: bool,
    /// `#{pane_current_path}` — the pane's live working directory, used to
    /// recover project ownership for unmatched records (#76).
    pub current_path: String,
}

pub fn list_panes_command() -> String {
    "list-panes -s -F '#{pane_id}\u{1}#{window_id}\u{1}#{pane_title}\u{1}#{pane_width}\u{1}#{pane_height}\u{1}#{alternate_on}\u{1}#{pane_current_command}\u{1}#{window_name}\u{1}#{pane_pid}\u{1}#{pane_start_command}\u{1}#{pane_key_mode}\u{1}#{pane_current_path}'".to_string()
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
            extended_keys_mode2: parts.get(10) == Some(&"Ext 2"),
            current_path: parts.get(11).unwrap_or(&"").to_string(),
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

/// Pane status for an LLM verdict. Option dialogs ALWAYS land on Waiting —
/// dmux never auto-accepts one (#31); the agent's own autonomous mode is
/// the only thing allowed to press Enter.
pub fn verdict_pane_status(v: &dmux_infer::PaneVerdict) -> PaneStatus {
    match v {
        dmux_infer::PaneVerdict::OptionDialog => PaneStatus::Waiting,
        dmux_infer::PaneVerdict::OpenPrompt => PaneStatus::Idle,
        dmux_infer::PaneVerdict::InProgress => PaneStatus::Working,
    }
}

#[cfg(test)]
mod tests;
