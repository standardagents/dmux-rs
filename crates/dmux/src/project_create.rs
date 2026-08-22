//! Create-missing-project flow for **Add Project** (#129): when the picker's
//! typed destination does not exist, it offers an explicit "create project
//! at <path>" action. Confirming runs here — off the input path via
//! `spawn_blocking` — creating the directory chain, running `git init` with
//! structured arguments (never a shell), and reporting the outcome through
//! the app message loop so the picker can show exactly what failed. Rollback
//! removes only directories this operation created, deepest-first, with
//! `remove_dir` (which refuses non-empty directories), so pre-existing data
//! and anything a racing writer placed inside are never deleted.

use std::path::{Path, PathBuf};

/// The deepest ancestor of `path` (the path itself included) that exists on
/// disk, without following the missing tail. `/` terminates the walk.
pub fn deepest_existing(path: &Path) -> PathBuf {
    let mut p = path;
    loop {
        if p.symlink_metadata().is_ok() {
            return p.to_path_buf();
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return p.to_path_buf(),
        }
    }
}

/// Whether the picker may OFFER creation for `path`: it must not exist, its
/// deepest existing ancestor must be a directory (a file in the chain can
/// never become one), and it must name a real final component. The `Err`
/// carries the picker's inline message.
pub fn offerable(path: &Path) -> Result<(), String> {
    if path.symlink_metadata().is_ok() {
        return Err(format!("not a directory: {}", path.display()));
    }
    if path.file_name().is_none() {
        return Err(format!("invalid project path: {}", path.display()));
    }
    let anchor = deepest_existing(path);
    if !anchor.is_dir() {
        return Err(format!("not a directory: {}", anchor.display()));
    }
    Ok(())
}

/// Create `dest` (and missing parents) and initialize a Git repository with
/// the user's own `git init` defaults — no invented commit, README, remote,
/// or template. A destination that became a directory since the offer is
/// accepted as-is and never reinitialized. On init failure the created
/// chain is rolled back; whatever cannot be removed safely is named in the
/// error for recovery.
pub fn create_project(dest: &Path) -> Result<(), String> {
    create_project_with(dest, crate::git::init)
}

pub fn create_project_with(
    dest: &Path,
    init: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let anchor = deepest_existing(dest);
    if dest.symlink_metadata().is_ok() {
        if dest.is_dir() {
            return Ok(());
        }
        return Err(format!("not a directory: {}", dest.display()));
    }
    if !anchor.is_dir() {
        return Err(format!("not a directory: {}", anchor.display()));
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let Err(err) = init(dest) else {
        return Ok(());
    };
    match rollback_created(&anchor, dest) {
        None => Err(err),
        Some(left) => Err(format!(
            "{err} (partial project left at {})",
            left.display()
        )),
    }
}

/// Remove `dest` and its parents up to (not including) `anchor`,
/// deepest-first. Returns the first path that could not be removed —
/// `remove_dir` refuses non-empty directories, so contents survive.
fn rollback_created(anchor: &Path, dest: &Path) -> Option<PathBuf> {
    let mut p = dest;
    while p != anchor {
        if std::fs::remove_dir(p).is_err() {
            return Some(p.to_path_buf());
        }
        p = p.parent()?;
    }
    None
}

impl crate::App {
    pub(crate) fn start_project_create(&mut self, raw: String) {
        let dest = PathBuf::from(&raw);
        let tx = self.app_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = create_project(&dest);
            let _ = tx.send(crate::AppMsg::ProjectCreated {
                path: dest.to_string_lossy().into_owned(),
                result,
            });
        });
    }

    pub(crate) fn handle_project_created(&mut self, path: String, result: Result<(), String>) {
        match result {
            Ok(()) => {
                // The picker's job is done; registration and opening reuse
                // the normal added-project behavior.
                self.views.remove_path_picker();
                let _ = self.execute_cmd(crate::views::AppCmd::OpenProjectAt(path));
            }
            Err(msg) => {
                // The error belongs in the still-open picker so the user can
                // correct the path; fall back to a toast if it was dismissed.
                match self.views.path_picker_mut() {
                    Some(picker) => picker.creation_failed(&msg),
                    None => self.toast(format!("create project failed: {msg}")),
                }
            }
        }
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dmux-create-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn offers_only_creatable_missing_directories() {
        let root = temp_root("offer");
        // Missing (nested) path under an existing directory: offerable.
        assert!(offerable(&root.join("new")).is_ok());
        assert!(offerable(&root.join("a/b/c")).is_ok());
        // Existing file: never offered, and the error names it.
        std::fs::write(root.join("f.txt"), "x").unwrap();
        let err = offerable(&root.join("f.txt")).unwrap_err();
        assert!(err.contains("not a directory"), "{err}");
        // A file in the ancestor chain can never become a directory.
        let err = offerable(&root.join("f.txt/inside")).unwrap_err();
        assert!(err.contains("f.txt"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn creates_nested_directories_and_initializes_git() {
        let root = temp_root("happy");
        let dest = root.join("group/new proj");
        create_project(&dest).unwrap();
        assert!(dest.is_dir());
        assert!(dest.join(".git").exists(), "git init ran");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_directories_are_never_reinitialized() {
        let root = temp_root("existing");
        let dest = root.join("already");
        std::fs::create_dir_all(&dest).unwrap();
        create_project(&dest).unwrap();
        // Ok, but untouched: no repository was invented in it.
        assert!(!dest.join(".git").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn init_failure_rolls_back_only_created_directories() {
        let root = temp_root("rollback");
        std::fs::write(root.join("keep.txt"), "x").unwrap();
        let dest = root.join("a/b");
        let err = create_project_with(&dest, |_| Err("git init: boom".into())).unwrap_err();
        assert!(err.contains("boom"), "{err}");
        assert!(!err.contains("partial"), "clean rollback: {err}");
        assert!(!root.join("a").exists(), "created chain removed");
        assert!(
            root.join("keep.txt").exists(),
            "pre-existing data untouched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unremovable_partial_state_is_reported_not_deleted() {
        let root = temp_root("partial");
        let dest = root.join("proj");
        let err = create_project_with(&dest, |d| {
            std::fs::write(d.join("half-made"), "x").unwrap();
            Err("git init: died".into())
        })
        .unwrap_err();
        assert!(err.contains("partial project left at"), "{err}");
        assert!(
            dest.join("half-made").exists(),
            "rollback never deletes files"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn destination_occupied_by_a_file_is_an_error() {
        let root = temp_root("occupied");
        let dest = root.join("taken");
        std::fs::write(&dest, "x").unwrap();
        let err = create_project(&dest).unwrap_err();
        assert!(err.contains("not a directory"), "{err}");
        assert!(dest.is_file(), "file untouched");
        let _ = std::fs::remove_dir_all(&root);
    }
}
