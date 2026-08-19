//! Shadow render-verifier: with `DMUX_VERIFY=1`, settled panes are
//! periodically compared cell-for-cell against tmux's authoritative grid
//! (`capture-pane -epqN`, replayed through the exact seed semantics). Any
//! mismatch writes an incident file — both grids plus the raw `%output`
//! bytes that produced ours — turning a real-world rendering bug into a
//! reproducible regression test instead of a screenshot mystery.

use std::time::Instant;

use dmux_cc::Reply;
use dmux_compositor::{CellBuffer, Color, Rect};
use dmux_vt::PaneTerm;

use crate::session::{seed_bytes, LogicalPane};

/// Raw `%output` ring buffer capacity per pane (verify mode only).
pub const RING_CAP: usize = 256 * 1024;

/// How long a pane must be quiet before its grid is compared (tmux delivers
/// pane output and command replies on independent schedules, so comparing
/// mid-stream would race).
pub const QUIESCE: std::time::Duration = std::time::Duration::from_millis(1200);
/// Minimum spacing between verifications of the same pane.
pub const INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

fn render_grid(term: &PaneTerm, cols: u16, rows: u16) -> CellBuffer {
    let mut buf = CellBuffer::new(cols, rows);
    term.render_into(&mut buf, Rect::new(0, 0, cols, rows));
    buf
}

/// Colors compare as equivalent when they differ only by dynamic-palette
/// resolution: our live grid resolves OSC 10/11/4 to concrete RGB while the
/// scratch replay of the capture (which has no OSC state) yields the
/// unresolved form.
fn color_equiv(live: Color, scratch: Color, live_term: &PaneTerm, slot_default: usize) -> bool {
    if live == scratch {
        return true;
    }
    if let (Color::Rgb(r, g, b), unres) = (live, scratch) {
        let slot = match unres {
            Color::Default => slot_default,
            Color::Indexed(i) => i as usize,
            Color::Rgb(..) => return false,
        };
        if let Some(rgb) = live_term.palette_color(slot) {
            return rgb == (r, g, b);
        }
    }
    false
}

/// Compare the live pane grid against a capture reply. Returns differing
/// cell descriptions (bounded), tolerating documented divergences:
/// palette-resolved defaults and trailing backgrounds tmux compacted away.
pub fn compare(pane: &LogicalPane, reply: &Reply) -> Vec<String> {
    let cols = pane.cols;
    let rows = pane.rows;
    let mut scratch = PaneTerm::new(cols, rows, 0);
    scratch.advance(&seed_bytes(reply));
    let live = render_grid(&pane.term, cols, rows);
    let shadow = render_grid(&scratch, cols, rows);

    // NamedColor::Foreground / Background slots in the palette table.
    const SLOT_FG: usize = 256;
    const SLOT_BG: usize = 257;

    let mut diffs = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let a = live.get(col, row);
            let b = shadow.get(col, row);
            let ch_eq = a.ch == b.ch || (a.ch == ' ' && b.ch == '\0') || (a.ch == '\0' && b.ch == ' ');
            let fg_eq = color_equiv(a.fg, b.fg, &pane.term, SLOT_FG);
            let bg_eq = color_equiv(a.bg, b.bg, &pane.term, SLOT_BG);
            // tmux compacts trailing BCE backgrounds on scrolled rows; we
            // keep them (real-terminal semantics) — tolerate bg-only diffs
            // on blank cells where tmux reports plain default.
            let tolerated_bg = a.ch == ' '
                && b.ch == ' '
                && fg_eq
                && b.bg == Color::Default
                && a.bg != Color::Default;
            if !(ch_eq && fg_eq && bg_eq) && !tolerated_bg {
                if diffs.len() < 64 {
                    diffs.push(format!(
                        "({col},{row}) live={:?}/{:?}/{:?} tmux={:?}/{:?}/{:?}",
                        a.ch, a.fg, a.bg, b.ch, b.fg, b.bg
                    ));
                } else {
                    diffs.push("…".into());
                    return diffs;
                }
            }
        }
    }
    diffs
}

/// Write the full evidence bundle for a mismatch.
pub fn write_incident(
    home: &std::path::Path,
    pane: &LogicalPane,
    reply: &Reply,
    diffs: &[String],
) -> std::io::Result<std::path::PathBuf> {
    let dir = home.join(".dmux").join("incidents");
    std::fs::create_dir_all(&dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("render-{}-{}.txt", ts, pane.slug));

    let mut out = String::new();
    out.push_str(&format!(
        "dmux-rs render-verify incident\npane: {} ({}) {}x{}\ndiffs: {}\n\n== first diffs ==\n",
        pane.slug,
        pane.tmux_pane,
        pane.cols,
        pane.rows,
        diffs.len()
    ));
    for d in diffs {
        out.push_str(d);
        out.push('\n');
    }
    out.push_str("\n== our grid (text) ==\n");
    for row in 0..pane.rows {
        out.push_str(&pane.term.row_text_public(row));
        out.push('\n');
    }
    out.push_str("\n== tmux capture (-epqN, escaped) ==\n");
    for line in &reply.lines {
        out.push_str(&String::from_utf8_lossy(line).escape_default().to_string());
        out.push('\n');
    }
    out.push_str("\n== raw %output ring (base64; replay: base64 -d | griddump COLS ROWS raw) ==\n");
    out.push_str(&crate::base64(&pane.recent_output));
    out.push('\n');
    std::fs::write(&path, out)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_with(content: &[u8]) -> LogicalPane {
        let reply = Reply {
            lines: vec!["%5\u{1}@0\u{1}p__dmux__p\u{1}30\u{1}4\u{1}0\u{1}zsh\u{1}w".as_bytes().to_vec()],
            ok: true,
            rtt: std::time::Duration::ZERO,
        };
        let infos = crate::session::parse_pane_list(&reply);
        let mut pane = crate::session::adopt_panes(None, &infos).remove(0);
        pane.term.advance(content);
        pane
    }

    fn capture_of(lines: &[&str]) -> Reply {
        Reply {
            lines: lines.iter().map(|l| l.as_bytes().to_vec()).collect(),
            ok: true,
            rtt: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn matching_grids_produce_no_diffs() {
        let pane = pane_with(b"\x1b[48;5;236mhello band\x1b[K\x1b[0m\r\nplain");
        let cap = capture_of(&[
            "\u{1b}[48;5;236mhello band                    ",
            "\u{1b}[49mplain",
            "",
            "",
        ]);
        assert!(compare(&pane, &cap).is_empty(), "identical content must not alert");
    }

    #[test]
    fn real_divergence_is_caught() {
        let pane = pane_with(b"hello CORRUPT");
        let cap = capture_of(&["hello world", "", "", ""]);
        let diffs = compare(&pane, &cap);
        assert!(!diffs.is_empty(), "diverging content must be reported");
    }
}

/// Whether this pane is in a comparable state right now.
pub fn eligible(pane: &LogicalPane, now: Instant) -> bool {
    !pane.hidden
        && pane.rect.is_some()
        && !pane.paused
        && !pane.throttled
        && pane.reseed_buffer.is_none()
        && pane.term.display_offset() == 0
        && pane.last_output.map(|t| now.duration_since(t) >= QUIESCE).unwrap_or(false)
        && pane.last_verify.map(|t| now.duration_since(t) >= INTERVAL).unwrap_or(true)
}
