//! Read-only GitHub issue access through the user's authenticated gh CLI.
//!
//! dmux keeps no GitHub credentials. This module resolves a project's GitHub
//! remote, asks gh for its open issues, and converts the response into the
//! small data model needed by the sidebar and issue browser.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct RepoRef {
    pub slug: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub enum IssueLoadState {
    Loading {
        repository: Option<String>,
    },
    Loaded {
        repository: String,
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
        IssueLoadState::Loading { .. } => "loading…".into(),
        IssueLoadState::Loaded { issues, .. } => issue_count_label(issues.len()),
        IssueLoadState::Error { .. } => "issues unavailable".into(),
    }
}

pub fn refresh_issue_state(
    state: SharedIssueState,
    project_root: String,
    finished: impl FnOnce() + Send + 'static,
) {
    let repository = state.lock().ok().and_then(|current| match &*current {
        IssueLoadState::Loading { repository } | IssueLoadState::Error { repository, .. } => {
            repository.clone()
        }
        IssueLoadState::Loaded { repository, .. } => Some(repository.clone()),
    });
    if let Ok(mut current) = state.lock() {
        *current = IssueLoadState::Loading { repository };
    }
    tokio::task::spawn_blocking(move || {
        let next = match repository_for_dir(Path::new(&project_root)) {
            Ok(repository) => match fetch_open_issues(&repository) {
                Ok(issues) => IssueLoadState::Loaded {
                    repository: repository.slug,
                    issues,
                },
                Err(message) => IssueLoadState::Error {
                    repository: Some(repository.slug),
                    message,
                },
            },
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

/// Resolve the GitHub repository represented by a project's origin remote.
pub fn repository_for_dir(path: &Path) -> Result<RepoRef, String> {
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
    } else if let Some(rest) = strip_prefix_ascii_case(remote, "https://") {
        let (authority, path) = rest.split_once('/')?;
        if !authority.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    } else {
        return None;
    };

    parse_slug(path)
}

/// Retrieve open issues using the user's existing gh authentication.
pub fn fetch_open_issues(repo: &RepoRef) -> Result<Vec<GitHubIssue>, String> {
    let endpoint = format!("repos/{}/issues?state=open&per_page=100", repo.slug);
    let output = Command::new("gh")
        .args(["api", "--paginate", "--slurp", &endpoint])
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

    parse_issues_json(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the array returned by gh api --paginate --slurp.
///
/// GitHub returns pull requests from this endpoint as issue-shaped objects.
/// Their pull_request field identifies them, so those entries are excluded
/// from the issue browser.
pub fn parse_issues_json(json: &str) -> Result<Vec<GitHubIssue>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| format!("invalid issue JSON: {error}"))?;
    let entries = match value {
        Value::Array(pages) => pages,
        _ => return Err("issue JSON must be an array".to_owned()),
    };

    let mut issues = Vec::new();
    for page_or_issue in entries {
        match page_or_issue {
            Value::Array(page) => {
                for entry in page {
                    if let Some(issue) = parse_issue(entry)? {
                        issues.push(issue);
                    }
                }
            }
            entry => {
                if let Some(issue) = parse_issue(entry)? {
                    issues.push(issue);
                }
            }
        }
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
        number,
        title,
        url,
        labels,
        assignees,
        updated_at,
    }))
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
    use super::*;

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
}
