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
    git(path, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false)
}

pub fn current_branch(path: &Path) -> Option<String> {
    git(path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|b| b != "HEAD")
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
    if let Some(msg) = message {
        git(worktree, &["add", "-A"]).map_err(|e| format!("git add: {e}"))?;
        git(worktree, &["commit", "-m", msg]).map_err(|e| format!("git commit: {e}"))?;
    }
    let target = current_branch(root).unwrap_or_else(|| "HEAD".into());
    match git(root, &["merge", "--no-edit", branch]) {
        Ok(_) => Ok(target),
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

/// Remove a merged worktree and its branch. Best-effort.
pub fn cleanup_worktree(root: &Path, worktree: &Path, branch: &str) -> Result<(), String> {
    git(root, &["worktree", "remove", "--force", &worktree.to_string_lossy()])
        .map_err(|e| format!("worktree remove: {e}"))?;
    let _ = git(root, &["branch", "-D", branch]);
    Ok(())
}
