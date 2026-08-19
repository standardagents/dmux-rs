/// The encoded-title delimiter shared with the TS implementation
/// (`src/utils/paneTitle.ts`). A pane title is either a bare stable title or
/// `<display>__dmux__<stable>`; the stable title is `<slug>` for same-project
/// panes and `<slug>@<projectName>-<md5(projectRoot)[0:4]>` for cross-project.
pub const PANE_TITLE_DELIMITER: &str = "__dmux__";

const LEGACY_DELIMITERS: &[&str] = &["::dmux::"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneTitle {
    pub display: String,
    pub stable: String,
    /// Slug portion of the stable title (before any `@project-tag`).
    pub slug: String,
    /// Project tag (`name-hash4`) when the stable title is cross-project.
    pub project_tag: Option<String>,
}

/// Parse a tmux pane title into its identity parts. Titles with no dmux
/// delimiter are treated as stable == display (matches TS behavior where a
/// bare stable title is used when display == stable).
pub fn parse_pane_title(title: &str) -> PaneTitle {
    let (display, stable) = split_once_any(title)
        .map(|(d, s)| (d.to_string(), s.to_string()))
        .unwrap_or_else(|| (title.to_string(), title.to_string()));

    let (slug, project_tag) = match stable.split_once('@') {
        Some((slug, tag)) => (slug.to_string(), Some(tag.to_string())),
        None => (stable.clone(), None),
    };

    PaneTitle {
        display,
        stable,
        slug,
        project_tag,
    }
}

/// Encode a pane title per the shared contract: bare stable title when the
/// display name equals it, `<display>__dmux__<stable>` otherwise. The display
/// half is sanitized so it can never smuggle a delimiter.
pub fn encode_pane_title(display: &str, stable: &str) -> String {
    let mut display = display.replace(PANE_TITLE_DELIMITER, " ");
    for delim in LEGACY_DELIMITERS {
        display = display.replace(delim, " ");
    }
    let display = display.split_whitespace().collect::<Vec<_>>().join(" ");
    if display.is_empty() || display == stable {
        stable.to_string()
    } else {
        format!("{display}{PANE_TITLE_DELIMITER}{stable}")
    }
}

fn split_once_any(title: &str) -> Option<(&str, &str)> {
    if let Some(idx) = title.find(PANE_TITLE_DELIMITER) {
        return Some((&title[..idx], &title[idx + PANE_TITLE_DELIMITER.len()..]));
    }
    for delim in LEGACY_DELIMITERS {
        if let Some(idx) = title.find(delim) {
            return Some((&title[..idx], &title[idx + delim.len()..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_slug() {
        let t = parse_pane_title("fix-auth");
        assert_eq!(t.display, "fix-auth");
        assert_eq!(t.stable, "fix-auth");
        assert_eq!(t.slug, "fix-auth");
        assert_eq!(t.project_tag, None);
    }

    #[test]
    fn encoded_title() {
        let t = parse_pane_title("Fix authentication__dmux__fix-auth");
        assert_eq!(t.display, "Fix authentication");
        assert_eq!(t.slug, "fix-auth");
    }

    #[test]
    fn cross_project_stable() {
        let t = parse_pane_title("My pane__dmux__fix-auth@webapp-a1b2");
        assert_eq!(t.slug, "fix-auth");
        assert_eq!(t.project_tag.as_deref(), Some("webapp-a1b2"));
    }

    #[test]
    fn legacy_delimiter() {
        let t = parse_pane_title("Old::dmux::slug1");
        assert_eq!(t.display, "Old");
        assert_eq!(t.slug, "slug1");
    }
}
