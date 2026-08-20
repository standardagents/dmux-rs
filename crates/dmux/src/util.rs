//! Small process-wide utilities shared across the app: encoding, shell
//! quoting, and timestamps.

/// Minimal base64 (standard alphabet, padded) for OSC 52 payloads.
pub(crate) fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Shell-quote a path/branch for the bootstrap command line.
pub(crate) fn shq(s: &str) -> String {
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub(crate) fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn iso_now() -> String {
    // Close-enough ISO timestamp without a chrono dependency (UTC seconds).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (mut y, mut rem_days) = (1970u64, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if rem_days < len {
            break;
        }
        rem_days -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_lens = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while rem_days >= month_lens[m] {
        rem_days -= month_lens[m];
        m += 1;
    }
    let tod = secs % 86_400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        y,
        m + 1,
        rem_days + 1,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

use std::time::{Duration, Instant};

/// Absolute animation schedule (#17): the next tick is pinned when armed and
/// advances only when a tick actually fires. Recomputing `now + interval`
/// each event-loop pass let any wakeup arriving inside the interval (pane
/// output, control messages) postpone the tick forever — spinners visibly
/// stalled under sustained output.
/// Transient cursor-anchored tooltip (#22): non-modal, uncapturable, and
/// self-expiring — "Copied to clipboard" beside the mouse-release point.
pub(crate) struct Tooltip {
    pub(crate) text: String,
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) until: Instant,
}

/// A wedged worktree hook must not block updates forever (#53).
pub(crate) const UPDATE_DEFER_CAP: Duration = Duration::from_secs(600);

/// Whether a deferred self-update may re-exec now (#53): only at a safe
/// boundary — no bootstrap mid-provisioning and no prompt injection queued —
/// or once the deferral cap expires.
pub(crate) fn update_may_apply(
    bootstraps_active: bool,
    injections_pending: usize,
    waited: Duration,
) -> bool {
    (!bootstraps_active && injections_pending == 0) || waited >= UPDATE_DEFER_CAP
}

#[derive(Default)]
pub(crate) struct AnimClock {
    next: Option<Instant>,
}

impl AnimClock {
    /// The pinned deadline, arming it from `now` if unarmed.
    pub(crate) fn deadline(&mut self, now: Instant, interval: Duration) -> Instant {
        *self.next.get_or_insert(now + interval)
    }

    /// True exactly when the pinned deadline has passed (or the clock was
    /// never armed); re-arms `interval` from `now` — no catch-up bursts.
    pub(crate) fn fire_if_due(&mut self, now: Instant, interval: Duration) -> bool {
        let due = self.next.map(|at| now >= at).unwrap_or(true);
        if due {
            self.next = Some(now + interval);
        }
        due
    }

    pub(crate) fn disarm(&mut self) {
        self.next = None;
    }
}

#[cfg(test)]
mod anim_tests {
    use super::*;

    #[test]
    fn updates_defer_until_bootstraps_and_injections_settle() {
        // #53: an update arriving between pane creation and Ev::Done must
        // wait; it applies once the launch dispatched (or on cap expiry).
        let short = Duration::from_secs(1);
        assert!(!update_may_apply(true, 0, short), "mid-bootstrap defers");
        assert!(
            !update_may_apply(false, 1, short),
            "queued injection defers"
        );
        assert!(!update_may_apply(true, 2, short));
        assert!(update_may_apply(false, 0, short), "safe boundary applies");
        assert!(
            update_may_apply(true, 1, UPDATE_DEFER_CAP),
            "cap breaks a wedge"
        );
    }

    #[test]
    fn anim_clock_survives_unrelated_wakeups() {
        // #17: wakeups inside the interval must not postpone the tick.
        let interval = Duration::from_millis(120);
        let t0 = Instant::now();
        let mut clock = AnimClock::default();
        let armed = clock.deadline(t0, interval);
        // Ten unrelated event-loop passes, each "30ms later": the pinned
        // deadline never moves.
        for i in 1..=10 {
            let now = t0 + Duration::from_millis(30 * i);
            assert_eq!(
                clock.deadline(now, interval),
                armed,
                "wakeup {i} moved the deadline"
            );
        }
        // Not due before the pin…
        assert!(!clock.fire_if_due(t0 + Duration::from_millis(119), interval));
        // …fires at the pin, and re-arms one interval from the fire time.
        assert!(clock.fire_if_due(t0 + interval, interval));
        assert_eq!(
            clock.deadline(t0 + interval, interval),
            t0 + interval + interval
        );
        // Disarm forgets the schedule.
        clock.disarm();
        assert!(
            clock.fire_if_due(t0 + interval, interval),
            "unarmed clock fires immediately"
        );
    }
}

/// Opt-in OSC palette provenance (#75): `DMUX_TRACE_PALETTE=1` appends one
/// decoded line per pane-local palette mutation to
/// `~/.dmux/logs/palette-trace.log` — metadata only, never terminal content.
pub(crate) fn trace_palette_enabled() -> bool {
    std::env::var("DMUX_TRACE_PALETTE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// One trace record: sequence, timestamp, pane identity, slot kind, and the
/// transition (set with rgb, or reset). Slots follow alacritty's layout:
/// 0..=255 indexed, 256 default foreground, 257 default background.
pub(crate) fn palette_trace_record(
    seq: u64,
    when: &str,
    pane: dmux_cc::PaneId,
    slug: &str,
    slot: usize,
    to: Option<(u8, u8, u8)>,
) -> String {
    let kind = match slot {
        256 => "fg".to_string(),
        257 => "bg".to_string(),
        i => format!("idx {i}"),
    };
    let action = match to {
        Some((r, g, b)) => format!("set #{r:02x}{g:02x}{b:02x}"),
        None => "reset".to_string(),
    };
    format!("{seq} {when} pane={pane} slug={slug} {kind} {action}")
}

/// Append a palette mutation to the trace sink with pane attribution.
pub(crate) fn trace_palette_line(
    home: &std::path::Path,
    pane: dmux_cc::PaneId,
    slug: &str,
    slot: usize,
    to: Option<(u8, u8, u8)>,
) {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let line = palette_trace_record(seq, &iso_now(), pane, slug, slot, to);
    let dir = home.join(".dmux").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("palette-trace.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod palette_trace_tests {
    use super::*;

    #[test]
    fn records_distinguish_fg_bg_indexed_and_transitions() {
        // #75: fg/bg/indexed slots and set/reset transitions are explicit,
        // with pane identity and ordering attached.
        let r = palette_trace_record(
            0,
            "t0",
            dmux_cc::PaneId(7),
            "codex-1",
            257,
            Some((0x12, 0x0f, 0x1a)),
        );
        assert_eq!(r, "0 t0 pane=%7 slug=codex-1 bg set #120f1a");
        let r = palette_trace_record(1, "t1", dmux_cc::PaneId(7), "codex-1", 256, None);
        assert_eq!(r, "1 t1 pane=%7 slug=codex-1 fg reset");
        let r = palette_trace_record(
            2,
            "t2",
            dmux_cc::PaneId(9),
            "other",
            4,
            Some((0xff, 0x00, 0xaa)),
        );
        assert_eq!(r, "2 t2 pane=%9 slug=other idx 4 set #ff00aa");
    }
}

use std::path::PathBuf;

pub(crate) fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Loose semver comparison: a > b?
pub(crate) fn is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-'])
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

pub(crate) fn slugify(prompt: &str) -> String {
    let mut slug = String::new();
    for word in prompt.split_whitespace().take(4) {
        let clean: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        if clean.is_empty() {
            continue;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&clean);
        if slug.len() >= 24 {
            break;
        }
    }
    slug.truncate(32);
    if slug.is_empty() {
        format!("agents-{}", timestamp() % 100_000)
    } else {
        slug
    }
}

/// React to a pane emulator side effect. Returns clipboard text to forward
/// (handled by the caller once the pane borrow ends).
/// Trim leading spinner/status glyphs from a pane-reported title. Agents
/// animate these in their OSC/ESC-k titles; dmux renders its own status
/// glyph, so keeping the app's copy showed two spinners per sidebar row
/// (#9). Strips the known spinner families plus separators, never the name.
pub(crate) fn strip_status_glyphs(title: &str) -> &str {
    title.trim_start_matches(|c: char| {
        matches!(c,
            // Claude/Codex asterisk-family frames.
            '✳' | '✻' | '✽' | '✶' | '✢' | '✣' | '✤' | '✥' | '✦' | '✧' | '∗' | '*' | '·' |
            // Circle/clock spinner families and status dots.
            '◐' | '◓' | '◑' | '◒' | '◴' | '◷' | '◶' | '◵' | '◜' | '◝' | '◞' | '◟' |
            '⏺' | '●' | '○' | '◌' | '◍' | '◉' | '⊙' |
            // dmux's own status glyphs, echoed back by some shells.
            '△' | '✗' |
            // Variation selectors that ride along with emoji forms.
            '\u{fe0e}' | '\u{fe0f}'
        ) || ('\u{2800}'..='\u{28ff}').contains(&c) // braille spinners
            || c.is_whitespace()
    })
}
