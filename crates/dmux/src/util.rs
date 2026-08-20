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
