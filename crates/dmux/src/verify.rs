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
            let ch_eq =
                a.ch == b.ch || (a.ch == ' ' && b.ch == '\0') || (a.ch == '\0' && b.ch == ' ');
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
        "dmux-rs render-verify incident\npane: {} ({}) {}x{}\ndiffs: {}\nreplay-deterministic: {}\n\n== first diffs ==\n",
        pane.slug,
        pane.tmux_pane,
        pane.cols,
        pane.rows,
        diffs.len(),
        if pane.ring_truncated { "no (recording overflowed since last seed)" } else { "yes" }
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

/// Parsed incident bundle: (cols, rows, capture lines, raw stream bytes).
pub fn parse_incident(text: &str) -> Option<(u16, u16, Vec<Vec<u8>>, Vec<u8>)> {
    let dims_line = text.lines().find(|l| l.starts_with("pane: "))?;
    let dims = dims_line.rsplit(' ').next()?;
    let (c, r) = dims.split_once('x')?;
    let (cols, rows) = (c.parse().ok()?, r.parse().ok()?);

    let cap_start = text.find("== tmux capture")?;
    let cap_body = &text[text[cap_start..].find('\n')? + cap_start + 1..];
    let cap_end = cap_body.find("\n== ")?;
    let capture: Vec<Vec<u8>> = cap_body[..cap_end].lines().map(unescape_default).collect();

    let ring_start = text.find("== raw %output ring")?;
    let ring_body = &text[text[ring_start..].find('\n')? + ring_start + 1..];
    let b64: String = ring_body.split_whitespace().collect();
    let ring = base64_decode(&b64)?;
    Some((cols, rows, capture, ring))
}

/// Inverse of `str::escape_default` for the capture section.
fn unescape_default(line: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('\\') => out.push(b'\\'),
            Some('\'') => out.push(b'\''),
            Some('"') => out.push(b'"'),
            Some('0') => out.push(0),
            Some('u') => {
                // \u{XXXX}
                if chars.next() == Some('{') {
                    let mut hex = String::new();
                    for h in chars.by_ref() {
                        if h == '}' {
                            break;
                        }
                        hex.push(h);
                    }
                    if let Ok(v) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(v) {
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                }
            }
            Some('x') => {
                let h: String = chars.by_ref().take(2).collect();
                if let Ok(v) = u8::from_str_radix(&h, 16) {
                    out.push(v);
                }
            }
            _ => {}
        }
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for ch in s.bytes() {
        if ch == b'=' {
            break;
        }
        let v = TABLE.iter().position(|&t| t == ch)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Replay an incident's recorded stream and compare against its stored tmux
/// capture — the offline form of the live verification. Returns the diffs.
pub fn replay_incident(text: &str) -> Option<Vec<String>> {
    let (cols, rows, capture, ring) = parse_incident(text)?;
    let mut live = PaneTerm::new(cols, rows, 0);
    live.advance(&ring);
    let reply = Reply {
        lines: capture,
        ok: true,
        rtt: std::time::Duration::ZERO,
    };
    let mut scratch = PaneTerm::new(cols, rows, 0);
    scratch.advance(&seed_bytes(&reply));
    let a = render_grid(&live, cols, rows);
    let b = render_grid(&scratch, cols, rows);
    let mut diffs = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let (x, y) = (a.get(col, row), b.get(col, row));
            let ch_eq =
                x.ch == y.ch || (x.ch == ' ' && y.ch == '\0') || (x.ch == '\0' && y.ch == ' ');
            let fg_eq = color_equiv(x.fg, y.fg, &live, 256);
            let bg_eq = color_equiv(x.bg, y.bg, &live, 257);
            let tolerated = x.ch == ' '
                && y.ch == ' '
                && fg_eq
                && y.bg == Color::Default
                && x.bg != Color::Default;
            if !(ch_eq && fg_eq && bg_eq) && !tolerated {
                diffs.push(format!(
                    "({col},{row}) replay={:?} capture={:?}",
                    x.ch, y.ch
                ));
            }
        }
    }
    Some(diffs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_with(content: &[u8]) -> LogicalPane {
        let reply = Reply {
            lines: vec!["%5\u{1}@0\u{1}p__dmux__p\u{1}30\u{1}4\u{1}0\u{1}zsh\u{1}w"
                .as_bytes()
                .to_vec()],
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
        assert!(
            compare(&pane, &cap).is_empty(),
            "identical content must not alert"
        );
    }

    /// Every incident promoted into tests/corpus/ must replay with zero
    /// diffs after its fix lands — real-world bugs become permanent
    /// regression tests.
    #[test]
    fn corpus_incidents_replay_clean() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("corpus");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("incident") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read corpus incident");
            let diffs = replay_incident(&text).expect("parse corpus incident");
            assert!(
                diffs.is_empty(),
                "corpus incident {:?} still diverges: {:?}",
                path.file_name(),
                &diffs[..diffs.len().min(5)]
            );
        }
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
        && pane
            .last_output
            .map(|t| now.duration_since(t) >= QUIESCE)
            .unwrap_or(false)
        && pane
            .last_verify
            .map(|t| now.duration_since(t) >= INTERVAL)
            .unwrap_or(true)
}
