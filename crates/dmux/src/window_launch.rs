//! tmux window creation, bootstrap completion, and pane input sequencing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dmux_cc::{PaneId, Reply};
use dmux_core::{encode_pane_title, DmuxPane, PaneKind};

use crate::registry::{self, project_context};
use crate::{bootstrap, input, shq, timestamp, App, AppMsg, Tag};

/// Context for a window dmux-rs created and is waiting on.
#[derive(Debug)]
pub(super) struct NewWindowCtx {
    pub(super) slug: String,
    pub(super) display: String,
    /// Prompt recorded on the pane (drives resume/duplicate).
    pub(super) prompt: String,
    pub(super) kind: PaneKind,
    pub(super) agent: Option<String>,
    pub(super) launch_cmd: Option<String>,
    /// (prompt, delay) for send-keys transport agents.
    pub(super) injection: Option<(String, u64)>,
    pub(super) worktree_path: Option<String>,
    /// Working directory for the new window (default: project root).
    pub(super) cwd: Option<String>,
    /// Owning project root when not the main project.
    pub(super) project_root: Option<String>,
    /// Native bootstrap (worktree + hook run by dmux, loader UI in the pane)
    /// with the agent launch deferred until it finishes.
    pub(super) bootstrap: Option<BootstrapSpec>,
}

#[derive(Debug)]
pub(super) struct BootstrapSpec {
    pub(super) plan: bootstrap::Plan,
    pub(super) launch: bootstrap::Launch,
    pub(super) agent_label: String,
}

fn new_window_command(cwd: &str) -> String {
    format!(
        "new-window -d -P -F '#{{window_id}}\u{1}#{{pane_id}}' -c {}",
        dmux_cc::quote_arg(cwd)
    )
}

fn window_cwd(ctx: &NewWindowCtx, session_root: &Path) -> String {
    ctx.cwd
        .clone()
        .or_else(|| ctx.project_root.clone())
        .unwrap_or_else(|| session_root.to_string_lossy().into_owned())
}

fn created_pane_id(reply: &Reply) -> Option<PaneId> {
    let line = reply.text_lines().into_iter().next()?;
    let mut fields = line.split('\u{1}');
    fields.next()?;
    let pane = fields.next()?;
    pane.strip_prefix('%')?.parse().ok().map(PaneId)
}

fn pane_line_commands(pane: PaneId, line: &str) -> Vec<String> {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\r');
    bytes
        .chunks(256)
        .map(|chunk| input::send_keys_hex(pane, chunk))
        .collect()
}

fn bootstrap_launch_line(launch: &bootstrap::Launch) -> String {
    format!(
        "clear; cd {} 2>/dev/null || cd {}; {}",
        shq(&launch.wt),
        shq(&launch.root),
        launch.agent_cmd
    )
}

enum BootstrapOutcome {
    None,
    Failed(String),
    Launch(PaneId, bootstrap::Launch),
}

fn advance_bootstrap(ui: &mut bootstrap::Ui, ev: bootstrap::Ev, now: Instant) -> BootstrapOutcome {
    match ev {
        bootstrap::Ev::Step(i) => {
            ui.current = i.min(ui.steps.len().saturating_sub(1));
            BootstrapOutcome::None
        }
        bootstrap::Ev::Detail(line) => {
            ui.detail = line;
            BootstrapOutcome::None
        }
        bootstrap::Ev::Failed(err) => {
            let message = format!("Bootstrap failed for '{}': {err}", ui.title);
            ui.failed = Some(err);
            ui.done_at = Some(now);
            BootstrapOutcome::Failed(message)
        }
        bootstrap::Ev::Done => {
            ui.done_at = Some(now);
            ui.detail.clear();
            match ui.launch.take() {
                Some(launch) => BootstrapOutcome::Launch(ui.pane, launch),
                None => BootstrapOutcome::None,
            }
        }
    }
}

fn take_due_injections(
    pending: &mut Vec<(PaneId, String, Instant)>,
    now: Instant,
) -> Vec<(PaneId, String)> {
    let mut due = Vec::new();
    pending.retain(|(pane, prompt, at)| {
        if now >= *at {
            due.push((*pane, prompt.clone()));
            false
        } else {
            true
        }
    });
    due
}

impl App {
    pub(super) fn next_injection_deadline(&self) -> Option<Instant> {
        self.pending_injections.iter().map(|(_, _, at)| *at).min()
    }

    pub(super) fn pending_injection_count(&self) -> usize {
        self.pending_injections.len()
    }

    pub(super) fn create_window(&mut self, ctx: NewWindowCtx) {
        let cwd = window_cwd(&ctx, &self.project_root);
        let hook_root = ctx
            .project_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        self.run_hook_if_controller(
            &hook_root,
            "before_pane_create",
            &hook_root,
            &[("DMUX_SLUG", ctx.slug.clone())],
        );
        let _ = self.send_shared_tagged(new_window_command(&cwd), Tag::NewWindow(Box::new(ctx)));
    }

    pub(super) fn new_terminal(&mut self, project_root: Option<String>) {
        // Slug must not collide with live panes OR persisted records from
        // any project (#76: a reused terminal-N repointed another project's
        // record and stole its ownership metadata).
        let taken = |slug: &str| {
            self.panes.iter().any(|p| p.slug == slug)
                || self.config.panes.iter().any(|r| r.slug == slug)
        };
        let mut n = 1 + self
            .panes
            .iter()
            .filter(|p| p.slug.starts_with("terminal-"))
            .count();
        while taken(&format!("terminal-{n}")) {
            n += 1;
        }
        let slug = format!("terminal-{n}");
        let project_root = project_root.or_else(|| self.active_project_root());
        let cwd = project_root.clone();
        let project_root = project_context(&self.project_root, project_root);
        self.create_window(NewWindowCtx {
            bootstrap: None,
            prompt: String::new(),
            display: slug.clone(),
            slug,
            kind: PaneKind::Shell,
            agent: None,
            launch_cmd: None,
            injection: None,
            worktree_path: None,
            cwd,
            project_root,
        });
    }

    pub(super) fn finish_new_window(&mut self, mut ctx: NewWindowCtx, reply: &Reply) {
        let Some(pane_id) = created_pane_id(reply) else {
            self.toast("Pane creation failed");
            return;
        };

        let encoded = encode_pane_title(&ctx.display, &ctx.slug);
        let _ = self.send_shared(format!(
            "select-pane -t {pane_id} -T {}",
            dmux_cc::quote_arg(&encoded)
        ));

        if let Some(spec) = ctx.bootstrap.take() {
            let steps = bootstrap::Ui::step_labels(&spec.agent_label, spec.plan.has_hook);
            self.bootstraps.insert(
                ctx.slug.clone(),
                bootstrap::Ui {
                    pane: pane_id,
                    title: ctx.display.clone(),
                    agent_label: spec.agent_label,
                    branch: spec.plan.branch.clone(),
                    steps,
                    current: 0,
                    detail: String::new(),
                    started: Instant::now(),
                    done_at: None,
                    failed: None,
                    launch: Some(spec.launch),
                },
            );
            let slug = ctx.slug.clone();
            let tx = self.app_tx.clone();
            tokio::task::spawn_blocking(move || {
                bootstrap::run_blocking(&spec.plan, &mut |ev| {
                    let _ = tx.send(AppMsg::Bootstrap {
                        slug: slug.clone(),
                        ev,
                    });
                });
            });
        }

        if let Some(cmd) = &ctx.launch_cmd {
            self.send_pane_line(pane_id, cmd);
        }
        if let Some((prompt, delay_ms)) = &ctx.injection {
            self.pending_injections.push((
                pane_id,
                prompt.clone(),
                Instant::now() + Duration::from_millis(*delay_ms),
            ));
        }

        let hook_root = ctx
            .project_root
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        let hook_cwd = ctx
            .worktree_path
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| hook_root.clone());
        let mut hook_env = vec![
            ("DMUX_SLUG", ctx.slug.clone()),
            ("DMUX_PANE_ID", pane_id.to_string()),
        ];
        if let Some(wt) = &ctx.worktree_path {
            hook_env.push(("DMUX_WORKTREE_PATH", wt.clone()));
        }
        self.run_hook_if_controller(&hook_root, "pane_created", &hook_cwd, &hook_env);

        // Config record first so reconcile adoption pairs slug → agent.
        // Resumed worktrees reuse their existing record (fresh pane id).
        if let Some(existing) = registry::reusable_record_index(
            &self.config.panes,
            &ctx.slug,
            ctx.project_root.as_deref(),
        )
        .map(|index| &mut self.config.panes[index])
        {
            // Same slug in the SAME project: a resume — refresh the pane id
            // and complete ownership metadata (#76: updating only
            // pane_id/agent let a reused slug keep another context's
            // projectRoot/kind/cwd).
            existing.pane_id = pane_id.to_string();
            existing.agent = ctx.agent.clone().or_else(|| existing.agent.clone());
            existing.kind = Some(ctx.kind);
            existing.worktree_path = ctx.worktree_path.clone();
            existing.shell_cwd = ctx.cwd.clone();
            existing.project_root = ctx.project_root.clone();
            existing.project_name = ctx.project_root.as_deref().map(|r| {
                Path::new(r)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.to_string())
            });
        } else {
            let mut record = DmuxPane::new_record(
                format!("pane-{}", timestamp()),
                ctx.slug.clone(),
                pane_id.to_string(),
                ctx.kind,
            );
            // A display equal to the slug is a default, not a chosen name.
            // Leave it unset so shell panes auto-name from their own titles.
            record.display_name = (ctx.display != ctx.slug).then(|| ctx.display.clone());
            record.prompt = ctx.prompt.clone();
            record.agent = ctx.agent.clone();
            record.worktree_path = ctx.worktree_path.clone();
            record.project_root = ctx.project_root.clone();
            record.project_name = ctx.project_root.as_deref().map(|r| {
                Path::new(r)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.to_string())
            });
            self.config.panes.push(record);
        }
        self.save_config(crate::audit::Reason::PaneLaunched);

        self.pending_focus = Some(pane_id);
        self.request_reconcile();
    }

    pub(super) fn handle_bootstrap_event(&mut self, slug: String, ev: bootstrap::Ev) {
        let outcome = if let Some(ui) = self.bootstraps.get_mut(&slug) {
            let outcome = advance_bootstrap(ui, ev, Instant::now());
            self.dirty = true;
            outcome
        } else {
            BootstrapOutcome::None
        };
        match outcome {
            BootstrapOutcome::None => {}
            BootstrapOutcome::Failed(message) => self.toast(message),
            BootstrapOutcome::Launch(pane, launch) => {
                self.send_pane_line(pane, &bootstrap_launch_line(&launch));
                if let Some((prompt, delay_ms)) = launch.injection {
                    self.pending_injections.push((
                        pane,
                        prompt,
                        Instant::now() + Duration::from_millis(delay_ms),
                    ));
                }
            }
        }
        self.try_apply_pending_update();
    }

    pub(super) fn send_due_injections(&mut self, now: Instant) {
        for (pane, prompt) in take_due_injections(&mut self.pending_injections, now) {
            self.send_pane_line(pane, &prompt);
        }
    }

    fn send_pane_line(&mut self, pane: PaneId, line: &str) {
        for command in pane_line_commands(pane, line) {
            let _ = self.send_shared(command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(lines: &[&str]) -> Reply {
        Reply {
            lines: lines.iter().map(|line| line.as_bytes().to_vec()).collect(),
            ok: true,
            rtt: Duration::ZERO,
        }
    }

    fn context(cwd: Option<&str>, project_root: Option<&str>) -> NewWindowCtx {
        NewWindowCtx {
            slug: "terminal-1".into(),
            display: "terminal-1".into(),
            prompt: String::new(),
            kind: PaneKind::Shell,
            agent: None,
            launch_cmd: None,
            injection: None,
            worktree_path: None,
            cwd: cwd.map(str::to_owned),
            project_root: project_root.map(str::to_owned),
            bootstrap: None,
        }
    }

    #[test]
    fn new_window_command_preserves_the_requested_directory() {
        assert_eq!(
            new_window_command("/tmp/project with spaces"),
            "new-window -d -P -F '#{window_id}\u{1}#{pane_id}' -c '/tmp/project with spaces'"
        );
    }

    #[test]
    fn window_directory_prefers_explicit_then_project_then_session_root() {
        let session = Path::new("/session");
        assert_eq!(
            window_cwd(&context(Some("/explicit"), Some("/project")), session),
            "/explicit"
        );
        assert_eq!(
            window_cwd(&context(None, Some("/project")), session),
            "/project"
        );
        assert_eq!(window_cwd(&context(None, None), session), "/session");
    }

    #[test]
    fn created_window_reply_requires_a_tmux_pane_id() {
        assert_eq!(created_pane_id(&reply(&["@4\u{1}%17"])), Some(PaneId(17)));
        assert_eq!(
            created_pane_id(&reply(&["@4\u{1}%17\u{1}ignored"])),
            Some(PaneId(17))
        );
        assert_eq!(created_pane_id(&reply(&["@4\u{1}pane-17"])), None);
        assert_eq!(created_pane_id(&reply(&[])), None);
    }

    #[test]
    fn pane_lines_append_enter_after_all_text_chunks() {
        let line = "x".repeat(300);
        let commands = pane_line_commands(PaneId(9), &line);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].matches(" 78").count(), 256);
        assert!(!commands[0].ends_with(" 0d"));
        assert_eq!(commands[1].matches(" 78").count(), 44);
        assert!(commands[1].ends_with(" 0d"));
    }

    #[test]
    fn bootstrap_launch_falls_back_to_the_project_root() {
        let launch = bootstrap::Launch {
            agent_cmd: "codex --yolo".into(),
            wt: "/tmp/work tree".into(),
            root: "/tmp/main repo".into(),
            injection: None,
        };
        assert_eq!(
            bootstrap_launch_line(&launch),
            "clear; cd '/tmp/work tree' 2>/dev/null || cd '/tmp/main repo'; codex --yolo"
        );
    }

    fn bootstrap_ui(launch: bootstrap::Launch) -> bootstrap::Ui {
        bootstrap::Ui {
            pane: PaneId(12),
            title: "agent".into(),
            agent_label: "Codex".into(),
            branch: "agent-branch".into(),
            steps: vec!["Create".into(), "Launch".into()],
            current: 0,
            detail: "working".into(),
            started: Instant::now(),
            done_at: None,
            failed: None,
            launch: Some(launch),
        }
    }

    #[test]
    fn bootstrap_defers_launch_until_done() {
        let launch = bootstrap::Launch {
            agent_cmd: "codex".into(),
            wt: "/tmp/worktree".into(),
            root: "/tmp/root".into(),
            injection: None,
        };
        let mut ui = bootstrap_ui(launch);
        let now = Instant::now();
        assert!(matches!(
            advance_bootstrap(&mut ui, bootstrap::Ev::Step(1), now),
            BootstrapOutcome::None
        ));
        assert!(ui.launch.is_some());

        let BootstrapOutcome::Launch(pane, launch) =
            advance_bootstrap(&mut ui, bootstrap::Ev::Done, now)
        else {
            panic!("bootstrap completion did not release the launch");
        };
        assert_eq!(pane, PaneId(12));
        assert_eq!(launch.agent_cmd, "codex");
        assert!(ui.launch.is_none());
        assert_eq!(ui.done_at, Some(now));
    }

    #[test]
    fn due_injections_are_removed_and_future_entries_remain() {
        let now = Instant::now();
        let mut pending = vec![
            (PaneId(1), "due".into(), now),
            (PaneId(2), "later".into(), now + Duration::from_secs(1)),
        ];
        assert_eq!(
            take_due_injections(&mut pending, now),
            vec![(PaneId(1), "due".into())]
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, PaneId(2));
    }
}
