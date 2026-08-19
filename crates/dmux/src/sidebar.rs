//! Sidebar keyboard routing and navigation targets.

use crate::{keys, render};

/// Sidebar drag-reorder gesture (#26). A press arms a row; crossing onto a
/// different row begins reordering, and release commits or cancels it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SidebarDrag {
    Armed { src: usize, start_row: u16 },
    Reordering { src: usize, pointer_row: u16 },
}

impl SidebarDrag {
    pub(crate) fn motion(self, row: u16) -> Self {
        match self {
            Self::Armed { src, start_row } if row != start_row => Self::Reordering {
                src,
                pointer_row: row,
            },
            Self::Armed { .. } => self,
            Self::Reordering { src, .. } => Self::Reordering {
                src,
                pointer_row: row,
            },
        }
    }

    pub(crate) fn reordering(&self) -> Option<(usize, u16)> {
        match self {
            Self::Reordering { src, pointer_row } => Some((*src, *pointer_row)),
            _ => None,
        }
    }
}

/// What a key does while the sidebar owns the keyboard (#27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarKeyAction {
    /// A global binding handled by normal routing.
    PassThrough,
    /// An unknown key consumed by the sidebar.
    Ignore,
    Up,
    Down,
    Left,
    Right,
    Activate,
    Menu,
    Hide,
    Close,
    Issues,
    NewAgent,
    NewTerminal,
    LeaveFocus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectAction {
    Issues,
    NewAgent,
    NewTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectSelection {
    pub root: String,
    pub action: ProjectAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectActivation {
    Issues(String),
    NewAgent(String),
    NewTerminal(String),
}

impl ProjectSelection {
    fn new(root: String, issues_available: bool) -> Self {
        Self {
            root,
            action: if issues_available {
                ProjectAction::Issues
            } else {
                ProjectAction::NewAgent
            },
        }
    }

    fn step_action(&mut self, delta: i32, issues_available: bool) {
        let actions = if issues_available {
            &[
                ProjectAction::Issues,
                ProjectAction::NewAgent,
                ProjectAction::NewTerminal,
            ][..]
        } else {
            &[ProjectAction::NewAgent, ProjectAction::NewTerminal][..]
        };
        let position = actions
            .iter()
            .position(|action| *action == self.action)
            .unwrap_or(0);
        self.action = actions[(position as i32 + delta).rem_euclid(actions.len() as i32) as usize];
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarNavTarget {
    Pane(usize),
    Project(ProjectSelection),
}

pub(crate) fn nav_targets(groups: &[render::SidebarGroup]) -> Vec<SidebarNavTarget> {
    let mut targets = Vec::new();
    for group in groups {
        targets.extend(
            group
                .pane_indices
                .iter()
                .copied()
                .map(SidebarNavTarget::Pane),
        );
        targets.push(SidebarNavTarget::Project(ProjectSelection::new(
            group.root.clone(),
            !group.issue_label.is_empty(),
        )));
    }
    targets
}

pub(crate) fn step_vertical(
    groups: &[render::SidebarGroup],
    selected_pane: usize,
    selected_project: Option<&ProjectSelection>,
    delta: i32,
) -> Option<SidebarNavTarget> {
    let targets = nav_targets(groups);
    if targets.is_empty() {
        return None;
    }
    let position = targets
        .iter()
        .position(|target| match (target, selected_project) {
            (SidebarNavTarget::Pane(index), None) => *index == selected_pane,
            (SidebarNavTarget::Project(next), Some(current)) => next.root == current.root,
            _ => false,
        })
        .unwrap_or(0);
    let next = (position as i32 + delta).rem_euclid(targets.len() as i32) as usize;
    Some(targets[next].clone())
}

pub(crate) fn step_horizontal(
    selection: &mut ProjectSelection,
    groups: &[render::SidebarGroup],
    delta: i32,
) {
    let issues_available = project_has_issues(selection, groups);
    selection.step_action(delta, issues_available);
}

pub(crate) fn activation(selection: ProjectSelection) -> ProjectActivation {
    match selection.action {
        ProjectAction::Issues => ProjectActivation::Issues(selection.root),
        ProjectAction::NewAgent => ProjectActivation::NewAgent(selection.root),
        ProjectAction::NewTerminal => ProjectActivation::NewTerminal(selection.root),
    }
}

pub(crate) fn normalize_project_action(
    selection: &mut ProjectSelection,
    groups: &[render::SidebarGroup],
) {
    if selection.action == ProjectAction::Issues && !project_has_issues(selection, groups) {
        selection.action = ProjectAction::NewAgent;
    }
}

fn project_has_issues(selection: &ProjectSelection, groups: &[render::SidebarGroup]) -> bool {
    groups
        .iter()
        .find(|group| group.root == selection.root)
        .is_some_and(|group| !group.issue_label.is_empty())
}

pub(crate) fn key_action(key: &dmux_host::KeyEvent, keymap: &keys::Keymap) -> SidebarKeyAction {
    use dmux_host::KeyCode;
    if let Some(chord) = keys::event_chord(key) {
        if keymap.is_leader(&chord) || keymap.lookup(&chord).is_some() {
            return SidebarKeyAction::PassThrough;
        }
    }
    if !key.modifiers.is_empty() {
        return SidebarKeyAction::Ignore;
    }
    match key.key {
        KeyCode::UpArrow | KeyCode::Char('k') => SidebarKeyAction::Up,
        KeyCode::DownArrow | KeyCode::Char('j') => SidebarKeyAction::Down,
        KeyCode::LeftArrow => SidebarKeyAction::Left,
        KeyCode::RightArrow => SidebarKeyAction::Right,
        KeyCode::Enter => SidebarKeyAction::Activate,
        KeyCode::Char('m') | KeyCode::Char(' ') => SidebarKeyAction::Menu,
        KeyCode::Char('h') => SidebarKeyAction::Hide,
        KeyCode::Char('x') => SidebarKeyAction::Close,
        KeyCode::Char('i') => SidebarKeyAction::Issues,
        KeyCode::Char('n') => SidebarKeyAction::NewAgent,
        KeyCode::Char('t') => SidebarKeyAction::NewTerminal,
        KeyCode::Escape => SidebarKeyAction::LeaveFocus,
        _ => SidebarKeyAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use dmux_compositor::Color;

    use super::*;

    fn group(root: &str, panes: Vec<usize>, issue_label: &str) -> render::SidebarGroup {
        render::SidebarGroup {
            name: root.into(),
            root: root.into(),
            accent: Color::Default,
            accent_soft: Color::Default,
            pane_indices: panes,
            issue_label: issue_label.into(),
            active: false,
        }
    }

    #[test]
    fn vertical_navigation_enters_action_rows_and_clears_pane_selection() {
        let groups = vec![
            group("/active", vec![0], "2 issues"),
            group("/empty", vec![], ""),
        ];
        let project = step_vertical(&groups, 0, None, 1).unwrap();
        assert_eq!(
            project,
            SidebarNavTarget::Project(ProjectSelection {
                root: "/active".into(),
                action: ProjectAction::Issues,
            })
        );
        assert_eq!(
            step_vertical(
                &groups,
                0,
                match &project {
                    SidebarNavTarget::Project(selection) => Some(selection),
                    SidebarNavTarget::Pane(_) => None,
                },
                1,
            ),
            Some(SidebarNavTarget::Project(ProjectSelection {
                root: "/empty".into(),
                action: ProjectAction::NewAgent,
            }))
        );
    }

    #[test]
    fn horizontal_navigation_wraps_and_skips_unavailable_issues() {
        let groups = vec![
            group("/repo", vec![], "2 issues"),
            group("/plain", vec![], ""),
        ];
        let mut repo = ProjectSelection::new("/repo".into(), true);
        step_horizontal(&mut repo, &groups, -1);
        assert_eq!(repo.action, ProjectAction::NewTerminal);
        step_horizontal(&mut repo, &groups, 1);
        assert_eq!(repo.action, ProjectAction::Issues);

        let mut plain = ProjectSelection::new("/plain".into(), false);
        step_horizontal(&mut plain, &groups, -1);
        assert_eq!(plain.action, ProjectAction::NewTerminal);
        step_horizontal(&mut plain, &groups, 1);
        assert_eq!(plain.action, ProjectAction::NewAgent);
    }

    #[test]
    fn activation_retains_the_selected_action_and_project() {
        for (action, expected) in [
            (
                ProjectAction::Issues,
                ProjectActivation::Issues("/repo".into()),
            ),
            (
                ProjectAction::NewAgent,
                ProjectActivation::NewAgent("/repo".into()),
            ),
            (
                ProjectAction::NewTerminal,
                ProjectActivation::NewTerminal("/repo".into()),
            ),
        ] {
            assert_eq!(
                activation(ProjectSelection {
                    root: "/repo".into(),
                    action,
                }),
                expected
            );
        }
    }

    #[test]
    fn horizontal_arrows_route_to_sidebar_actions() {
        use dmux_host::{KeyCode, KeyEvent, Modifiers};

        let keymap = keys::Keymap::from_overrides(&Default::default());
        let event = |key| KeyEvent {
            key,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(
            key_action(&event(KeyCode::LeftArrow), &keymap),
            SidebarKeyAction::Left
        );
        assert_eq!(
            key_action(&event(KeyCode::RightArrow), &keymap),
            SidebarKeyAction::Right
        );
        for (key, action) in [
            ('i', SidebarKeyAction::Issues),
            ('n', SidebarKeyAction::NewAgent),
            ('t', SidebarKeyAction::NewTerminal),
        ] {
            assert_eq!(key_action(&event(KeyCode::Char(key)), &keymap), action);
        }
    }
}
