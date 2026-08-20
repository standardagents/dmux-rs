//! Project lifecycle hooks (`<root>/.dmux-hooks/<name>`), the TS `hooks.ts`
//! contract. Ordinary hooks run on blocking workers while renderer ownership
//! stays locked. The veto hook (`pre_merge`) runs inline in `git.rs`.

use std::path::{Path, PathBuf};

pub fn hook_path(root: &Path, name: &str) -> Option<PathBuf> {
    let p = root.join(".dmux-hooks").join(name);
    p.is_file().then_some(p)
}

/// Run one hook to completion on a blocking worker.
pub(crate) fn run_blocking(root: &Path, name: &str, cwd: &Path, envs: &[(&str, String)]) {
    let Some(path) = hook_path(root, name) else {
        return;
    };
    let cwd = if cwd.is_dir() { cwd } else { root };
    let mut cmd = std::process::Command::new(path);
    cmd.current_dir(cwd)
        .env("DMUX_ROOT", root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    match cmd.status() {
        Ok(_) => tracing::debug!(hook = name, "lifecycle hook completed"),
        Err(err) => tracing::warn!(hook = name, %err, "lifecycle hook failed"),
    }
}

impl crate::App {
    pub(crate) fn run_hook_if_controller(
        &self,
        root: &Path,
        name: &str,
        cwd: &Path,
        env: &[(&str, String)],
    ) {
        let Some(owner_guard) = self.renderer.confirmed_guard() else {
            return;
        };
        let root = root.to_path_buf();
        let name = name.to_string();
        let cwd = cwd.to_path_buf();
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect();
        tokio::task::spawn_blocking(move || {
            let borrowed: Vec<(&str, String)> = env
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone()))
                .collect();
            run_blocking(&root, &name, &cwd, &borrowed);
            drop(owner_guard);
        });
    }
}
