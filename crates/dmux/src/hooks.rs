//! Project lifecycle hooks (`<root>/.dmux-hooks/<name>`), the TS `hooks.ts`
//! contract. Everything here is fire-and-forget; the one veto hook
//! (`pre_merge`) runs inline in `git.rs` instead.

use std::path::{Path, PathBuf};

pub fn hook_path(root: &Path, name: &str) -> Option<PathBuf> {
    let p = root.join(".dmux-hooks").join(name);
    p.is_file().then_some(p)
}

/// Spawn the hook detached with the standard env; never blocks the UI and
/// never fails visibly (missing/broken hooks are the project's business).
pub fn run_detached(root: &Path, name: &str, cwd: &Path, envs: &[(&str, String)]) {
    let Some(path) = hook_path(root, name) else { return };
    // The preferred cwd may not exist yet (pane_created fires while the
    // pane's own bootstrap is still creating the worktree) — fall back to
    // the project root rather than failing to spawn.
    let cwd = if cwd.is_dir() { cwd } else { root };
    let mut cmd = std::process::Command::new(path);
    cmd.current_dir(cwd)
        .env("DMUX_ROOT", root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    match cmd.spawn() {
        Ok(_) => tracing::debug!(hook = name, "lifecycle hook spawned"),
        Err(err) => tracing::warn!(hook = name, %err, "lifecycle hook failed to spawn"),
    }
}
