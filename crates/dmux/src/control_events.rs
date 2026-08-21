//! Ordered tmux control-mode notifications and command replies.

use std::time::Instant;

use dmux_cc::{CcEvent, Reply, Routed as CcRouted};

use crate::{
    audit, handle_side_effect, renderer_control, session, App, AppMsg, PaneStatus, Tag,
    FLOOD_BYTES_PER_WINDOW, FLOOD_RESEED_EVERY, FLOOD_WINDOW,
};

impl App {
    pub(super) fn maybe_finish_renderer_startup(&mut self) {
        if self.renderer.state != renderer_control::State::Startup
            || self.renderer.identity.is_none()
            || !self.startup_legacy_checked
            || !self.startup_panes_ready
        {
            return;
        }
        if self.startup_legacy_alive {
            self.renderer.become_legacy_follower();
            self.toast("Viewing: TypeScript dmux controls this session");
            return;
        }
        if let Some(reexec) = self.renderer.take_reexec_startup() {
            match reexec {
                renderer_control::ReexecStartup::Claim => {
                    self.request_renderer_claim(renderer_control::ClaimReason::Startup, None);
                }
                renderer_control::ReexecStartup::Follow => {
                    self.renderer.become_follower(self.renderer.owner.clone());
                    self.dirty = true;
                }
                renderer_control::ReexecStartup::Recover(expected) => {
                    self.request_renderer_claim(renderer_control::ClaimReason::Recovery, expected);
                }
            }
            return;
        }
        self.request_renderer_claim(renderer_control::ClaimReason::Startup, None);
    }

    pub(super) fn request_renderer_claim(
        &mut self,
        reason: renderer_control::ClaimReason,
        expected: Option<String>,
    ) {
        if !self.renderer.begin_claim(reason, expected) {
            return;
        }
        let Some(path) = self.renderer.lock_path() else {
            self.renderer.become_follower(None);
            return;
        };
        let tx = self.app_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = renderer_control::ClaimLock::acquire(&path).map_err(|err| err.to_string());
            let _ = tx.send(AppMsg::RendererLock(result));
        });
        self.dirty = true;
    }

    fn send_renderer_claim(&mut self, record: renderer_control::OwnerRecord) {
        let encoded = record.encode();
        let _ = self.client.send(format!(
            "set-option -t {} {} {}",
            dmux_cc::quote_arg(&self.session_name),
            renderer_control::OWNER_OPTION,
            dmux_cc::quote_arg(&encoded)
        ));
        let _ = self.client.send(format!(
            "set-option -t {} {} {}",
            dmux_cc::quote_arg(&self.session_name),
            renderer_control::TOKEN_OPTION,
            record.token
        ));

        let (default_fg, default_bg) = dmux_vt::palette::default_fg_bg_hex();
        let shared = [
            format!("set -g window-style 'fg={default_fg},bg={default_bg}'"),
            format!("set -g window-active-style 'fg={default_fg},bg={default_bg}'"),
        ];
        for command in shared
            .iter()
            .map(String::as_str)
            .chain(session::extended_key_commands())
        {
            let _ = self
                .client
                .send_deferred(renderer_control::guarded_command(&record, command));
        }

        let mut per_window: std::collections::HashMap<dmux_cc::WindowId, u32> =
            std::collections::HashMap::new();
        for pane in &self.panes {
            *per_window.entry(pane.tmux_window).or_default() += 1;
        }
        for pane in &self.panes {
            if per_window.get(&pane.tmux_window).copied() != Some(1) {
                continue;
            }
            let Some(rect) = pane.rect else { continue };
            if rect.is_empty() {
                continue;
            }
            for command in [
                format!("set-option -w -t {} window-size manual", pane.tmux_window),
                format!(
                    "set-option -w -t {} pane-border-status off",
                    pane.tmux_window
                ),
                format!(
                    "resize-window -t {} -x {} -y {}",
                    pane.tmux_window, rect.w, rect.h
                ),
            ] {
                let _ = self
                    .client
                    .send_deferred(renderer_control::guarded_command(&record, &command));
            }
        }
        self.renderer.owner = Some(record.clone());
        let fence = renderer_control::guarded_command(
            &record,
            &format!("display-message -p {}", record.token),
        );
        let _ = self.client.send_deferred_tagged(fence, Tag::ClaimFence);
    }

    fn finish_renderer_claim(&mut self, reply: &Reply) {
        let Some(record) = self.renderer.owner.clone() else {
            self.renderer.become_follower(None);
            return;
        };
        let confirmed = reply.ok
            && !renderer_control::reply_denied(reply)
            && reply
                .text_lines()
                .iter()
                .any(|line| line.trim() == record.token);
        if !confirmed {
            self.renderer.become_follower(None);
            self.pending_owner_input.clear();
            self.toast("Renderer ownership claim failed");
            return;
        }
        self.renderer.become_controller(record);
        self.interactions.abandon_pane_input();
        tracing::info!(token = %self.renderer.token, "renderer control claimed");
        let live = self
            .panes
            .iter()
            .map(|pane| pane.tmux_pane.to_string())
            .collect();
        self.save_config(audit::Reason::Reconcile { live });
        self.request_reconcile();
        self.apply_window_sizes();
        self.ensure_keepalive();
        self.request_prototype_path();
        self.dirty = true;

        let pending = std::mem::take(&mut self.pending_owner_input);
        for event in pending {
            if !self.handle_timed_input_now(event) {
                self.want_exit = true;
                break;
            }
        }
        self.renderer.claim_lock = None;
    }

    pub(super) fn handle_cc(&mut self, ev: CcEvent) -> bool {
        match self.router.route(ev) {
            CcRouted::Notification(ev) => self.handle_notification(ev),
            CcRouted::Reply(tag, mut reply) => {
                // Decode octal-escaped payloads on servers that escape them;
                // the probe reply itself must stay raw to be judged (#19).
                if !matches!(tag, Tag::EscapeProbe) && self.replies_escaped == Some(true) {
                    reply.unescape_lines();
                }
                self.handle_reply(tag, reply);
                true
            }
            CcRouted::Consumed => true,
            CcRouted::Desync => {
                tracing::error!("protocol desync — exiting (restart to reattach)");
                false
            }
        }
    }

    fn handle_notification(&mut self, ev: CcEvent) -> bool {
        match ev {
            CcEvent::Output { pane, data } | CcEvent::ExtendedOutput { pane, data, .. } => {
                self.metrics.record_input(data.len());
                let output_at = Instant::now();
                let mut visible_output = false;
                let data = if self.fault_drop > 0 {
                    let n = data.len().min(self.fault_drop);
                    self.fault_drop -= n;
                    data[n..].to_vec()
                } else {
                    data
                };
                let mut clipboard_out: Vec<String> = Vec::new();
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    let now = output_at;
                    if now.duration_since(p.window_start) >= FLOOD_WINDOW {
                        p.window_start = now;
                        p.window_bytes = 0;
                    }
                    p.window_bytes += data.len() as u64;

                    if let Some(buffer) = &mut p.reseed_buffer {
                        // Recorded when the seed drains it (finish_reseed).
                        buffer.push(data);
                    } else {
                        let effects = p.advance_recorded(&data);
                        for effect in effects {
                            if let Some(text) = handle_side_effect(
                                &self.client,
                                self.renderer.owner_record(),
                                p,
                                effect,
                            ) {
                                clipboard_out.push(text);
                            }
                        }
                        p.engine.on_output();
                        p.dirty = true;
                        p.last_output = Some(now);
                        if p.status != PaneStatus::Dead {
                            p.status = PaneStatus::Working;
                        }
                        self.dirty = true;
                        visible_output = true;
                    }

                    if !p.throttled && p.window_bytes > FLOOD_BYTES_PER_WINDOW {
                        tracing::info!(pane = %pane, "flood detected; throttling output at source");
                        p.throttled = true;
                        p.resume_at = Some(now + FLOOD_RESEED_EVERY);
                        let _ = self.client.send(format!("refresh-client -A '{pane}:off'"));
                        self.dirty = true;
                    }
                }
                if visible_output {
                    self.handle_pane_interaction_output(pane, output_at);
                }
                for text in clipboard_out {
                    self.forward_clipboard(&text);
                }
                true
            }
            CcEvent::Pause(pane) => {
                self.metrics.pauses += 1;
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    p.paused = true;
                    p.begin_reseed();
                    let _ = self
                        .client
                        .send(format!("refresh-client -A '{pane}:continue'"));
                    let _ = self.client.send_tagged(p.seed_command(), Tag::Seed(pane));
                    let _ = self
                        .client
                        .send_tagged(p.cursor_command(), Tag::Cursor(pane));
                    self.dirty = true;
                }
                true
            }
            CcEvent::Continue(pane) => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane) {
                    p.paused = false;
                    p.dirty = true;
                    self.dirty = true;
                }
                true
            }
            CcEvent::WindowClose(w) => {
                for p in self.panes.iter_mut().filter(|p| p.tmux_window == w) {
                    p.status = PaneStatus::Dead;
                    p.dirty = true;
                }
                self.request_reconcile();
                true
            }
            CcEvent::UnlinkedWindowClose(_) => {
                // A window closed in a session OURS is not attached to
                // (grouped sessions, other users' sessions). Our windows are
                // always linked to our session, so this is never one of our
                // panes dying — marking Dead here false-kills healthy panes
                // whenever session groups churn. Reconcile picks up any real
                // topology change.
                self.request_reconcile();
                true
            }
            CcEvent::WindowAdd(_)
            | CcEvent::LayoutChange { .. }
            | CcEvent::WindowPaneChanged { .. }
            | CcEvent::PaneModeChanged(_) => {
                self.request_reconcile();
                true
            }
            CcEvent::SubscriptionChanged { raw } => {
                if let Some(owner) = renderer_control::parse_owner_subscription(&raw) {
                    let previous = self.renderer.owner.clone();
                    let departed = self.renderer.observe_owner(owner.clone());
                    tracing::info!(owner = ?owner.as_ref().map(|record| &record.token), "renderer owner changed");
                    self.dirty = true;
                    if owner.is_none()
                        && self.renderer.state != renderer_control::State::LegacyFollower
                    {
                        let expected = departed.or_else(|| previous.map(|record| record.token));
                        self.request_renderer_claim(
                            renderer_control::ClaimReason::Recovery,
                            expected,
                        );
                    }
                } else {
                    self.request_reconcile();
                }
                true
            }
            CcEvent::ClientDetached { client } => {
                let expected = self
                    .renderer
                    .owner
                    .as_ref()
                    .filter(|owner| owner.client_name == client)
                    .map(|owner| owner.token.clone());
                if expected.is_some() && !self.renderer.is_controller() {
                    self.request_renderer_claim(renderer_control::ClaimReason::Recovery, expected);
                }
                true
            }
            CcEvent::WindowRenamed { window, name }
            | CcEvent::UnlinkedWindowRenamed { window, name } => {
                if let Some(p) = self
                    .panes
                    .iter_mut()
                    .find(|p| p.tmux_window == window && p.title.is_empty())
                {
                    p.title = name;
                    self.dirty = true;
                }
                true
            }
            CcEvent::Exit(reason) => {
                tracing::info!(?reason, "server closed control connection");
                false
            }
            CcEvent::ConfigError(err) => {
                tracing::warn!(%err, "tmux config error");
                true
            }
            CcEvent::Unknown(line) => {
                tracing::debug!(%line, "unknown control-mode line");
                true
            }
            _ => true,
        }
    }

    fn handle_reply(&mut self, tag: Tag, reply: Reply) {
        match tag {
            Tag::Input(pane, sequence) => {
                let accepted = reply.ok && !renderer_control::reply_denied(&reply);
                self.handle_pane_input_ack(pane, sequence, accepted, Instant::now())
            }
            Tag::ListPanes => {
                self.reconcile_in_flight = false;
                self.apply_pane_list(&reply);
                self.startup_panes_ready = true;
                self.maybe_finish_renderer_startup();
                if self.reconcile_again {
                    self.reconcile_again = false;
                    self.request_reconcile();
                }
            }
            Tag::Seed(pane_id) => {
                // An error reply (e.g. the pane died mid-flight) must never
                // be applied as grid content.
                if reply.ok {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) {
                        p.pending_seed = Some(reply);
                    }
                }
            }
            Tag::VerifyCap(pane_id) => {
                if !reply.ok {
                    return;
                }
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) {
                    if p.reseed_buffer.is_none() && !p.paused && !p.throttled {
                        p.pending_verify = Some(crate::verify::PendingCapture {
                            reply,
                            at: Instant::now(),
                            cols: p.cols,
                            rows: p.rows,
                            reseed_count: p.reseed_count,
                        });
                    }
                }
            }
            Tag::Cursor(pane_id) => {
                if let Some(p) = self.panes.iter_mut().find(|p| p.tmux_pane == pane_id) {
                    if let Some(seed) = p.pending_seed.take() {
                        let cursor = reply
                            .ok
                            .then(|| session::parse_cursor_reply(&reply))
                            .flatten();
                        p.finish_reseed(&seed, cursor);
                        self.dirty = true;
                    } else {
                        // Seed failed: stop buffering so live output flows again.
                        if let Some(buffered) = p.reseed_buffer.take() {
                            for chunk in buffered {
                                let _ = p.advance_recorded(&chunk);
                            }
                        }
                        self.dirty = true;
                    }
                }
            }
            Tag::EscapeProbe => {
                // Raw servers echo the literal 0x01 byte; escaping servers
                // turn it into the four bytes \001.
                let escaped = reply
                    .lines
                    .first()
                    .map(|l| !l.contains(&0x01) && l.windows(4).any(|w| w == b"\\001"))
                    .unwrap_or(false);
                self.replies_escaped = Some(escaped);
                tracing::info!(escaped, "reply-escaping probe");
            }
            Tag::RendererIdentity => {
                self.renderer.identity = reply
                    .text_lines()
                    .first()
                    .and_then(|line| renderer_control::TmuxIdentity::parse(line));
                if self.renderer.identity.is_none() {
                    tracing::error!("tmux renderer identity reply was invalid");
                    self.renderer.become_follower(None);
                }
                self.maybe_finish_renderer_startup();
            }
            Tag::ClaimCheck => {
                let (legacy_pid, current) = reply
                    .text_lines()
                    .first()
                    .map(|line| renderer_control::parse_claim_check(line))
                    .unwrap_or((None, None));
                let legacy_alive = legacy_pid.is_some_and(renderer_control::pid_alive);
                if self.renderer.claim_lock.is_none() {
                    self.startup_legacy_checked = true;
                    self.startup_legacy_alive = legacy_alive;
                    self.renderer.owner = current;
                    self.maybe_finish_renderer_startup();
                    return;
                }
                if legacy_alive {
                    self.renderer.become_legacy_follower();
                    self.pending_owner_input.clear();
                    self.toast("Viewing: TypeScript dmux controls this session");
                    return;
                }
                if self.renderer.claim_reason == Some(renderer_control::ClaimReason::Recovery) {
                    let expected = self.renderer.recovery_expected.as_deref();
                    let replaced = current
                        .as_ref()
                        .is_some_and(|owner| Some(owner.token.as_str()) != expected);
                    if replaced {
                        self.renderer.become_follower(current);
                        self.dirty = true;
                        return;
                    }
                }
                let Some(record) = self.renderer.record(self.size) else {
                    self.renderer.become_follower(current);
                    return;
                };
                self.send_renderer_claim(record);
            }
            Tag::ClaimFence => {
                self.finish_renderer_claim(&reply);
            }
            Tag::NewWindow(ctx) => {
                self.finish_new_window(*ctx, &reply);
            }
            Tag::KillWindow(pane_id) => {
                let err = reply.text_lines().first().cloned().unwrap_or_default();
                let ok = reply.ok && !renderer_control::reply_denied(&reply);
                self.finish_close(pane_id, ok, err);
            }
            Tag::KeepaliveCreated => {
                self.keepalive_pending = false;
                if reply.ok && !renderer_control::reply_denied(&reply) {
                    // Pin the name so name-based tooling stays readable even
                    // under automatic-rename configs (identity itself is the
                    // start command and does not depend on this).
                    if let Some(win) = reply.text_lines().first().map(|l| l.trim().to_string()) {
                        if win.starts_with('@') {
                            let _ = self.send_shared(format!(
                                "set-option -w -t {win} automatic-rename off"
                            ));
                        }
                    }
                } else {
                    // Creation failed; allow a later reconcile to retry.
                    self.keepalive_present = false;
                }
            }
            Tag::PrototypePath => self.receive_prototype_path(&reply),
        }
    }
}
