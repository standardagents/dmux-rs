//! Sidebar keyboard routing and navigation targets.

use crate::{keys, render};

/// What a key does while the sidebar owns the keyboard (#27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarKeyAction {
    /// A global binding handled by normal routing.
    PassThrough,
    /// An unknown key consumed by the sidebar.
    Ignore,
    Up,
    Down,
    Activate,
    Menu,
    Hide,
    Close,
    Issues,
    NewAgent,
    NewTerminal,
    LeaveFocus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarNavTarget {
    Pane(usize),
    Project(String),
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
        targets.push(SidebarNavTarget::Project(group.root.clone()));
    }
    targets
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
