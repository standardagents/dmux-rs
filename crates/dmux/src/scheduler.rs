//! Frame pacing with a stable 60 Hz cadence and bounded catch-up behavior.

use std::time::{Duration, Instant};

const FRAME_PERIOD: Duration = Duration::from_nanos(16_666_667);

pub(crate) struct FrameClock {
    next: Instant,
}

impl FrameClock {
    pub(crate) fn new(now: Instant) -> Self {
        Self { next: now }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.next
    }

    pub(crate) fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    pub(crate) fn should_drain(&self, dirty: bool, now: Instant) -> bool {
        !dirty || !self.due(now)
    }

    /// Advance on the stable cadence when the frame finishes on time. A late
    /// frame drops every elapsed slot and resumes one period after completion.
    pub(crate) fn rendered(&mut self, started: Instant, completed: Instant) {
        // An idle app can remain past its old deadline indefinitely. Anchor a
        // newly active cadence at its first frame instead of treating idle
        // time as frame debt.
        let cadence = if started > self.next + FRAME_PERIOD {
            started
        } else {
            self.next
        };
        let scheduled = cadence + FRAME_PERIOD;
        if scheduled > completed {
            self.next = scheduled;
            return;
        }

        self.next = completed + FRAME_PERIOD;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_starts_due_and_keeps_start_to_start_cadence() {
        let start = Instant::now();
        let mut clock = FrameClock::new(start);
        assert!(clock.due(start));

        clock.rendered(start, start + Duration::from_millis(2));
        assert_eq!(clock.deadline(), start + FRAME_PERIOD);
        assert!(!clock.due(start + Duration::from_millis(16)));
        assert!(clock.due(start + FRAME_PERIOD));
    }

    #[test]
    fn late_frame_skips_slots_without_catch_up() {
        let start = Instant::now();
        let completed = start + Duration::from_millis(50);
        let mut clock = FrameClock::new(start);

        clock.rendered(start, completed);
        assert_eq!(clock.deadline(), completed + FRAME_PERIOD);
        assert!(!clock.due(completed));
    }

    #[test]
    fn first_frame_after_idle_starts_a_fresh_cadence() {
        let start = Instant::now();
        let resumed = start + Duration::from_secs(10);
        let completed = resumed + Duration::from_millis(1);
        let mut clock = FrameClock::new(start);

        clock.rendered(resumed, completed);
        assert_eq!(clock.deadline(), resumed + FRAME_PERIOD);
    }

    #[test]
    fn dirty_event_drain_stops_at_the_frame_deadline() {
        let start = Instant::now();
        let clock = FrameClock::new(start);

        assert!(!clock.should_drain(true, start));
        assert!(clock.should_drain(false, start));
    }
}
