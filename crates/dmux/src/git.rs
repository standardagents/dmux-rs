//! Git operations for the merge flow. Fast queries run synchronously; the
//! merge itself runs on a blocking task and reports back over the app
//! channel (hooks can make merges slow).

use std::path::Path;

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

/// Uncommitted changes (staged, unstaged, or untracked)?
pub fn worktree_dirty(path: &Path) -> bool {
    git(path, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

pub fn current_branch(path: &Path) -> Option<String> {
    git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|b| b != "HEAD")
}

/// Commit everything in the worktree (when `message` is Some), then merge its
/// branch into the project root's checked-out branch. On conflict the merge
/// is aborted so the root stays clean. Blocking — call from spawn_blocking.
pub fn commit_and_merge(
    root: &Path,
    worktree: &Path,
    branch: &str,
    message: Option<&str>,
) -> Result<String, String> {
    // Project pre_merge hook (`.dmux-hooks/pre_merge`): a nonzero exit vetoes
    // the merge, matching the TS hook contract.
    let hook = root.join(".dmux-hooks").join("pre_merge");
    if hook.is_file() {
        let target = current_branch(root).unwrap_or_default();
        let status = std::process::Command::new(&hook)
            .current_dir(worktree)
            .env("DMUX_ROOT", root)
            .env("DMUX_WORKTREE_PATH", worktree)
            .env("DMUX_BRANCH", branch)
            .env("DMUX_TARGET_BRANCH", &target)
            .status();
        match status {
            Ok(st) if !st.success() => {
                return Err(format!(
                    "pre_merge hook vetoed the merge (exit {})",
                    st.code().unwrap_or(-1)
                ));
            }
            Err(err) => return Err(format!("pre_merge hook: {err}")),
            _ => {}
        }
    }
    if let Some(msg) = message {
        git(worktree, &["add", "-A"]).map_err(|e| format!("git add: {e}"))?;
        git(worktree, &["commit", "-m", msg]).map_err(|e| format!("git commit: {e}"))?;
    }
    let target = current_branch(root).unwrap_or_else(|| "HEAD".into());
    match git(root, &["merge", "--no-edit", branch]) {
        Ok(_) => {
            crate::hooks::run_blocking(
                root,
                "post_merge",
                root,
                &[
                    ("DMUX_BRANCH", branch.to_string()),
                    ("DMUX_TARGET_BRANCH", target.clone()),
                ],
            );
            Ok(target)
        }
        Err(err) => {
            // Leave no half-merged state behind.
            let _ = git(root, &["merge", "--abort"]);
            Err(format!("merge failed: {err}"))
        }
    }
}

/// Re-run a merge EXPECTING conflicts and leave the conflicted state in
/// place for interactive/agent resolution. Returns the conflicted files.
pub fn merge_leaving_conflicts(root: &Path, branch: &str) -> Result<Vec<String>, String> {
    // A clean merge is fine too (someone fixed it meanwhile).
    let merge = git(root, &["merge", "--no-edit", branch]);
    let conflicted = git(root, &["diff", "--name-only", "--diff-filter=U"])
        .map(|s| s.lines().map(String::from).collect::<Vec<_>>())
        .unwrap_or_default();
    match (merge, conflicted.is_empty()) {
        (Ok(_), _) => Ok(Vec::new()),
        (Err(_), false) => Ok(conflicted),
        (Err(err), true) => {
            let _ = git(root, &["merge", "--abort"]);
            Err(format!("merge failed without conflicts: {err}"))
        }
    }
}

/// Stage a resolved file and, once all are staged, commit the merge.
pub fn stage_file(root: &Path, file: &str) -> Result<(), String> {
    git(root, &["add", "--", file]).map(|_| ())
}

pub fn commit_merge(root: &Path) -> Result<(), String> {
    git(root, &["commit", "--no-edit"]).map(|_| ())
}

pub fn abort_merge(root: &Path) {
    let _ = git(root, &["merge", "--abort"]);
}

/// Remove a merged worktree and its branch. Best-effort.
pub fn cleanup_worktree(root: &Path, worktree: &Path, branch: &str) -> Result<(), String> {
    git(
        root,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    )
    .map_err(|e| format!("worktree remove: {e}"))?;
    let _ = git(root, &["branch", "-D", branch]);
    Ok(())
}

use std::path::PathBuf;

pub fn git_main_worktree_root(dir: &std::path::Path) -> Option<PathBuf> {
    let out = worktree_list(dir)?;
    if !out.status.success() {
        return None;
    }
    parse_worktree_list(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .next()
}

/// Return the registered worktree containing `dir`, if Git knows one.
pub fn git_worktree_root_for_path(dir: &std::path::Path) -> Option<PathBuf> {
    let target = std::fs::canonicalize(dir).ok()?;
    let output = worktree_list(dir)?;
    if !output.status.success() {
        return None;
    }
    parse_worktree_list(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .filter_map(|path| {
            let root = std::fs::canonicalize(path).ok()?;
            target.starts_with(&root).then_some(root)
        })
        .max_by_key(|path| path.components().count())
}

fn worktree_list(dir: &std::path::Path) -> Option<std::process::Output> {
    std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(dir)
        .output()
        .ok()
}

fn parse_worktree_list(raw: &str) -> Vec<PathBuf> {
    raw.lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_worktree_list;

    #[test]
    fn parses_registered_worktree_paths_in_git_order() {
        let paths = parse_worktree_list(
            "worktree /projects/dmux-rs\nHEAD abc\n\nworktree /projects/dmux-rs-wt/fix\nbranch refs/heads/fix\n",
        );
        assert_eq!(
            paths,
            vec![
                std::path::PathBuf::from("/projects/dmux-rs"),
                std::path::PathBuf::from("/projects/dmux-rs-wt/fix"),
            ]
        );
    }
}
