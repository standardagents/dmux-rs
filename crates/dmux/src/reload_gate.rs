//! Controller-owned prototype reload safety. Local input records activity at
//! receipt time; candidate evaluation combines that quiet period with live
//! interaction state supplied by the application.

use std::time::{Duration, Instant};

use crate::util::UPDATE_DEFER_CAP;

pub(crate) const INPUT_QUIET: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocker {
    RecentInput,
    ControllerOwnership,
    ActiveInteraction,
    PendingPaneInput,
    TextEntry,
    Bootstrap,
    PromptInjection,
}

impl Blocker {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::RecentInput => "prototype waiting for input to settle…",
            Self::ControllerOwnership => "prototype waiting for controller ownership…",
            Self::ActiveInteraction => "prototype waiting for the active interaction…",
            Self::PendingPaneInput => "prototype waiting for pane input delivery…",
            Self::TextEntry => "prototype waiting for text entry to close…",
            Self::Bootstrap => "prototype waiting for pane setup…",
            Self::PromptInjection => "prototype waiting for prompt delivery…",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ReloadFacts {
    pub(crate) controller_ready: bool,
    pub(crate) interaction_active: bool,
    pub(crate) pane_input_pending: bool,
    pub(crate) text_entry_open: bool,
    pub(crate) bootstrap_active: bool,
    pub(crate) prompt_injections: usize,
    pub(crate) candidate_wait: Duration,
}

pub(crate) struct ReloadGate {
    last_user_activity: Instant,
}

impl ReloadGate {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_user_activity: now.checked_sub(INPUT_QUIET).unwrap_or(now),
        }
    }

    /// Record controller-accepted input using controller receipt time. A
    /// future authenticated remote-control path can call this same boundary.
    pub(crate) fn record_control_activity(&mut self, received_at: Instant) {
        self.last_user_activity = self.last_user_activity.max(received_at);
    }

    pub(crate) fn quiet_deadline(&self) -> Instant {
        self.last_user_activity + INPUT_QUIET
    }

    pub(crate) fn blocker(&self, now: Instant, facts: ReloadFacts) -> Option<Blocker> {
        if now < self.quiet_deadline() {
            return Some(Blocker::RecentInput);
        }
        if !facts.controller_ready {
            return Some(Blocker::ControllerOwnership);
        }
        if facts.interaction_active {
            return Some(Blocker::ActiveInteraction);
        }
        if facts.pane_input_pending {
            return Some(Blocker::PendingPaneInput);
        }
        if facts.text_entry_open {
            return Some(Blocker::TextEntry);
        }
        if facts.candidate_wait < UPDATE_DEFER_CAP {
            if facts.bootstrap_active {
                return Some(Blocker::Bootstrap);
            }
            if facts.prompt_injections > 0 {
                return Some(Blocker::PromptInjection);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ReloadFacts {
        ReloadFacts {
            controller_ready: true,
            interaction_active: false,
            pane_input_pending: false,
            text_entry_open: false,
            bootstrap_active: false,
            prompt_injections: 0,
            candidate_wait: Duration::ZERO,
        }
    }

    #[test]
    fn input_on_the_deadline_starts_a_new_quiet_period() {
        let start = Instant::now();
        let mut gate = ReloadGate::new(start);
        gate.record_control_activity(start);
        let deadline = start + INPUT_QUIET;
        assert_eq!(gate.blocker(deadline, facts()), None);
        gate.record_control_activity(deadline);
        assert_eq!(gate.blocker(deadline, facts()), Some(Blocker::RecentInput));
        assert_eq!(gate.blocker(deadline + INPUT_QUIET, facts()), None);
    }

    #[test]
    fn operational_cap_never_overrides_user_or_unsaved_input() {
        let start = Instant::now();
        let gate = ReloadGate::new(start);
        let mut current = facts();
        current.candidate_wait = UPDATE_DEFER_CAP;
        current.interaction_active = true;
        current.pane_input_pending = true;
        current.text_entry_open = true;
        assert_eq!(
            gate.blocker(start + INPUT_QUIET, current),
            Some(Blocker::ActiveInteraction)
        );
    }

    #[test]
    fn operational_cap_only_releases_launch_blockers() {
        let start = Instant::now();
        let gate = ReloadGate::new(start);
        let mut current = facts();
        current.bootstrap_active = true;
        current.prompt_injections = 1;
        assert_eq!(
            gate.blocker(start + INPUT_QUIET, current),
            Some(Blocker::Bootstrap)
        );
        current.candidate_wait = UPDATE_DEFER_CAP;
        assert_eq!(gate.blocker(start + INPUT_QUIET, current), None);
    }
}
