//! Self-update ("HMR for the multiplexer"): first-party builds poll the
//! dmux-rs repo's latest release; a new tag is downloaded via `gh` (repo
//! access = distribution auth), atomically swapped over the running binary,
//! and the process re-execs itself in place. Because tmux owns the sessions,
//! the swap is a sub-second reattach + reseed — panes, agents, and layout
//! all survive. Local dev builds (empty DMUX_BUILD_TAG) never self-update.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const BUILD_TAG: &str = env!("DMUX_BUILD_TAG");
pub const GIT_SHA: &str = env!("DMUX_GIT_SHA");

/// Human/build identity: release builds carry the exact source commit so
/// an installed binary can always be resolved to its snapshot (#80).
fn build_version(build_tag: &str, git_sha: &str) -> String {
    if build_tag.is_empty() {
        format!("dev ({git_sha})")
    } else {
        format!("{build_tag} ({git_sha})")
    }
}

/// Static build version used by Clap's command metadata.
pub fn cli_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| build_version(BUILD_TAG, GIT_SHA))
        .as_str()
}

/// Sidebar version line.
pub fn version_line() -> String {
    format!("dmux-rs {}", cli_version())
}

pub fn enabled() -> bool {
    !BUILD_TAG.is_empty()
        && std::env::var("DMUX_NO_UPDATE")
            .map(|v| v != "1")
            .unwrap_or(true)
}

/// Whether a GitHub release tag represents a newer stable release than the
/// tag embedded in the running executable.
pub fn is_newer_release(candidate: &str, current: &str) -> bool {
    fn parse(tag: &str) -> Option<(u64, u64, u64)> {
        let mut parts = tag.strip_prefix('v').unwrap_or(tag).split('.');
        let version = (
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        );
        parts.next().is_none().then_some(version)
    }

    parse(candidate)
        .zip(parse(current))
        .is_some_and(|(candidate, current)| candidate > current)
}

fn asset_name() -> String {
    format!(
        "dmux-rs-{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn gh(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("gh")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Latest release tag on the repo, if any.
pub fn latest_tag(repo: &str) -> Result<String, String> {
    gh(&[
        "api",
        &format!("repos/{repo}/releases/latest"),
        "-q",
        ".tag_name",
    ])
}

fn verify_release_candidate(candidate: &Path, tag: &str) -> Result<(), String> {
    let output = std::process::Command::new(candidate)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("could not run staged executable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "staged executable --version failed with {}",
            output.status
        ));
    }
    let reported = String::from_utf8(output.stdout)
        .map_err(|error| format!("staged executable reported an invalid version: {error}"))?;
    let reported = reported.trim();
    let expected = format!("dmux-rs {tag}");
    if reported == expected || reported.starts_with(&format!("{expected} (")) {
        Ok(())
    } else {
        Err(format!(
            "staged executable reported {reported:?}, expected {expected:?}"
        ))
    }
}

/// Download the platform asset for `tag` into a staging path next to the
/// current binary (same filesystem, so the final rename is atomic).
pub fn stage(repo: &str, tag: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let staging = exe.with_extension(format!("staged-{tag}"));
    let _ = std::fs::remove_file(&staging);
    staging.parent().ok_or("no parent dir")?;
    gh(&[
        "release",
        "download",
        tag,
        "-R",
        repo,
        "-p",
        &asset_name(),
        "-O",
        &staging.to_string_lossy(),
        "--clobber",
    ])?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    if let Err(error) = verify_release_candidate(&staging, tag) {
        return match std::fs::remove_file(&staging) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}; could not remove rejected staged executable: {cleanup_error}"
            )),
        };
    }
    Ok(staging)
}

/// Build an untagged release binary from a worktree for local prototyping.
/// The empty build tag keeps the candidate outside the published updater loop.
pub fn build_prototype(worktree: &Path) -> Result<PathBuf, String> {
    build_prototype_with_output(worktree, |_| {})
}

pub fn prototype_worktree(worktree: &Path) -> Result<PathBuf, String> {
    let worktree = std::fs::canonicalize(worktree)
        .map_err(|err| format!("prototype worktree is unavailable: {err}"))?;
    if !worktree.is_dir() {
        return Err(format!(
            "prototype path is not a directory: {}",
            worktree.display()
        ));
    }
    if !worktree.join("Cargo.toml").is_file() {
        return Err(format!(
            "prototype worktree has no Cargo.toml: {}",
            worktree.display()
        ));
    }
    Ok(worktree)
}

/// Build a local prototype while reporting Cargo's latest status line. All
/// worktrees share dependency artifacts, then receive their own executable so
/// the running binary still identifies its source worktree.
pub fn build_prototype_with_output(
    worktree: &Path,
    mut report: impl FnMut(String),
) -> Result<PathBuf, String> {
    let worktree = prototype_worktree(worktree)?;
    let target_dir = crate::dirs_home()
        .join(".dmux")
        .join("cache")
        .join("prototype-target");
    std::fs::create_dir_all(&target_dir)
        .map_err(|err| format!("could not create prototype build cache: {err}"))?;
    let mut command = std::process::Command::new("cargo");
    command
        .args(["build", "--release", "--bin", "dmux-rs", "--color", "never"])
        .current_dir(&worktree)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_INCREMENTAL", "1")
        .env_remove("DMUX_BUILD_TAG")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // setpriority is async-signal-safe on the supported Unix targets. A
        // denied priority change leaves the build usable at normal priority.
        unsafe {
            command.pre_exec(|| {
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                Ok(())
            });
        }
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not run cargo: {err}"))?;
    let mut last_line = String::new();
    if let Some(stderr) = child.stderr.take() {
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            let line = line.trim().to_string();
            if !line.is_empty() {
                last_line.clone_from(&line);
                report(line);
            }
        }
    }
    let status = child
        .wait()
        .map_err(|err| format!("could not wait for cargo: {err}"))?;
    if !status.success() {
        let detail = if last_line.is_empty() {
            String::new()
        } else {
            format!(": {last_line}")
        };
        return Err(format!(
            "prototype build failed in {}{detail}",
            worktree.display()
        ));
    }
    let built = target_dir.join("release/dmux-rs");
    if !built.is_file() {
        return Err(format!(
            "prototype build did not produce {}",
            built.display()
        ));
    }
    let output_dir = worktree.join("target/dmux-prototype");
    std::fs::create_dir_all(&output_dir)
        .map_err(|err| format!("could not create prototype output directory: {err}"))?;
    let executable = output_dir.join("dmux-rs");
    let staged = output_dir.join("dmux-rs.next");
    std::fs::copy(&built, &staged)
        .map_err(|err| format!("could not stage prototype executable: {err}"))?;
    std::fs::rename(&staged, &executable)
        .map_err(|err| format!("could not install prototype executable: {err}"))?;
    Ok(executable)
}

#[derive(Debug)]
pub struct AppliedUpdate {
    executable: PathBuf,
    backup: PathBuf,
}

#[derive(Debug)]
pub enum ReexecTarget {
    Update(AppliedUpdate),
    Prototype(PathBuf),
}

impl crate::App {
    /// Apply a deferred self-update once launch work reaches a safe boundary.
    pub(crate) fn try_apply_pending_update(&mut self) {
        let Some((_, _, since)) = &self.pending_update else {
            return;
        };
        let active = self.bootstraps.values().any(|ui| ui.done_at.is_none());
        if !crate::util::update_may_apply(active, self.pending_injection_count(), since.elapsed()) {
            return;
        }
        let (tag, staged, _) = self.pending_update.take().expect("update pending");
        match apply(&staged) {
            Ok(update) => {
                self.toast(format!("⬆ updating to {tag}…"));
                self.reexec_after = Some(ReexecTarget::Update(update));
                self.want_exit = true;
            }
            Err(error) => {
                tracing::warn!(%error, "update apply failed");
                self.toast(format!("update failed: {error}"));
            }
        }
    }
}

impl ReexecTarget {
    fn executable(&self) -> &Path {
        match self {
            Self::Update(update) => &update.executable,
            Self::Prototype(executable) => executable,
        }
    }
}

/// Swap the staged binary over the current executable while retaining the
/// prior executable for activation rollback.
pub fn apply(staged: &Path) -> Result<AppliedUpdate, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    apply_to(&exe, staged)
}

fn apply_to(exe: &Path, staged: &Path) -> Result<AppliedUpdate, String> {
    let backup = exe.with_extension("previous");
    if backup.exists() {
        std::fs::remove_file(&backup)
            .map_err(|error| format!("could not remove prior update backup: {error}"))?;
    }
    std::fs::rename(exe, &backup)
        .map_err(|error| format!("could not back up current executable: {error}"))?;
    if let Err(replacement_error) = std::fs::rename(staged, exe) {
        return match std::fs::rename(&backup, exe) {
            Ok(()) => Err(format!(
                "could not activate staged executable: {replacement_error}; previous executable restored"
            )),
            Err(restoration_error) => Err(format!(
                "could not activate staged executable: {replacement_error}; could not restore previous executable: {restoration_error}"
            )),
        };
    }
    Ok(AppliedUpdate {
        executable: exe.to_path_buf(),
        backup,
    })
}

fn rollback(update: &AppliedUpdate) -> Result<PathBuf, String> {
    if let Err(error) = std::fs::remove_file(&update.executable) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(format!(
                "could not remove failed update executable: {error}"
            ));
        }
    }
    std::fs::rename(&update.backup, &update.executable)
        .map_err(|error| format!("could not restore previous executable: {error}"))?;
    Ok(update.executable.clone())
}

fn recover_activation_with(
    update: &AppliedUpdate,
    activation_error: &str,
    relaunch_previous: impl FnOnce(&Path) -> String,
) -> String {
    match rollback(update) {
        Ok(previous) => {
            let relaunch_error = relaunch_previous(&previous);
            format!(
                "update activation failed: {activation_error}; previous executable restored; previous executable re-exec failed: {relaunch_error}"
            )
        }
        Err(restoration_error) => format!(
            "update activation failed: {activation_error}; rollback failed: {restoration_error}"
        ),
    }
}

/// Replace this process with the (new) binary at `exe`, preserving args.
/// Only returns on error.
#[cfg(unix)]
fn reexec(
    exe: &Path,
    renderer_token: &str,
    context: &crate::renderer_control::ReexecContext,
    default_executable: &Path,
    updated: bool,
) -> String {
    use std::os::unix::process::CommandExt;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = std::process::Command::new(exe);
    command
        .args(&args)
        .env(
            "DMUX_PROTOTYPE_DEFAULT_EXE",
            default_executable.to_string_lossy().as_ref(),
        )
        .env(
            crate::renderer_control::preserved_token_env(),
            renderer_token,
        )
        .env(
            crate::renderer_control::reexec_role_env(),
            match context.role {
                crate::renderer_control::ReexecRole::Controller => "controller",
                crate::renderer_control::ReexecRole::Follower => "follower",
            },
        )
        .env_remove(crate::renderer_control::reexec_owner_env());
    if updated {
        command.env("DMUX_JUST_UPDATED", "1");
    } else {
        command.env_remove("DMUX_JUST_UPDATED");
    }
    if let Some(owner) = &context.expected_owner {
        command.env(crate::renderer_control::reexec_owner_env(), owner);
    }
    let err = command.exec();
    format!("exec failed: {err}")
}

pub fn reexec_target(
    target: &ReexecTarget,
    renderer_token: &str,
    context: &crate::renderer_control::ReexecContext,
    default_executable: &Path,
) -> String {
    let activation_error = reexec(
        target.executable(),
        renderer_token,
        context,
        default_executable,
        true,
    );
    match target {
        ReexecTarget::Update(update) => {
            recover_activation_with(update, &activation_error, |previous| {
                reexec(previous, renderer_token, context, default_executable, false)
            })
        }
        ReexecTarget::Prototype(_) => activation_error,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_to, build_version, is_newer_release, recover_activation_with,
        verify_release_candidate,
    };
    use std::path::PathBuf;

    fn temp_dir(test: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "dmux-updater-{test}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn formats_release_and_development_builds() {
        assert_eq!(build_version("v1.2.3", "abc1234"), "v1.2.3 (abc1234)");
        assert_eq!(build_version("", "abc1234"), "dev (abc1234)");
    }

    #[test]
    fn compares_github_release_tags_against_the_embedded_build_tag() {
        assert!(is_newer_release("v0.35.0", "v0.34.4"));
        assert!(is_newer_release("v1.0.0", "v0.99.99"));
        assert!(!is_newer_release("v0.34.4", "v0.34.4"));
        assert!(!is_newer_release("v0.34.3", "v0.34.4"));
        assert!(!is_newer_release("latest", "v0.34.4"));
        assert!(!is_newer_release("v0.35", "v0.34.4"));
        assert!(!is_newer_release("v0.35.0", ""));
    }

    #[test]
    fn replacement_failure_restores_the_current_executable() {
        let dir = temp_dir("replace-failure");
        let executable = dir.join("dmux-rs");
        let missing_staged = dir.join("missing-staged");
        std::fs::write(&executable, "previous").unwrap();

        let error = apply_to(&executable, &missing_staged).unwrap_err();

        assert!(error.contains("previous executable restored"));
        assert_eq!(std::fs::read_to_string(&executable).unwrap(), "previous");
        assert!(!executable.with_extension("previous").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_failure_is_reported_before_replacement() {
        let dir = temp_dir("backup-failure");
        let executable = dir.join("dmux-rs");
        let staged = dir.join("dmux-rs.staged");
        let backup = executable.with_extension("previous");
        std::fs::write(&executable, "previous").unwrap();
        std::fs::write(&staged, "candidate").unwrap();
        std::fs::create_dir(&backup).unwrap();

        let error = apply_to(&executable, &staged).unwrap_err();

        assert!(error.contains("could not remove prior update backup"));
        assert_eq!(std::fs::read_to_string(&executable).unwrap(), "previous");
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), "candidate");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn activation_failure_restores_and_relaunches_the_previous_executable() {
        let dir = temp_dir("activation-failure");
        let executable = dir.join("dmux-rs");
        let staged = dir.join("dmux-rs.staged");
        std::fs::write(&executable, "previous").unwrap();
        std::fs::write(&staged, "candidate").unwrap();
        let update = apply_to(&executable, &staged).unwrap();
        let mut relaunched = false;

        let error = recover_activation_with(&update, "candidate exec failed", |previous| {
            relaunched = true;
            assert_eq!(std::fs::read_to_string(previous).unwrap(), "previous");
            "test relaunch returned".to_string()
        });

        assert!(relaunched);
        assert!(error.contains("previous executable restored"));
        assert!(error.contains("test relaunch returned"));
        assert_eq!(std::fs::read_to_string(&executable).unwrap(), "previous");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn staged_candidate_must_report_the_requested_release_tag() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("candidate-version");
        let candidate = dir.join("dmux-rs.staged");
        std::fs::write(
            &candidate,
            "#!/bin/sh\nprintf 'dmux-rs v2.3.4 (fixture)\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755)).unwrap();

        verify_release_candidate(&candidate, "v2.3.4").unwrap();
        let error = verify_release_candidate(&candidate, "v2.3.5").unwrap_err();
        assert!(error.contains("expected \"dmux-rs v2.3.5\""));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
