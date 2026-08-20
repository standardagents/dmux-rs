use std::path::Path;

use dmux_compositor::Rect;

use crate::layout::{GUTTER, TITLE_ROWS};
use crate::session::LogicalPane;

/// Filesystem path used by pane-scoped actions such as copy path and editor.
pub fn path(pane: &LogicalPane, session_root: &Path) -> String {
    resolve_path(
        pane.worktree_path.as_deref(),
        pane.project_root.as_deref(),
        session_root,
    )
}

fn resolve_path(worktree: Option<&str>, project: Option<&str>, session_root: &Path) -> String {
    worktree
        .or(project)
        .map(str::to_owned)
        .unwrap_or_else(|| session_root.to_string_lossy().into_owned())
}

/// Menu-launched utility surfaces (#104): the editor, hook, and PR panes
/// dmux opens as temporary UI from menu actions. Their red close dot
/// dismisses in one action — a transient surface needs no confirmation.
/// Agents and terminals (user workspaces) keep the confirmed close.
pub fn is_transient_surface(slug: &str) -> bool {
    ["editor-", "hook-", "pr-"].iter().any(|prefix| {
        slug.strip_prefix(prefix)
            .is_some_and(|n| n.parse::<u32>().is_ok())
    })
}

/// The command the red title-bar close dot issues for this pane (#104).
pub fn title_close_cmd(pane: &LogicalPane, idx: usize) -> crate::views::AppCmd {
    if is_transient_surface(&pane.slug) {
        crate::views::AppCmd::ClosePane(idx)
    } else {
        crate::views::AppCmd::ConfirmClose(idx)
    }
}

/// Pane chrome and terminal body retained at full color under its flyout.
pub fn surface_rect(body: Rect) -> Rect {
    let title_rows = TITLE_ROWS.min(body.y);
    Rect::new(
        body.x,
        body.y - title_rows,
        body.w.saturating_add(GUTTER),
        body.h.saturating_add(title_rows),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_surfaces_close_without_confirmation() {
        // Menu-launched utility panes: one-click close.
        assert!(is_transient_surface("editor-1"));
        assert!(is_transient_surface("hook-2"));
        assert!(is_transient_surface("pr-10"));
        // User workspaces keep the confirmation, including slugs that
        // merely resemble the utility families.
        assert!(!is_transient_surface("terminal-1"));
        assert!(!is_transient_surface("my-agent"));
        assert!(!is_transient_surface("editor-notes"));
        assert!(!is_transient_surface("pr-review-agent"));
    }

    #[test]
    fn pane_action_path_prefers_worktree_then_owning_project() {
        let fallback = Path::new("/session");
        assert_eq!(
            resolve_path(Some("/worktree"), Some("/project"), fallback),
            "/worktree"
        );
        assert_eq!(resolve_path(None, Some("/project"), fallback), "/project");
        assert_eq!(resolve_path(None, None, fallback), "/session");
    }

    #[test]
    fn pane_surface_includes_title_and_right_border() {
        assert_eq!(
            surface_rect(Rect::new(41, 1, 62, 39)),
            Rect::new(41, 0, 63, 40)
        );
    }
}
