//! Local prototype handoff: build a worktree and ask the live controller to
//! re-exec that binary while preserving its tmux session and renderer claim.

use std::io;
use std::path::{Path, PathBuf};

use dmux_cc::Reply;

use crate::{renderer_control, updater, App, AppMsg, Cli, Tag};

pub(crate) struct PendingPrototype {
    path: PathBuf,
    since: std::time::Instant,
    blocker: Option<crate::reload_gate::Blocker>,
}

pub(crate) fn default_executable() -> PathBuf {
    std::env::var_os("DMUX_PROTOTYPE_DEFAULT_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("dmux-rs"))
}

pub(crate) fn dmux_worktree(path: &Path) -> Option<PathBuf> {
    let worktree = crate::git::git_worktree_root_for_path(path)?;
    let repository = crate::github::repository_for_dir(&worktree).ok()?;
    (repository.slug == crate::report::DEFAULT_REPO).then_some(worktree)
}

pub(crate) fn active_worktree() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    if std::fs::canonicalize(&current).ok()? == std::fs::canonicalize(default_executable()).ok()? {
        return None;
    }
    dmux_worktree(current.parent()?)
}

pub(crate) fn reexec(
    target: &updater::ReexecTarget,
    token: &str,
    context: &renderer_control::ReexecContext,
) -> String {
    updater::reexec_target(target, token, context, &default_executable())
}

pub(crate) fn run_command(
    cli: &Cli,
    worktree: &Path,
    watch: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if watch {
        return crate::prototype_watcher::run(cli, worktree);
    }
    let (_, _, session_name) = crate::startup::resolve_session(cli)?;
    let (command, owner) = controller(cli, &session_name)?;
    let executable = updater::build_prototype(worktree).map_err(io::Error::other)?;
    request_handoff(&command, &owner, &executable)?;
    println!(
        "prototype requested for '{session_name}': {}",
        executable.display()
    );
    Ok(())
}

pub(crate) fn handoff(cli: &Cli, session_name: &str, executable: &Path) -> io::Result<()> {
    let (command, owner) = controller(cli, session_name)?;
    request_handoff(&command, &owner, executable)
}

pub(crate) fn ensure_controller(cli: &Cli, session_name: &str) -> io::Result<()> {
    controller(cli, session_name).map(|_| ())
}

fn controller(
    cli: &Cli,
    session_name: &str,
) -> io::Result<(
    renderer_control::CommandContext,
    renderer_control::OwnerRecord,
)> {
    let command = renderer_control::CommandContext {
        tmux: cli.tmux.clone(),
        socket: cli.socket.clone(),
        session_name: session_name.to_string(),
    };
    let owner = command.read_owner()?.ok_or_else(|| {
        io::Error::other(format!(
            "no running dmux-rs controller for '{session_name}'"
        ))
    })?;
    if owner.pid > i32::MAX as u32 || !renderer_control::pid_alive(owner.pid as i32) {
        return Err(io::Error::other(format!(
            "dmux-rs controller for '{session_name}' is no longer running"
        )));
    }
    Ok((command, owner))
}

fn request_handoff(
    command: &renderer_control::CommandContext,
    owner: &renderer_control::OwnerRecord,
    executable: &Path,
) -> io::Result<()> {
    if !command.request_prototype(owner, executable)? {
        return Err(io::Error::other(
            "the dmux-rs controller changed before the prototype request was sent",
        ));
    }
    let current = command.read_owner()?;
    let published = published_prototype(command)?;
    if current.as_ref().map(|record| &record.token) != Some(&owner.token)
        || published.as_deref() != Some(executable)
    {
        clear_prototype_request(command, owner);
        return Err(io::Error::other(
            "the prototype request was not accepted by the current controller",
        ));
    }
    let signal_result = unsafe { libc::kill(owner.pid as i32, libc::SIGUSR1) };
    if signal_result != 0 {
        clear_prototype_request(command, owner);
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn clear_prototype_request(
    command: &renderer_control::CommandContext,
    owner: &renderer_control::OwnerRecord,
) {
    let mut process = std::process::Command::new(&command.tmux);
    if let Some(socket) = &command.socket {
        process.args(["-L", socket]);
    }
    let condition = format!(
        "#{{==:#{{{}}},{}}}",
        renderer_control::TOKEN_OPTION,
        owner.token
    );
    let clear = format!(
        "unset-option -t {} {}",
        dmux_cc::quote_arg(&command.session_name),
        renderer_control::PROTOTYPE_OPTION
    );
    let _ = process
        .args([
            "if-shell",
            "-t",
            &command.session_name,
            "-F",
            &condition,
            &clear,
        ])
        .status();
}

fn published_prototype(command: &renderer_control::CommandContext) -> io::Result<Option<PathBuf>> {
    let mut process = std::process::Command::new(&command.tmux);
    if let Some(socket) = &command.socket {
        process.args(["-L", socket]);
    }
    let output = process
        .args([
            "show-options",
            "-t",
            &command.session_name,
            "-qv",
            renderer_control::PROTOTYPE_OPTION,
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("could not confirm prototype handoff"));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!raw.is_empty()).then(|| PathBuf::from(raw)))
}

pub(crate) fn spawn_signal_listener(
    tx: tokio::sync::mpsc::UnboundedSender<AppMsg>,
) -> io::Result<()> {
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
    tokio::spawn(async move {
        while signal.recv().await.is_some() {
            if tx.send(AppMsg::PrototypeRequested).is_err() {
                break;
            }
        }
    });
    Ok(())
}

impl App {
    pub(crate) fn start_prototype_build(&mut self, raw_path: String) {
        let worktree = crate::command_dispatch::expand_user_path(&raw_path);
        if !worktree.is_dir() {
            self.toast(format!(
                "Prototype path is not a directory: {}",
                worktree.display()
            ));
            return;
        }
        let (view, build_ui) = crate::views::PrototypeBuildView::new(&worktree);
        self.views.push(Box::new(view));
        self.dirty = true;
        let tx = self.app_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = updater::build_prototype_with_output(&worktree, |line| {
                build_ui.detail(line);
            });
            match &result {
                Ok(_) => build_ui.ready(),
                Err(err) => build_ui.failed(err.clone()),
            }
            let _ = tx.send(AppMsg::PrototypeBuildDone(result));
        });
    }

    pub(crate) fn handle_prototype_build_done(&mut self, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => {
                self.queue_prototype(path);
                self.try_apply_pending_prototype();
            }
            Err(err) => self.toast(format!("prototype build failed: {err}")),
        }
    }

    pub(crate) fn unload_prototype(&mut self) {
        let executable = default_executable();
        if !executable.is_file() {
            self.toast(format!(
                "default dmux-rs binary is unavailable: {}",
                executable.display()
            ));
            return;
        }
        self.queue_prototype(executable);
        self.try_apply_pending_prototype();
    }

    pub(crate) fn request_prototype_path(&mut self) {
        if !self.renderer.is_controller() {
            tracing::debug!("ignored prototype request while renderer is not controller");
            return;
        }
        let _ = self.client.send_deferred_tagged(
            renderer_control::prototype_path_command(&self.session_name),
            Tag::PrototypePath,
        );
    }

    pub(crate) fn receive_prototype_path(&mut self, reply: &Reply) {
        let clear = renderer_control::clear_prototype_command(&self.session_name);
        if let Some(owner) = self.renderer.owner_record().cloned() {
            let _ = self
                .client
                .send(renderer_control::guarded_command(&owner, &clear));
        }
        if !reply.ok {
            self.toast("prototype request could not be read");
            return;
        }
        let lines = reply.text_lines();
        let Some(raw_path) = lines.first().map(String::as_str) else {
            return;
        };
        if raw_path.trim().is_empty() {
            return;
        }
        let path = PathBuf::from(raw_path);
        let Ok(path) = std::fs::canonicalize(&path) else {
            self.toast("prototype binary is unavailable");
            return;
        };
        if !path.is_file() {
            self.toast("prototype path is not an executable file");
            return;
        }
        self.queue_prototype(path);
        self.try_apply_pending_prototype();
    }

    fn queue_prototype(&mut self, path: PathBuf) {
        self.pending_prototype = Some(PendingPrototype {
            path,
            since: std::time::Instant::now(),
            blocker: None,
        });
    }

    pub(crate) fn pending_prototype_deadline(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        let pending = self.pending_prototype.as_ref()?;
        [
            self.reload_gate.quiet_deadline(),
            pending.since + crate::util::UPDATE_DEFER_CAP,
        ]
        .into_iter()
        .filter(|deadline| *deadline > now)
        .min()
    }

    pub(crate) fn try_apply_pending_prototype(&mut self) {
        let Some(pending) = &self.pending_prototype else {
            return;
        };
        let now = std::time::Instant::now();
        let facts = crate::reload_gate::ReloadFacts {
            controller_ready: self.renderer.is_controller(),
            interaction_active: self.mouse_buttons.any_down()
                || self.sidebar_drag.is_some()
                || self.profiler_drag.is_some()
                || self.drag_select.is_some()
                || self.mouse_forward.is_some(),
            pane_input_pending: !self.pending_owner_input.is_empty()
                || self.scroll_pacer.is_pending()
                || self.interactions.pane_input_pending(),
            text_entry_open: self.views.blocks_reload(),
            bootstrap_active: self.bootstraps.values().any(|ui| ui.done_at.is_none()),
            prompt_injections: self.pending_injection_count(),
            candidate_wait: now.saturating_duration_since(pending.since),
        };
        if let Some(blocker) = self.reload_gate.blocker(now, facts) {
            if pending.blocker != Some(blocker) {
                if let Some(pending) = &mut self.pending_prototype {
                    pending.blocker = Some(blocker);
                }
                self.status_msg = blocker.message().to_string();
                self.status_clear_at = None;
                self.dirty = true;
            }
            return;
        }
        let path = self
            .pending_prototype
            .take()
            .expect("prototype pending")
            .path;
        self.toast("loading local prototype…");
        self.reexec_after = Some(updater::ReexecTarget::Prototype(path));
        self.want_exit = true;
    }
}
