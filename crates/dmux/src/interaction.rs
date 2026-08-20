//! Interaction batching and server-side latency correlation.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use dmux_cc::PaneId;
use dmux_host::{InputEvent, MouseButtons, TimedInputEvent};
use tokio::sync::mpsc;

use crate::input::{MouseButtonState, MouseKind};
use crate::{input, renderer_control, session, App};

const MAX_INPUT_BATCH: usize = 64;
const MAX_PENDING_AGE: Duration = Duration::from_secs(2);
const KEY_RESPONSE_WINDOW: Duration = Duration::from_millis(50);
const SCROLL_PERIOD: Duration = Duration::from_nanos(8_333_333);
const MAX_PENDING_SCROLL: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Key,
    Pointer,
    Scroll,
}

impl Kind {
    pub(crate) const ALL: [Self; 3] = [Self::Key, Self::Pointer, Self::Scroll];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Key => 0,
            Self::Pointer => 1,
            Self::Scroll => 2,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Pointer => "pointer",
            Self::Scroll => "scroll",
        }
    }
}

#[derive(Debug)]
pub(crate) struct InputBatch {
    pub(crate) events: Vec<TimedInputEvent>,
    pub(crate) coalesced_motion: u64,
}

/// Drain the input queue once and retain only the newest position from each
/// run of buttonless pointer motion. Button transitions, drag paths, wheel
/// ticks, keys, and paste remain ordered barriers.
pub(crate) fn drain_input_batch(
    first: TimedInputEvent,
    rx: &mut mpsc::Receiver<TimedInputEvent>,
    button_state: MouseButtonState,
) -> InputBatch {
    let mut raw = Vec::with_capacity(MAX_INPUT_BATCH);
    raw.push(first);
    while raw.len() < MAX_INPUT_BATCH {
        match rx.try_recv() {
            Ok(event) => raw.push(event),
            Err(_) => break,
        }
    }
    coalesce_motion(raw, button_state)
}

fn coalesce_motion(events: Vec<TimedInputEvent>, mut button_state: MouseButtonState) -> InputBatch {
    let mut kept = Vec::with_capacity(events.len());
    let mut pending_motion: Option<TimedInputEvent> = None;
    let mut coalesced_motion = 0;

    for event in events {
        let mouse_kind = match &event.event {
            InputEvent::Mouse(mouse) => {
                Some(input::classify_mouse(mouse, button_state.any_down()).2)
            }
            _ => None,
        };
        let pure_motion = mouse_kind == Some(MouseKind::Hover);
        if pure_motion {
            if pending_motion.replace(event).is_some() {
                coalesced_motion += 1;
            }
            continue;
        }

        if let Some(motion) = pending_motion.take() {
            kept.push(motion);
        }
        if let Some(kind) = mouse_kind {
            button_state.update(kind);
        }
        kept.push(event);
    }
    if let Some(motion) = pending_motion {
        kept.push(motion);
    }

    InputBatch {
        events: kept,
        coalesced_motion,
    }
}

pub(crate) fn kind(event: &InputEvent) -> Option<Kind> {
    match event {
        InputEvent::Key(_) | InputEvent::Paste(_) => Some(Kind::Key),
        InputEvent::Mouse(mouse) if mouse.mouse_buttons.contains(MouseButtons::VERT_WHEEL) => {
            Some(Kind::Scroll)
        }
        InputEvent::Mouse(_) => Some(Kind::Pointer),
        _ => None,
    }
}

/// Replays a short trackpad backlog at one wheel tick per interaction frame.
/// This preserves momentum without turning an SSH input burst into a multi-row
/// jump. New overflow replaces stale motion so the tail remains bounded.
#[derive(Default)]
pub(crate) struct ScrollPacer {
    pending: VecDeque<TimedInputEvent>,
    next_at: Option<Instant>,
}

impl ScrollPacer {
    fn submit(&mut self, event: TimedInputEvent, now: Instant) -> Option<TimedInputEvent> {
        debug_assert_eq!(kind(&event.event), Some(Kind::Scroll));
        if self.pending.is_empty() && self.next_at.is_none_or(|next| now >= next) {
            self.next_at = Some(now + SCROLL_PERIOD);
            return Some(event);
        }

        if self
            .pending
            .back()
            .is_some_and(|previous| wheels_cancel(previous, &event))
        {
            self.pending.pop_back();
            return None;
        }
        if self.pending.len() == MAX_PENDING_SCROLL {
            self.pending.pop_front();
        }
        self.pending.push_back(event);
        None
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        if self.pending.is_empty() {
            None
        } else {
            self.next_at
        }
    }

    fn take_due(&mut self, now: Instant) -> Option<TimedInputEvent> {
        let next = self.deadline()?;
        if now < next {
            return None;
        }
        let event = self.pending.pop_front();
        self.next_at = Some(now + SCROLL_PERIOD);
        event
    }

    fn cancel(&mut self) {
        self.pending.clear();
        self.next_at = None;
    }
}

fn wheel_signature(event: &TimedInputEvent) -> Option<(u16, u16, bool)> {
    let InputEvent::Mouse(mouse) = &event.event else {
        return None;
    };
    mouse
        .mouse_buttons
        .contains(MouseButtons::VERT_WHEEL)
        .then(|| {
            (
                mouse.x,
                mouse.y,
                mouse.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE),
            )
        })
}

fn wheels_cancel(left: &TimedInputEvent, right: &TimedInputEvent) -> bool {
    matches!(
        (wheel_signature(left), wheel_signature(right)),
        (Some((lx, ly, lup)), Some((rx, ry, rup)))
            if lx == rx && ly == ry && lup != rup
    )
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    sequence: u64,
    kind: Kind,
    received_at: Instant,
    pane: Option<PaneId>,
    acknowledged_at: Option<Instant>,
    local_change: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Observation {
    pub(crate) kind: Kind,
    pub(crate) elapsed: Duration,
}

pub(crate) struct PaneOutput {
    observations: Vec<Observation>,
    key_response: bool,
}

/// Correlates an input event with local damage or the first later output from
/// the pane it was forwarded to. The pane correlation is intentionally a
/// response-burst measurement because arbitrary terminal output has no key ID.
pub(crate) struct Tracker {
    next_sequence: u64,
    current: Option<Sample>,
    pending_pane: VecDeque<Sample>,
    ready: VecDeque<Sample>,
    latest_pane_input: HashMap<PaneId, (u64, Kind)>,
    key_windows: HashMap<PaneId, (u64, Instant)>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            next_sequence: 1,
            current: None,
            pending_pane: VecDeque::new(),
            ready: VecDeque::new(),
            latest_pane_input: HashMap::new(),
            key_windows: HashMap::new(),
        }
    }
}

impl Tracker {
    pub(crate) fn begin(&mut self, event: &TimedInputEvent) -> Option<Observation> {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        self.current = kind(&event.event).map(|kind| Sample {
            sequence,
            kind,
            received_at: event.received_at,
            pane: None,
            acknowledged_at: (kind != Kind::Key).then_some(event.received_at),
            local_change: false,
        });
        self.current.map(|sample| Observation {
            kind: sample.kind,
            elapsed: Instant::now().saturating_duration_since(sample.received_at),
        })
    }

    pub(crate) fn forwarded_to(&mut self, pane: PaneId) {
        if let Some(current) = &mut self.current {
            current.pane = Some(pane);
            self.latest_pane_input
                .insert(pane, (current.sequence, current.kind));
            if current.kind != Kind::Key {
                self.key_windows.remove(&pane);
            }
        }
    }

    pub(crate) fn current_key_sequence(&self) -> Option<u64> {
        self.current
            .filter(|sample| sample.kind == Kind::Key)
            .map(|sample| sample.sequence)
    }

    pub(crate) fn pane_input_ack(
        &mut self,
        pane: PaneId,
        sequence: u64,
        ok: bool,
        at: Instant,
    ) -> bool {
        if !ok {
            return false;
        }
        let Some(sample) = self
            .pending_pane
            .iter_mut()
            .find(|sample| sample.pane == Some(pane) && sample.sequence == sequence)
        else {
            return false;
        };
        sample.acknowledged_at = Some(at);
        if self.latest_pane_input.get(&pane) == Some(&(sequence, Kind::Key)) {
            self.key_windows
                .insert(pane, (sequence, at + KEY_RESPONSE_WINDOW));
            return true;
        }
        false
    }

    pub(crate) fn local_changed(&mut self) {
        if let Some(current) = &mut self.current {
            current.local_change = true;
        }
    }

    /// Finish the current handler. True requests a latency-sensitive frame.
    pub(crate) fn finish(&mut self) -> bool {
        let Some(sample) = self.current.take() else {
            return false;
        };
        if sample.pane.is_some() {
            self.retain_fresh(Instant::now());
            if sample.kind != Kind::Key {
                if let Some(existing) = self
                    .pending_pane
                    .iter_mut()
                    .find(|pending| pending.pane == sample.pane && pending.kind == sample.kind)
                {
                    if sample.received_at < existing.received_at {
                        existing.received_at = sample.received_at;
                    }
                    return false;
                }
            }
            self.pending_pane.push_back(sample);
            return false;
        }
        if sample.local_change {
            self.push_ready(sample);
            return true;
        }
        false
    }

    /// Mark the first pane-output burst after forwarded input as interactive.
    pub(crate) fn pane_output(&mut self, pane: PaneId, at: Instant) -> PaneOutput {
        self.retain_fresh(at);
        self.key_windows.retain(|_, (_, until)| at <= *until);
        let key_response = self.key_windows.contains_key(&pane);
        let mut observed = Vec::new();
        let mut retained = VecDeque::with_capacity(self.pending_pane.len());
        while let Some(sample) = self.pending_pane.pop_front() {
            if sample.pane == Some(pane) && sample.acknowledged_at.is_some() {
                observed.push(Observation {
                    kind: sample.kind,
                    elapsed: at.saturating_duration_since(sample.received_at),
                });
                self.push_ready(sample);
            } else {
                retained.push_back(sample);
            }
        }
        self.pending_pane = retained;
        PaneOutput {
            observations: observed,
            key_response,
        }
    }

    pub(crate) fn frame_written(&mut self, at: Instant) -> Vec<Observation> {
        let mut observations = Vec::with_capacity(self.ready.len());
        while let Some(sample) = self.ready.pop_front() {
            observations.push(Observation {
                kind: sample.kind,
                elapsed: at.saturating_duration_since(sample.received_at),
            });
        }
        observations
    }

    fn push_ready(&mut self, sample: Sample) {
        if let Some(existing) = self
            .ready
            .iter_mut()
            .find(|ready| ready.kind == sample.kind)
        {
            if sample.received_at < existing.received_at {
                existing.received_at = sample.received_at;
            }
        } else {
            self.ready.push_back(sample);
        }
    }

    fn retain_fresh(&mut self, now: Instant) {
        self.pending_pane
            .retain(|sample| now.saturating_duration_since(sample.received_at) <= MAX_PENDING_AGE);
    }
}

impl App {
    pub(super) fn handle_pane_interaction_output(&mut self, pane: PaneId, at: Instant) {
        let output = self.interactions.pane_output(pane, at);
        let interaction_response = !output.observations.is_empty();
        for observation in output.observations {
            self.metrics.record_pane_output(observation);
        }
        if output.key_response {
            self.frame_clock.request_key_response(at);
        } else if interaction_response {
            self.frame_clock.request_interactive(at);
        }
    }

    pub(super) fn handle_pane_input_ack(
        &mut self,
        pane: PaneId,
        sequence: u64,
        ok: bool,
        at: Instant,
    ) {
        if self.interactions.pane_input_ack(pane, sequence, ok, at) && self.dirty {
            self.frame_clock.request_key_response(at);
        }
    }

    pub(super) fn handle_or_queue_timed_input(&mut self, timed: TimedInputEvent) -> bool {
        if kind(&timed.event) == Some(Kind::Scroll) {
            let Some(timed) = self.scroll_pacer.submit(timed, Instant::now()) else {
                return true;
            };
            return self.handle_timed_input_now(timed);
        }

        if interrupts_scroll(&timed.event, self.mouse_buttons) {
            self.scroll_pacer.cancel();
        }
        self.handle_timed_input_now(timed)
    }

    pub(super) fn handle_due_scroll(&mut self, now: Instant) -> bool {
        let Some(timed) = self.scroll_pacer.take_due(now) else {
            return true;
        };
        if !self.handle_timed_input_now(timed) {
            return false;
        }
        true
    }

    pub(super) fn handle_interaction_resize(&mut self, size: (u16, u16)) {
        self.scroll_pacer.cancel();
        self.handle_resize(size);
    }

    /// Handle one timestamped host event and schedule direct manipulation at
    /// the lower-latency interaction cadence.
    pub(super) fn handle_timed_input_now(&mut self, timed: TimedInputEvent) -> bool {
        if matches!(
            self.renderer.state,
            renderer_control::State::Startup | renderer_control::State::Claiming
        ) {
            self.pending_owner_input.push_back(timed);
            return true;
        }
        if !self.renderer.is_controller()
            && renderer_control::claim_worthy(&timed.event, &self.mouse_buttons)
        {
            self.pending_owner_input.push_back(timed);
            self.request_renderer_claim(renderer_control::ClaimReason::Activity, None);
            return true;
        }
        if let Some(queue) = self.interactions.begin(&timed) {
            self.metrics.record_input_queue(queue.kind, queue.elapsed);
        }
        let was_dirty = self.dirty;
        let keep_running = self.handle_input(timed.event);
        if !was_dirty && self.dirty {
            self.interactions.local_changed();
        }
        if self.interactions.finish() {
            self.frame_clock.request_interactive(Instant::now());
        }
        keep_running
    }

    /// Returns false to quit.
    fn handle_input(&mut self, event: InputEvent) -> bool {
        match event {
            InputEvent::Key(key) => {
                if self.hovered.take().is_some() {
                    self.dirty = true;
                }
                if let Some(top) = self.views.last_mut() {
                    let result = top.on_key(&key);
                    self.dirty = true;
                    return self.apply_view_result(result);
                }
                let leader_was_armed = self.leader_armed;
                if leader_was_armed {
                    self.leader_armed = false;
                    self.dirty = true;
                }
                if !leader_was_armed && self.welcome_active() {
                    if let Some(handled) = self.handle_welcome_key(&key) {
                        return handled;
                    }
                }
                if !leader_was_armed && self.sidebar_focused {
                    if let Some(handled) = self.handle_sidebar_key(&key) {
                        return handled;
                    }
                }
                let modes = session::pane_input_modes(&self.panes, self.focused);
                let routed = input::route_key(&key, modes, leader_was_armed, &self.keymap);
                self.execute_routed(routed)
            }
            InputEvent::Mouse(mouse) => {
                let (col, row, kind, shift) =
                    input::classify_mouse(&mouse, self.mouse_buttons.any_down());
                self.handle_mouse(col, row, kind, shift)
            }
            InputEvent::Paste(text) => {
                if self.hovered.take().is_some() {
                    self.dirty = true;
                }
                if let Some(top) = self.views.last_mut() {
                    let result = top.on_paste(&text);
                    self.dirty = true;
                    return self.apply_view_result(result);
                }
                let modes = session::pane_input_modes(&self.panes, self.focused);
                self.send_pane_bytes(&input::encode_paste(&text, modes));
                true
            }
            InputEvent::Resized { cols, rows } => {
                self.handle_resize((cols as u16, rows as u16));
                true
            }
            _ => true,
        }
    }

    pub(super) fn send_pane_bytes(&mut self, bytes: &[u8]) {
        let Some(p) = self.panes.get_mut(self.focused) else {
            return;
        };
        if p.status == crate::PaneStatus::Dead || p.hidden {
            return;
        }
        if p.term.selection_clear() {
            p.dirty = true;
            self.dirty = true;
        }
        if p.term.display_offset() > 0 {
            p.term.scroll_to_bottom();
            p.dirty = true;
            self.dirty = true;
        }
        let pane = p.tmux_pane;
        self.interactions.forwarded_to(pane);
        let key_sequence = self.interactions.current_key_sequence();
        let mut chunks = bytes.chunks(256).peekable();
        while let Some(chunk) = chunks.next() {
            let command = input::send_keys_hex(pane, chunk);
            let result = match (key_sequence, chunks.peek().is_none()) {
                (Some(sequence), true) => {
                    self.send_shared_tagged(command, crate::Tag::Input(pane, sequence))
                }
                _ => self.send_shared(command),
            };
            if result.is_err() {
                break;
            }
        }
    }
}

fn interrupts_scroll(event: &InputEvent, buttons: MouseButtonState) -> bool {
    match event {
        InputEvent::Key(_) | InputEvent::Paste(_) | InputEvent::Resized { .. } => true,
        InputEvent::Mouse(mouse) => !matches!(
            input::classify_mouse(mouse, buttons.any_down()).2,
            MouseKind::Hover | MouseKind::WheelUp | MouseKind::WheelDown
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::{KeyCode, KeyEvent, Modifiers, MouseEvent};

    fn mouse(x: u16, buttons: MouseButtons, at: Instant) -> TimedInputEvent {
        TimedInputEvent {
            event: InputEvent::Mouse(MouseEvent {
                x,
                y: 3,
                mouse_buttons: buttons,
                modifiers: Modifiers::NONE,
            }),
            received_at: at,
        }
    }

    fn key(at: Instant) -> TimedInputEvent {
        TimedInputEvent {
            event: InputEvent::Key(KeyEvent {
                key: KeyCode::Char('x'),
                modifiers: Modifiers::NONE,
            }),
            received_at: at,
        }
    }

    fn mouse_x(event: &TimedInputEvent) -> Option<u16> {
        match &event.event {
            InputEvent::Mouse(mouse) => Some(mouse.x),
            _ => None,
        }
    }

    #[test]
    fn hover_burst_keeps_only_the_newest_position() {
        let at = Instant::now();
        let batch = coalesce_motion(
            vec![
                mouse(1, MouseButtons::NONE, at),
                mouse(2, MouseButtons::NONE, at),
                mouse(3, MouseButtons::NONE, at),
            ],
            MouseButtonState::default(),
        );

        assert_eq!(batch.events.len(), 1);
        assert_eq!(mouse_x(&batch.events[0]), Some(3));
        assert_eq!(batch.coalesced_motion, 2);
    }

    #[test]
    fn keys_and_wheel_ticks_are_ordered_barriers() {
        let at = Instant::now();
        let wheel = MouseButtons::VERT_WHEEL | MouseButtons::WHEEL_POSITIVE;
        let batch = coalesce_motion(
            vec![
                mouse(1, MouseButtons::NONE, at),
                mouse(2, MouseButtons::NONE, at),
                key(at),
                mouse(3, MouseButtons::NONE, at),
                mouse(4, wheel, at),
                mouse(5, MouseButtons::NONE, at),
            ],
            MouseButtonState::default(),
        );

        assert_eq!(batch.events.len(), 5);
        assert_eq!(mouse_x(&batch.events[0]), Some(2));
        assert!(matches!(batch.events[1].event, InputEvent::Key(_)));
        assert_eq!(mouse_x(&batch.events[2]), Some(3));
        assert_eq!(mouse_x(&batch.events[3]), Some(4));
        assert_eq!(mouse_x(&batch.events[4]), Some(5));
    }

    #[test]
    fn press_drag_release_motion_is_never_coalesced() {
        let at = Instant::now();
        let batch = coalesce_motion(
            vec![
                mouse(1, MouseButtons::LEFT, at),
                mouse(2, MouseButtons::LEFT, at),
                mouse(3, MouseButtons::LEFT, at),
                mouse(4, MouseButtons::NONE, at),
                mouse(5, MouseButtons::NONE, at),
                mouse(6, MouseButtons::NONE, at),
            ],
            MouseButtonState::default(),
        );

        assert_eq!(batch.events.len(), 5);
        assert_eq!(
            batch.events.iter().filter_map(mouse_x).collect::<Vec<_>>(),
            [1, 2, 3, 4, 6]
        );
        assert_eq!(batch.coalesced_motion, 1);
    }

    #[test]
    fn dual_button_releases_remain_ordered_before_hover_motion() {
        let at = Instant::now();
        let batch = coalesce_motion(
            vec![
                mouse(1, MouseButtons::LEFT, at),
                mouse(2, MouseButtons::RIGHT, at),
                mouse(3, MouseButtons::NONE, at),
                mouse(4, MouseButtons::NONE, at),
                mouse(5, MouseButtons::NONE, at),
                mouse(6, MouseButtons::NONE, at),
            ],
            MouseButtonState::default(),
        );

        assert_eq!(
            batch.events.iter().filter_map(mouse_x).collect::<Vec<_>>(),
            [1, 2, 3, 4, 6]
        );
        assert_eq!(batch.coalesced_motion, 1);
    }

    #[test]
    fn pane_response_promotes_the_oldest_burst_to_the_next_frame() {
        let at = Instant::now();
        let mut tracker = Tracker::default();
        let event = key(at);
        tracker.begin(&event);
        tracker.forwarded_to(PaneId(7));
        let sequence = tracker.current_key_sequence().unwrap();
        assert!(!tracker.finish());

        let before_ack = tracker.pane_output(PaneId(7), at + Duration::from_millis(2));
        assert!(before_ack.observations.is_empty());
        assert!(!before_ack.key_response);
        assert!(tracker.pane_input_ack(PaneId(7), sequence, true, at + Duration::from_millis(2)));
        let output = tracker.pane_output(PaneId(7), at + Duration::from_millis(3));
        assert!(output.key_response);
        assert_eq!(output.observations.len(), 1);
        assert_eq!(output.observations[0].kind, Kind::Key);
        assert_eq!(output.observations[0].elapsed, Duration::from_millis(3));

        let followup = tracker.pane_output(PaneId(7), at + Duration::from_millis(10));
        assert!(followup.key_response);
        assert!(followup.observations.is_empty());
        let expired = tracker.pane_output(PaneId(7), at + Duration::from_millis(60));
        assert!(!expired.key_response);

        let frame = tracker.frame_written(at + Duration::from_millis(5));
        assert_eq!(frame.len(), 1);
        assert_eq!(frame[0].elapsed, Duration::from_millis(5));
    }

    #[test]
    fn failed_or_stale_input_ack_does_not_arm_a_key_response() {
        let at = Instant::now();
        let mut tracker = Tracker::default();
        tracker.begin(&key(at));
        tracker.forwarded_to(PaneId(7));
        let sequence = tracker.current_key_sequence().unwrap();
        tracker.finish();

        assert!(!tracker.pane_input_ack(PaneId(7), sequence, false, at));
        assert!(!tracker.pane_input_ack(PaneId(7), sequence + 1, true, at));

        assert!(tracker
            .pane_output(PaneId(7), at + Duration::from_millis(3))
            .observations
            .is_empty());
    }

    #[test]
    fn later_pointer_input_cancels_the_key_response_window() {
        let at = Instant::now();
        let mut tracker = Tracker::default();
        tracker.begin(&key(at));
        tracker.forwarded_to(PaneId(7));
        let sequence = tracker.current_key_sequence().unwrap();
        tracker.finish();
        assert!(tracker.pane_input_ack(PaneId(7), sequence, true, at));

        tracker.begin(&mouse(4, MouseButtons::NONE, at));
        tracker.forwarded_to(PaneId(7));
        tracker.finish();

        let output = tracker.pane_output(PaneId(7), at + Duration::from_millis(2));
        assert!(!output.key_response);
    }

    #[test]
    fn scroll_burst_waits_for_the_next_interaction_period() {
        let at = Instant::now();
        let wheel = MouseButtons::VERT_WHEEL | MouseButtons::WHEEL_POSITIVE;
        let mut pacer = ScrollPacer::default();

        assert!(pacer.submit(mouse(1, wheel.clone(), at), at).is_some());
        assert!(pacer.submit(mouse(1, wheel, at), at).is_none());
        assert_eq!(pacer.deadline(), Some(at + SCROLL_PERIOD));
        assert!(pacer
            .take_due(at + SCROLL_PERIOD - Duration::from_nanos(1))
            .is_none());
        assert!(pacer.take_due(at + SCROLL_PERIOD).is_some());
        assert_eq!(pacer.deadline(), None);
    }

    #[test]
    fn opposite_pending_wheel_ticks_cancel_at_the_same_pointer_location() {
        let at = Instant::now();
        let up = MouseButtons::VERT_WHEEL | MouseButtons::WHEEL_POSITIVE;
        let down = MouseButtons::VERT_WHEEL;
        let mut pacer = ScrollPacer::default();

        assert!(pacer.submit(mouse(4, up.clone(), at), at).is_some());
        assert!(pacer.submit(mouse(4, up, at), at).is_none());
        assert!(pacer.submit(mouse(4, down, at), at).is_none());
        assert_eq!(pacer.deadline(), None);
    }

    #[test]
    fn scroll_queue_retains_the_newest_bounded_motion() {
        let at = Instant::now();
        let wheel = MouseButtons::VERT_WHEEL | MouseButtons::WHEEL_POSITIVE;
        let mut pacer = ScrollPacer::default();
        assert!(pacer.submit(mouse(0, wheel.clone(), at), at).is_some());
        for x in 1..=10 {
            assert!(pacer.submit(mouse(x, wheel.clone(), at), at).is_none());
        }

        let mut emitted = Vec::new();
        let mut due = at + SCROLL_PERIOD;
        while pacer.deadline().is_some() {
            emitted.push(mouse_x(&pacer.take_due(due).unwrap()).unwrap());
            due += SCROLL_PERIOD;
        }
        assert_eq!(emitted, [3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(due, at + SCROLL_PERIOD * 9);
    }

    #[test]
    fn late_scroll_wake_emits_one_tick_without_catching_up() {
        let at = Instant::now();
        let wheel = MouseButtons::VERT_WHEEL | MouseButtons::WHEEL_POSITIVE;
        let mut pacer = ScrollPacer::default();
        assert!(pacer.submit(mouse(0, wheel.clone(), at), at).is_some());
        for x in 1..=3 {
            assert!(pacer.submit(mouse(x, wheel.clone(), at), at).is_none());
        }

        let late = at + Duration::from_millis(100);
        assert_eq!(mouse_x(&pacer.take_due(late).unwrap()), Some(1));
        assert!(pacer.take_due(late).is_none());
        assert_eq!(pacer.deadline(), Some(late + SCROLL_PERIOD));
        assert_eq!(
            mouse_x(&pacer.take_due(late + SCROLL_PERIOD).unwrap()),
            Some(2)
        );
    }

    #[test]
    fn deliberate_input_interrupts_queued_scroll_while_hover_does_not() {
        let at = Instant::now();
        let buttons = MouseButtonState::default();
        assert!(interrupts_scroll(&key(at).event, buttons));
        assert!(!interrupts_scroll(
            &mouse(1, MouseButtons::NONE, at).event,
            buttons
        ));
        assert!(interrupts_scroll(
            &mouse(1, MouseButtons::LEFT, at).event,
            buttons
        ));
        assert!(interrupts_scroll(
            &InputEvent::Resized { cols: 80, rows: 24 },
            buttons
        ));
    }
}
