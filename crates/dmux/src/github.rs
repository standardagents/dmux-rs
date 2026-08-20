//! Read-only GitHub issue access through the user's authenticated gh CLI.
//!
//! dmux keeps no GitHub credentials. This module resolves a project's GitHub
//! remote, asks gh for its open issues, and converts the response into the
//! small data model needed by the sidebar and issue browser.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct RepoRef {
    pub slug: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GitHubIssue {
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IssueSection {
    Yours,
    AssignedTo(Vec<String>),
    Unassigned,
}

impl IssueSection {
    pub fn label(&self) -> String {
        match self {
            Self::Yours => "Yours".to_owned(),
            Self::AssignedTo(logins) => format!("@{}", logins.join(", @")),
            Self::Unassigned => "Unassigned".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum IssueLoadState {
    Unavailable,
    Loading {
        repository: Option<String>,
    },
    Loaded {
        repository: String,
        viewer_login: String,
        issues: Vec<GitHubIssue>,
    },
    Error {
        repository: Option<String>,
        message: String,
    },
}

pub type SharedIssueState = Arc<Mutex<IssueLoadState>>;

pub fn issue_state_label(state: Option<&SharedIssueState>) -> String {
    let Some(state) = state else {
        return "loading…".into();
    };
    let Ok(state) = state.lock() else {
        return "issues unavailable".into();
    };
    match &*state {
        IssueLoadState::Unavailable => String::new(),
        IssueLoadState::Loading { .. } => "loading…".into(),
        IssueLoadState::Loaded { issues, .. } => issue_count_label(
            issues
                .iter()
                .filter(|issue| issue.assignees.is_empty())
                .count(),
        ),
        IssueLoadState::Error { .. } => "issues unavailable".into(),
    }
}

pub fn refresh_issue_state(
    state: SharedIssueState,
    project_root: String,
    finished: impl FnOnce() + Send + 'static,
) {
    let repository = state.lock().ok().and_then(|current| match &*current {
        IssueLoadState::Unavailable => None,
        IssueLoadState::Loading { repository } | IssueLoadState::Error { repository, .. } => {
            repository.clone()
        }
        IssueLoadState::Loaded { repository, .. } => Some(repository.clone()),
    });
    if let Ok(mut current) = state.lock() {
        *current = IssueLoadState::Loading { repository };
    }
    tokio::task::spawn_blocking(move || {
        let next = match repositories_for_project(Path::new(&project_root)) {
            Ok(repositories) if repositories.is_empty() => IssueLoadState::Unavailable,
            Ok(repositories) => {
                let label = repository_label(&repositories);
                match authenticated_viewer_login() {
                    Ok(viewer_login) => {
                        let mut issues = Vec::new();
                        let mut error = None;
                        for repository in &repositories {
                            match fetch_open_issues(repository) {
                                Ok(mut repository_issues) => issues.append(&mut repository_issues),
                                Err(message) => {
                                    error = Some(format!("{}: {message}", repository.slug));
                                    break;
                                }
                            }
                        }
                        match error {
                            Some(message) => IssueLoadState::Error {
                                repository: Some(label),
                                message,
                            },
                            None => {
                                prepare_issues_for_view(&mut issues, &viewer_login);
                                IssueLoadState::Loaded {
                                    repository: label,
                                    viewer_login,
                                    issues,
                                }
                            }
                        }
                    }
                    Err(message) => IssueLoadState::Error {
                        repository: Some(label),
                        message,
                    },
                }
            }
            Err(message) => IssueLoadState::Error {
                repository: None,
                message,
            },
        };
        if let Ok(mut current) = state.lock() {
            *current = next;
        }
        finished();
    });
}

/// Resolve the project root and nested repositories to unique GitHub remotes.
pub fn repositories_for_project(path: &Path) -> Result<Vec<RepoRef>, String> {
    if !has_git_marker(path) {
        return Ok(Vec::new());
    }

    let mut directories = Vec::new();
    discover_nested_repository_dirs(path, &mut directories)?;

    let mut seen = BTreeSet::new();
    let mut repositories = Vec::new();
    if let Ok(repository) = repository_for_dir(path) {
        seen.insert(repository.slug.clone());
        repositories.push(repository);
    }
    for directory in directories {
        if let Ok(repository) = repository_for_dir(&directory) {
            if seen.insert(repository.slug.clone()) {
                repositories.push(repository);
            }
        }
    }
    Ok(repositories)
}

/// Resolve the GitHub repository represented by a project's origin remote.
pub fn repository_for_dir(path: &Path) -> Result<RepoRef, String> {
    if !has_git_marker(path) {
        return Err("project directory is not a Git repository root".to_owned());
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            "git could not read the origin remote".to_owned()
        } else {
            format!("git could not read the origin remote: {detail}")
        });
    }

    let remote = String::from_utf8_lossy(&output.stdout);
    parse_remote_url(remote.trim())
        .ok_or_else(|| "the origin remote is not a supported GitHub URL".to_owned())
}

/// Parse a GitHub remote in SSH SCP, SSH URL, or HTTPS URL form.
pub fn parse_remote_url(remote: &str) -> Option<RepoRef> {
    let remote = remote.trim();
    let path = if let Some(path) = remote.strip_prefix("git@github.com:") {
        path
    } else if let Some(rest) = strip_prefix_ascii_case(remote, "ssh://") {
        let (authority, path) = rest.split_once('/')?;
        if !authority.eq_ignore_ascii_case("git@github.com") {
            return None;
        }
        path
    } else {
        let rest = strip_prefix_ascii_case(remote, "https://")?;
        let (authority, path) = rest.split_once('/')?;
        if !authority.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };

    parse_slug(path)
}

/// Retrieve open issues using the user's existing gh authentication.
pub fn fetch_open_issues(repo: &RepoRef) -> Result<Vec<GitHubIssue>, String> {
    let output = issue_api_command(repo)
        .output()
        .map_err(|error| format!("could not run gh: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            "gh could not retrieve open issues".to_owned()
        } else {
            format!("gh could not retrieve open issues: {detail}")
        });
    }

    let mut issues = parse_issues_json(&String::from_utf8_lossy(&output.stdout))?;
    for issue in &mut issues {
        issue.repository.clone_from(&repo.slug);
    }
    Ok(issues)
}

/// Parse the sequential JSON arrays returned by gh api --paginate.
///
/// GitHub returns pull requests from this endpoint as issue-shaped objects.
/// Their pull_request field identifies them, so those entries are excluded
/// from the issue browser.
pub fn parse_issues_json(json: &str) -> Result<Vec<GitHubIssue>, String> {
    let mut issues = Vec::new();
    let mut pages = serde_json::Deserializer::from_str(json).into_iter::<Value>();
    let mut found_page = false;
    for page in &mut pages {
        found_page = true;
        let page = page.map_err(|error| format!("invalid issue JSON: {error}"))?;
        let Value::Array(entries) = page else {
            return Err("issue JSON pages must be arrays".to_owned());
        };
        for entry in entries {
            parse_issue_entry(entry, &mut issues)?;
        }
    }
    if !found_page {
        return Err("issue JSON must contain an array".to_owned());
    }
    Ok(issues)
}

/// Format the project-level issue count shown in the sidebar.
pub fn issue_count_label(count: usize) -> String {
    if count == 1 {
        "1 issue".to_owned()
    } else {
        format!("{count} issues")
    }
}

pub fn issue_section(issue: &GitHubIssue, viewer_login: &str) -> IssueSection {
    if issue
        .assignees
        .iter()
        .any(|login| login.eq_ignore_ascii_case(viewer_login))
    {
        IssueSection::Yours
    } else if issue.assignees.is_empty() {
        IssueSection::Unassigned
    } else {
        let mut logins = issue.assignees.clone();
        logins.sort_by_key(|login| login.to_ascii_lowercase());
        IssueSection::AssignedTo(logins)
    }
}

fn prepare_issues_for_view(issues: &mut [GitHubIssue], viewer_login: &str) {
    issues.sort_by(|left, right| {
        issue_section_sort_key(left, viewer_login)
            .cmp(&issue_section_sort_key(right, viewer_login))
            .then_with(|| left.repository.cmp(&right.repository))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.number.cmp(&right.number))
    });
}

fn issue_section_sort_key(issue: &GitHubIssue, viewer_login: &str) -> (u8, String) {
    match issue_section(issue, viewer_login) {
        IssueSection::Yours => (0, String::new()),
        IssueSection::AssignedTo(logins) => (1, logins.join("\0").to_ascii_lowercase()),
        IssueSection::Unassigned => (2, String::new()),
    }
}

fn authenticated_viewer_login() -> Result<String, String> {
    let output = viewer_api_command()
        .output()
        .map_err(|error| format!("could not run gh: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            "gh could not identify the authenticated user".to_owned()
        } else {
            format!("gh could not identify the authenticated user: {detail}")
        });
    }
    let login = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if login.is_empty() {
        return Err("gh returned an empty authenticated user login".to_owned());
    }
    Ok(login)
}

fn parse_issue(value: Value) -> Result<Option<GitHubIssue>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "issue JSON entries must be objects".to_owned())?;

    if object.contains_key("pull_request") {
        return Ok(None);
    }

    let number = object
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| "issue is missing a numeric number".to_owned())?;
    let title = required_string(object, "title")?;
    let url = required_string(object, "html_url")?;
    let updated_at = required_string(object, "updated_at")?;
    let labels = named_values(object, "labels", "name")?;
    let assignees = named_values(object, "assignees", "login")?;

    Ok(Some(GitHubIssue {
        repository: String::new(),
        number,
        title,
        url,
        labels,
        assignees,
        updated_at,
    }))
}

fn repository_label(repositories: &[RepoRef]) -> String {
    if repositories.len() == 1 {
        repositories[0].slug.clone()
    } else {
        format!("{} repositories", repositories.len())
    }
}

fn discover_nested_repository_dirs(
    directory: &Path,
    repositories: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not scan {}: {error}", directory.display()))?;
    let mut children = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not scan {}: {error}", directory.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() && !file_type.is_symlink() && should_scan_directory(&entry.path()) {
            children.push(entry.path());
        }
    }
    children.sort();

    for child in children {
        if has_git_marker(&child) {
            repositories.push(child);
        } else {
            discover_nested_repository_dirs(&child, repositories)?;
        }
    }
    Ok(())
}

fn has_git_marker(path: &Path) -> bool {
    fs::symlink_metadata(path.join(".git")).is_ok()
}

fn should_scan_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    !name.starts_with('.') && !matches!(name, "node_modules" | "target")
}

fn issue_api_command(repo: &RepoRef) -> Command {
    let endpoint = format!("repos/{}/issues?state=open&per_page=100", repo.slug);
    let mut command = Command::new("gh");
    command.args(["api", "--paginate"]).arg(endpoint);
    command
}

fn viewer_api_command() -> Command {
    let mut command = Command::new("gh");
    command.args(["api", "user", "--jq", ".login"]);
    command
}

fn parse_issue_entry(value: Value, issues: &mut Vec<GitHubIssue>) -> Result<(), String> {
    if let Value::Array(entries) = value {
        for entry in entries {
            parse_issue_entry(entry, issues)?;
        }
    } else if let Some(issue) = parse_issue(value)? {
        issues.push(issue);
    }
    Ok(())
}

fn required_string(object: &serde_json::Map<String, Value>, field: &str) -> Result<String, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("issue is missing string field {field:?}"))
}

fn named_values(
    object: &serde_json::Map<String, Value>,
    field: &str,
    name_field: &str,
) -> Result<Vec<String>, String> {
    let values = object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("issue is missing array field {field:?}"))?;
    values
        .iter()
        .map(|value| {
            value
                .get(name_field)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    format!("issue {field:?} entry is missing string field {name_field:?}")
                })
        })
        .collect()
}

fn parse_slug(path: &str) -> Option<RepoRef> {
    if path.contains('?') || path.contains('#') || path.chars().any(char::is_whitespace) {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/');
    let owner = segments.next()?;
    let repository = segments.next()?;
    if segments.next().is_some() || owner.is_empty() || repository.is_empty() {
        return None;
    }
    Some(RepoRef {
        slug: format!("{owner}/{repository}"),
    })
}

fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;

    struct TestTree(PathBuf);

    static NEXT_TEST_TREE: AtomicU64 = AtomicU64::new(0);

    impl TestTree {
        fn new() -> Self {
            let process = std::process::id();
            loop {
                let sequence = NEXT_TEST_TREE.fetch_add(1, Ordering::Relaxed);
                let path =
                    std::env::temp_dir().join(format!("dmux-github-test-{process}-{sequence}"));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create test tree {}: {error}", path.display()),
                }
            }
        }

        fn init_repo(&self, relative: &str, remote: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).unwrap();
            assert!(Command::new("git")
                .args(["init", "-q"])
                .arg(&path)
                .status()
                .unwrap()
                .success());
            assert!(Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["remote", "add", "origin", remote])
                .status()
                .unwrap()
                .success());
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn concurrent_test_trees_have_distinct_owned_directories() {
        let worker_count = 16;
        let barrier = Arc::new(Barrier::new(worker_count));
        let workers: Vec<_> = (0..worker_count)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    TestTree::new()
                })
            })
            .collect();
        let mut trees: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let mut paths: Vec<_> = trees.iter().map(|tree| tree.0.clone()).collect();
        paths.sort();
        paths.dedup();

        assert_eq!(paths.len(), worker_count);
        assert!(paths.iter().all(|path| path.is_dir()));
        let removed = trees[0].0.clone();
        drop(trees.remove(0));
        assert!(!removed.exists());
        assert!(trees.iter().all(|tree| tree.0.is_dir()));
    }

    #[test]
    fn parses_supported_remote_forms() {
        let cases = [
            "git@github.com:standardagents/dmux-rs.git",
            "ssh://git@github.com/standardagents/dmux-rs.git",
            "https://github.com/standardagents/dmux-rs.git",
            "https://GITHUB.COM/standardagents/dmux-rs",
        ];
        for remote in cases {
            assert_eq!(
                parse_remote_url(remote),
                Some(RepoRef {
                    slug: "standardagents/dmux-rs".to_owned()
                })
            );
        }
    }

    #[test]
    fn rejects_non_github_and_malformed_remotes() {
        for remote in [
            "git@gitlab.com:standardagents/dmux-rs.git",
            "ssh://git@gitlab.com/standardagents/dmux-rs.git",
            "https://github.example.com/standardagents/dmux-rs.git",
            "https://github.com/standardagents/dmux-rs/issues",
            "https://github.com/standardagents",
            "github.com:standardagents/dmux-rs.git",
            "git@github.com:/dmux-rs.git",
            "https://github.com/standardagents/dmux-rs.git?state=open",
        ] {
            assert_eq!(parse_remote_url(remote), None, "{remote}");
        }
    }

    #[test]
    fn formats_singular_and_plural_counts() {
        assert_eq!(issue_count_label(0), "0 issues");
        assert_eq!(issue_count_label(1), "1 issue");
        assert_eq!(issue_count_label(32), "32 issues");
        let unavailable = Arc::new(Mutex::new(IssueLoadState::Unavailable));
        assert_eq!(issue_state_label(Some(&unavailable)), "");
    }

    #[test]
    fn sidebar_count_includes_only_unassigned_issues() {
        let state = Arc::new(Mutex::new(IssueLoadState::Loaded {
            repository: "owner/repo".into(),
            viewer_login: "andrew".into(),
            issues: vec![
                test_issue(1, &[]),
                test_issue(2, &["andrew"]),
                test_issue(3, &["someone-else"]),
            ],
        }));

        assert_eq!(issue_state_label(Some(&state)), "1 issue");
    }

    #[test]
    fn discovers_nested_repositories_and_deduplicates_remotes() {
        let tree = TestTree::new();
        tree.init_repo("", "git@github.com:standardagents/coordinator.git");
        tree.init_repo(
            "group/agentbuilder",
            "git@github.com:standardagents/agentbuilder.git",
        );
        tree.init_repo(
            "agentbuilder-copy",
            "https://github.com/standardagents/agentbuilder.git",
        );
        tree.init_repo(
            ".dmux/worktrees/agentbuilder",
            "git@github.com:standardagents/ignored-worktree.git",
        );
        tree.init_repo(
            "node_modules/dependency",
            "git@github.com:standardagents/ignored-dependency.git",
        );
        tree.init_repo(
            "target/generated",
            "git@github.com:standardagents/ignored-build.git",
        );

        let repositories = repositories_for_project(&tree.0).unwrap();
        let slugs: Vec<_> = repositories
            .iter()
            .map(|repository| repository.slug.as_str())
            .collect();
        assert_eq!(
            slugs,
            ["standardagents/coordinator", "standardagents/agentbuilder"]
        );
        assert_eq!(repository_label(&repositories), "2 repositories");
    }

    #[test]
    fn plain_parent_does_not_inherit_nested_repository_issues() {
        let tree = TestTree::new();
        tree.init_repo(
            "group/agentbuilder",
            "git@github.com:standardagents/agentbuilder.git",
        );

        assert_eq!(repositories_for_project(&tree.0), Ok(Vec::new()));
    }

    #[test]
    fn nested_directory_does_not_inherit_ancestor_remote() {
        let tree = TestTree::new();
        tree.init_repo("", "git@github.com:standardagents/coordinator.git");
        let nested = tree.0.join("plain-directory");
        fs::create_dir(&nested).unwrap();

        assert!(repository_for_dir(&nested).is_err());
    }

    #[test]
    fn unsupported_repository_sources_produce_no_github_repositories() {
        let tree = TestTree::new();
        tree.init_repo("", "git@gitlab.com:standardagents/coordinator.git");
        tree.init_repo("child", "https://example.com/standardagents/child.git");
        assert_eq!(repositories_for_project(&tree.0), Ok(Vec::new()));
    }

    #[test]
    fn parses_slurped_pages_and_filters_pull_requests() {
        let json = r#"[
          [{
            "number": 7,
            "title": "Fix rendering",
            "html_url": "https://github.com/standardagents/dmux-rs/issues/7",
            "labels": [{"name": "bug"}, {"name": "render"}],
            "assignees": [{"login": "andrew-boyd"}],
            "updated_at": "2026-08-19T20:00:00Z"
          }],
          [{
            "number": 8,
            "title": "A pull request",
            "pull_request": {"url": "https://api.github.com/repos/standardagents/dmux-rs/pulls/8"}
          }]
        ]"#;
        assert_eq!(
            parse_issues_json(json),
            Ok(vec![GitHubIssue {
                repository: String::new(),
                number: 7,
                title: "Fix rendering".to_owned(),
                url: "https://github.com/standardagents/dmux-rs/issues/7".to_owned(),
                labels: vec!["bug".to_owned(), "render".to_owned()],
                assignees: vec!["andrew-boyd".to_owned()],
                updated_at: "2026-08-19T20:00:00Z".to_owned(),
            }])
        );
    }

    #[test]
    fn parses_sequential_paginated_arrays() {
        let json = r#"[{"number":1,"title":"First","html_url":"https://github.com/o/r/issues/1","labels":[],"assignees":[],"updated_at":"today"}]
[{"number":2,"title":"Second","html_url":"https://github.com/o/r/issues/2","labels":[],"assignees":[],"updated_at":"today"}]"#;
        let issues = parse_issues_json(json).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].number, 1);
        assert_eq!(issues[1].number, 2);
    }

    #[test]
    fn uses_pagination_without_the_newer_slurp_flag() {
        let command = issue_api_command(&RepoRef {
            slug: "standardagents/dmux-rs".to_owned(),
        });
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "api",
                "--paginate",
                "repos/standardagents/dmux-rs/issues?state=open&per_page=100"
            ]
        );
    }

    #[test]
    fn identifies_the_viewer_through_gh_authentication() {
        let command = viewer_api_command();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["api", "user", "--jq", ".login"]);
    }

    #[test]
    fn orders_yours_other_assignees_and_unassigned() {
        let mut issues = vec![
            test_issue(1, &[]),
            test_issue(2, &["someone-else"]),
            test_issue(3, &["coauthor", "AnDrEw"]),
            test_issue(4, &["andrew"]),
        ];

        prepare_issues_for_view(&mut issues, "andrew");

        assert_eq!(
            issues.iter().map(|issue| issue.number).collect::<Vec<_>>(),
            [4, 3, 2, 1]
        );
        assert_eq!(issue_section(&issues[0], "andrew"), IssueSection::Yours);
        assert_eq!(
            issue_section(&issues[2], "andrew"),
            IssueSection::AssignedTo(vec!["someone-else".into()])
        );
        assert_eq!(
            issue_section(&issues[3], "andrew"),
            IssueSection::Unassigned
        );
    }

    #[test]
    fn parses_an_unpaginated_array_too() {
        let json = r#"[{"number":1,"title":"Issue","html_url":"https://github.com/o/r/issues/1","labels":[],"assignees":[],"updated_at":"today"}]"#;
        assert_eq!(parse_issues_json(json).unwrap().len(), 1);
    }

    #[test]
    fn rejects_invalid_issue_shapes() {
        assert!(parse_issues_json("{}").is_err());
        assert!(parse_issues_json("not json").is_err());
        assert!(parse_issues_json("[{\"number\":1}]").is_err());
    }

    fn test_issue(number: u64, assignees: &[&str]) -> GitHubIssue {
        GitHubIssue {
            repository: "owner/repo".into(),
            number,
            title: format!("Issue {number}"),
            url: format!("https://github.com/owner/repo/issues/{number}"),
            labels: Vec::new(),
            assignees: assignees.iter().map(|login| (*login).to_owned()).collect(),
            updated_at: format!("2026-08-19T20:00:0{number}Z"),
        }
    }
}
