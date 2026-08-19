//! Self-update ("HMR for the multiplexer"): first-party builds poll the
//! dmux-rs repo's latest release; a new tag is downloaded via `gh` (repo
//! access = distribution auth), atomically swapped over the running binary,
//! and the process re-execs itself in place. Because tmux owns the sessions,
//! the swap is a sub-second reattach + reseed — panes, agents, and layout
//! all survive. Local dev builds (empty DMUX_BUILD_TAG) never self-update.

use std::path::PathBuf;

pub const BUILD_TAG: &str = env!("DMUX_BUILD_TAG");
pub const GIT_SHA: &str = env!("DMUX_GIT_SHA");

/// Sidebar version line.
pub fn version_line() -> String {
    if BUILD_TAG.is_empty() {
        format!("dmux-rs dev ({GIT_SHA})")
    } else {
        format!("dmux-rs {BUILD_TAG}")
    }
}

pub fn enabled() -> bool {
    !BUILD_TAG.is_empty() && std::env::var("DMUX_NO_UPDATE").map(|v| v != "1").unwrap_or(true)
}

fn asset_name() -> String {
    format!("dmux-rs-{}-{}", std::env::consts::OS, std::env::consts::ARCH)
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

/// Download the platform asset for `tag` into a staging path next to the
/// current binary (same filesystem, so the final rename is atomic).
pub fn stage(repo: &str, tag: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let staging = exe.with_extension(format!("staged-{tag}"));
    let _ = std::fs::remove_file(&staging);
    let dir = staging.parent().ok_or("no parent dir")?.to_path_buf();
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
    let _ = dir;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(staging)
}

/// Swap the staged binary over the current executable. The running process
/// keeps its (unlinked) image; the caller then re-execs the new file.
pub fn apply(staged: &PathBuf) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Keep one rollback copy.
    let backup = exe.with_extension("previous");
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::rename(&exe, &backup);
    std::fs::rename(staged, &exe).map_err(|e| {
        // Restore on failure.
        let _ = std::fs::rename(&backup, &exe);
        format!("swap failed: {e}")
    })?;
    Ok(exe)
}

/// Replace this process with the (new) binary at `exe`, preserving args.
/// Only returns on error.
#[cfg(unix)]
pub fn reexec(exe: &PathBuf) -> String {
    use std::os::unix::process::CommandExt;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = std::process::Command::new(exe)
        .args(&args)
        .env("DMUX_JUST_UPDATED", "1")
        .exec();
    format!("exec failed: {err}")
}
