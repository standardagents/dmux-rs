//! Frame pacing with a stable 60 Hz cadence and bounded catch-up behavior.

use std::time::{Duration, Instant};

const FRAME_PERIOD: Duration = Duration::from_nanos(16_666_667);
const INTERACTION_PERIOD: Duration = Duration::from_nanos(8_333_333);
const KEY_RESPONSE_PERIOD: Duration = Duration::from_nanos(4_166_667);

pub(crate) struct FrameClock {
    regular_next: Instant,
    interactive_next: Option<Instant>,
    last_completed: Option<Instant>,
}

impl FrameClock {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            regular_next: now,
            interactive_next: None,
            last_completed: None,
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.interactive_next
            .map(|interactive| interactive.min(self.regular_next))
            .unwrap_or(self.regular_next)
    }

    pub(crate) fn due(&self, now: Instant) -> bool {
        now >= self.deadline()
    }

    pub(crate) fn should_drain(
        &self,
        dirty: bool,
        scroll_deadline: Option<Instant>,
        now: Instant,
    ) -> bool {
        scroll_deadline.is_none_or(|deadline| now < deadline) && (!dirty || !self.due(now))
    }

    /// Pull the next frame forward for direct manipulation or the first pane
    /// response after input. Repeated requests are bounded at 120 Hz.
    pub(crate) fn request_interactive(&mut self, now: Instant) {
        let earliest = self
            .last_completed
            .map(|completed| completed + INTERACTION_PERIOD)
            .unwrap_or(now);
        let requested = now.max(earliest);
        self.interactive_next = Some(
            self.interactive_next
                .map(|pending| pending.min(requested))
                .unwrap_or(requested),
        );
    }

    /// Acknowledged keyboard input uses a tighter, bounded response cadence.
    pub(crate) fn request_key_response(&mut self, now: Instant) {
        let earliest = self
            .last_completed
            .map(|completed| completed + KEY_RESPONSE_PERIOD)
            .unwrap_or(now);
        let requested = now.max(earliest);
        self.interactive_next = Some(
            self.interactive_next
                .map(|pending| pending.min(requested))
                .unwrap_or(requested),
        );
    }

    /// Advance on the stable cadence when the frame finishes on time. A late
    /// frame drops every elapsed slot and resumes one period after completion.
    pub(crate) fn rendered(&mut self, started: Instant, completed: Instant) {
        // An idle app can remain past its old deadline indefinitely. Anchor a
        // newly active cadence at its first frame instead of treating idle
        // time as frame debt.
        self.interactive_next = None;
        if started >= self.regular_next || completed >= self.regular_next {
            let cadence = if started > self.regular_next + FRAME_PERIOD {
                started
            } else {
                self.regular_next
            };
            let scheduled = cadence + FRAME_PERIOD;
            self.regular_next = if scheduled > completed {
                scheduled
            } else {
                completed + FRAME_PERIOD
            };
        }
        self.last_completed = Some(completed);
    }
}

impl crate::App {
    pub(super) fn render_if_due(&mut self) {
        if !self.renderer.is_ready() {
            return;
        }
        if self.dirty && self.frame_clock.due(Instant::now()) {
            self.render_frame();
        } else if self.dirty {
            self.metrics.coalesced += 1;
        }
    }

    pub(super) fn handle_deadlines_if_due(&mut self, deadline: Option<tokio::time::Instant>) {
        if deadline.is_some_and(|due| tokio::time::Instant::now() >= due) {
            self.handle_deadlines();
        }
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

        assert!(!clock.should_drain(true, None, start));
        assert!(clock.should_drain(false, None, start));
    }

    #[test]
    fn pending_scroll_deadline_stops_output_drain_even_when_clean() {
        let start = Instant::now();
        let clock = FrameClock::new(start);
        let scroll = start + Duration::from_millis(2);

        assert!(clock.should_drain(false, Some(scroll), start));
        assert!(!clock.should_drain(false, Some(scroll), scroll));
    }

    #[test]
    fn interaction_pulls_a_frame_forward_with_120hz_spacing() {
        let start = Instant::now();
        let mut clock = FrameClock::new(start);
        clock.rendered(start, start + Duration::from_millis(1));
        assert_eq!(clock.deadline(), start + FRAME_PERIOD);

        clock.request_interactive(start + Duration::from_millis(2));
        assert_eq!(
            clock.deadline(),
            start + Duration::from_millis(1) + INTERACTION_PERIOD
        );
        let interactive = clock.deadline();
        clock.rendered(interactive, interactive + Duration::from_millis(1));
        assert_eq!(clock.deadline(), start + FRAME_PERIOD);
    }

    #[test]
    fn repeated_interaction_requests_do_not_create_an_unbounded_burst() {
        let start = Instant::now();
        let mut clock = FrameClock::new(start);
        clock.rendered(start, start + Duration::from_millis(1));

        clock.request_interactive(start + Duration::from_millis(2));
        let requested = clock.deadline();
        clock.request_interactive(start + Duration::from_millis(3));
        clock.request_interactive(start + Duration::from_millis(4));

        assert_eq!(clock.deadline(), requested);
        assert!(!clock.due(requested - Duration::from_nanos(1)));
        assert!(clock.due(requested));
    }

    #[test]
    fn acknowledged_key_response_bypasses_interaction_spacing() {
        let start = Instant::now();
        let mut clock = FrameClock::new(start);
        clock.rendered(start, start + Duration::from_millis(1));
        let response = start + Duration::from_millis(2);

        clock.request_key_response(response);

        let key_deadline = start + Duration::from_millis(1) + KEY_RESPONSE_PERIOD;
        assert_eq!(clock.deadline(), key_deadline);
        assert!(key_deadline < start + Duration::from_millis(1) + INTERACTION_PERIOD);
        assert!(!clock.due(key_deadline - Duration::from_nanos(1)));
        assert!(clock.due(key_deadline));

        clock.request_key_response(start + Duration::from_millis(3));
        assert_eq!(clock.deadline(), key_deadline);
        clock.rendered(key_deadline, key_deadline + Duration::from_millis(1));
        assert_eq!(clock.deadline(), start + FRAME_PERIOD);
    }

    #[test]
    fn ordinary_frames_keep_the_60hz_cadence_without_interaction() {
        let start = Instant::now();
        let mut clock = FrameClock::new(start);
        clock.rendered(start, start + Duration::from_millis(1));
        let second = clock.deadline();
        clock.rendered(second, second + Duration::from_millis(1));

        assert_eq!(clock.deadline(), start + FRAME_PERIOD * 2);
    }
}
