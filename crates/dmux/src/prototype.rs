//! Local prototype handoff: build a worktree and ask the live controller to
//! re-exec that binary while preserving its tmux session and renderer claim.

use std::io;
use std::path::{Path, PathBuf};

use dmux_cc::Reply;

use crate::{renderer_control, updater, App, AppMsg, Cli, Tag};

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

pub(crate) fn run_command(cli: &Cli, worktree: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (_, _, session_name) = crate::startup::resolve_session(cli)?;
    let command = renderer_control::CommandContext {
        tmux: cli.tmux.clone(),
        socket: cli.socket.clone(),
        session_name: session_name.clone(),
    };
    let owner = command.read_owner()?.ok_or_else(|| {
        io::Error::other(format!(
            "no running dmux-rs controller for '{session_name}'"
        ))
    })?;
    if owner.pid > i32::MAX as u32 || !renderer_control::pid_alive(owner.pid as i32) {
        return Err(io::Error::other(format!(
            "dmux-rs controller for '{session_name}' is no longer running"
        ))
        .into());
    }
    let executable = updater::build_prototype(worktree).map_err(io::Error::other)?;
    if !command.request_prototype(&owner, &executable)? {
        return Err(io::Error::other(
            "the dmux-rs controller changed before the prototype request was sent",
        )
        .into());
    }
    let signal_result = unsafe { libc::kill(owner.pid as i32, libc::SIGUSR1) };
    if signal_result != 0 {
        return Err(io::Error::last_os_error().into());
    }
    println!(
        "prototype requested for '{session_name}': {}",
        executable.display()
    );
    Ok(())
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
                self.pending_prototype = Some((path, std::time::Instant::now()));
                self.try_apply_pending_prototype();
                if self.pending_prototype.is_some() {
                    self.toast("prototype built when the current launch settles…");
                }
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
        self.pending_prototype = Some((executable, std::time::Instant::now()));
        self.try_apply_pending_prototype();
        if self.pending_prototype.is_some() {
            self.toast("default dmux-rs will load when the current launch settles…");
        }
    }

    pub(crate) fn request_prototype_path(&mut self) {
        if !self.renderer.is_controller() {
            tracing::debug!("ignored prototype request while renderer is not controller");
            return;
        }
        let _ = self.client.send_tagged(
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
            self.toast("prototype request did not include a binary");
            return;
        };
        let path = PathBuf::from(raw_path);
        let Ok(path) = std::fs::canonicalize(&path) else {
            self.toast("prototype binary is unavailable");
            return;
        };
        if !path.is_file() {
            self.toast("prototype path is not an executable file");
            return;
        }
        self.pending_prototype = Some((path, std::time::Instant::now()));
        self.try_apply_pending_prototype();
        if self.pending_prototype.is_some() {
            self.toast("prototype loaded when the current launch settles…");
        }
    }

    pub(crate) fn try_apply_pending_prototype(&mut self) {
        let Some((_, since)) = &self.pending_prototype else {
            return;
        };
        if !crate::update_may_apply(
            self.bootstraps.values().any(|ui| ui.done_at.is_none()),
            self.pending_injection_count(),
            since.elapsed(),
        ) {
            return;
        }
        let (path, _) = self.pending_prototype.take().expect("prototype pending");
        self.toast("loading local prototype…");
        self.reexec_after = Some(updater::ReexecTarget::Prototype(path));
        self.want_exit = true;
    }
}
