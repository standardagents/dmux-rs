//! Automatic incident reporting: a shadow-verifier mismatch files a real
//! GitHub issue on the dmux-rs repo — description, both grids, and a secret
//! gist holding the full incident bundle (the byte stream is too big for an
//! issue body). First-party users have repo access, so plain `gh` auth is
//! the only credential needed. Filed issues are remembered locally
//! (`~/.dmux/issues-filed.json`) for the sidebar.

use std::path::{Path, PathBuf};

pub const DEFAULT_REPO: &str = "standardagents/dmux-rs";

/// One locally-remembered filed issue (sidebar rows).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FiledIssue {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub slug: String,
    pub filed_at: u64,
    pub build: String,
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join(".dmux").join("issues-filed.json")
}

pub fn load_filed(home: &Path) -> Vec<FiledIssue> {
    std::fs::read(state_path(home))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn append_filed(home: &Path, issue: &FiledIssue) {
    let mut all = load_filed(home);
    all.push(issue.clone());
    if let Ok(json) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::create_dir_all(home.join(".dmux"));
        let _ = std::fs::write(state_path(home), json);
    }
}

fn run_gh(args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("gh spawn: {e}"))?;
    if let Some(input) = stdin {
        use std::io::Write;
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(input.as_bytes());
        }
    } else {
        drop(child.stdin.take());
    }
    let out = child.wait_with_output().map_err(|e| format!("gh: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Compose the issue body: human description first, machine evidence after.
pub fn issue_body(
    build: &str,
    slug: &str,
    cols: u16,
    rows: u16,
    diffs: &[String],
    our_grid: &str,
    tmux_grid_escaped: &str,
    gist_url: &str,
    deterministic: bool,
) -> String {
    let clip = |s: &str, max: usize| -> String {
        if s.len() > max {
            format!("{}\n… (truncated)", &s[..max])
        } else {
            s.to_string()
        }
    };
    format!(
        "**Automatic render-divergence report** (shadow verifier)\n\n\
         The live dmux-rs grid diverged from tmux's authoritative grid for the same pane. \
         The full incident bundle (including the seed-anchored byte stream) is in the linked gist; \
         replay it offline with `dmux-rs --replay-incident <file>`.\n\n\
         | field | value |\n|---|---|\n\
         | build | `{build}` |\n| pane | `{slug}` ({cols}x{rows}) |\n\
         | differing cells | {n} |\n| deterministic replay | {det} |\n\n\
         **Incident bundle:** {gist_url}\n\n\
         <details><summary>First differing cells</summary>\n\n```\n{diffs}\n```\n</details>\n\n\
         <details><summary>Our grid (text)</summary>\n\n```\n{ours}\n```\n</details>\n\n\
         <details><summary>tmux grid (capture -epqN, escaped)</summary>\n\n```\n{tmux}\n```\n</details>\n\n\
         ---\nFix loop: `--replay-incident` to reproduce → patch → add the bundle to \
         `crates/dmux/tests/corpus/` → release. Filed automatically; deduped per pane per process lifetime.",
        build = build,
        slug = slug,
        cols = cols,
        rows = rows,
        n = diffs.len(),
        det = if deterministic { "yes" } else { "no (recording overflowed)" },
        gist_url = gist_url,
        diffs = clip(&diffs.join("\n"), 4_000),
        ours = clip(our_grid, 12_000),
        tmux = clip(tmux_grid_escaped, 12_000),
    )
}

pub struct Filed {
    pub issue: FiledIssue,
}

/// File the issue (blocking; call from spawn_blocking). `dry_run_dir` writes
/// the would-be gist + body to files instead of talking to GitHub (tests).
#[allow(clippy::too_many_arguments)]
pub fn file_issue(
    repo: &str,
    home: &Path,
    build: &str,
    slug: &str,
    cols: u16,
    rows: u16,
    diffs: &[String],
    our_grid: &str,
    tmux_grid_escaped: &str,
    incident_path: &Path,
    deterministic: bool,
    dry_run_dir: Option<&Path>,
) -> Result<Filed, String> {
    let title = format!(
        "render divergence: {slug} — {n} cells{first}",
        n = diffs.len(),
        first = diffs
            .first()
            .map(|d| format!(" (first at {})", d.split(' ').next().unwrap_or("")))
            .unwrap_or_default()
    );

    if let Some(dir) = dry_run_dir {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let body = issue_body(build, slug, cols, rows, diffs, our_grid, tmux_grid_escaped, "dry-run://gist", deterministic);
        std::fs::write(dir.join("issue-title.txt"), &title).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("issue-body.md"), &body).map_err(|e| e.to_string())?;
        let issue = FiledIssue {
            number: 0,
            title,
            url: "dry-run://issue".into(),
            slug: slug.to_string(),
            filed_at: now(),
            build: build.to_string(),
        };
        append_filed(home, &issue);
        return Ok(Filed { issue });
    }

    // 1. Secret gist with the full bundle.
    let gist_url = run_gh(
        &[
            "gist",
            "create",
            "--desc",
            &format!("dmux-rs render incident: {slug} ({build})"),
            &incident_path.to_string_lossy(),
        ],
        None,
    )?;
    let gist_url = gist_url.lines().last().unwrap_or("").trim().to_string();

    // 2. The issue itself.
    let body = issue_body(build, slug, cols, rows, diffs, our_grid, tmux_grid_escaped, &gist_url, deterministic);
    let issue_url = run_gh(
        &[
            "issue", "create", "-R", repo, "--title", &title, "--label", "render-incident", "--body", &body,
        ],
        None,
    )?;
    let issue_url = issue_url.lines().last().unwrap_or("").trim().to_string();
    let number = issue_url.rsplit('/').next().and_then(|n| n.parse().ok()).unwrap_or(0);

    let issue = FiledIssue {
        number,
        title,
        url: issue_url,
        slug: slug.to_string(),
        filed_at: now(),
        build: build.to_string(),
    };
    append_filed(home, &issue);
    Ok(Filed { issue })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
