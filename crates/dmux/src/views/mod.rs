//! Overlay views: every modal surface (menus, settings, dialogs, the agent
//! allocator) is a `View` on the app's overlay stack, rendered above the pane
//! grid from `dmux-ui` components. This replaces all tmux `display-popup`
//! machinery from the TS implementation.

mod agent_select;
mod confirm;
mod input_view;
mod logs;
mod menu;
mod settings_view;
mod shortcuts;

pub use agent_select::AgentSelectView;
pub use confirm::ConfirmView;
pub use input_view::{InputPurpose, InputView};
pub use logs::LogsView;
pub use menu::{MenuItem, MenuView};
pub use settings_view::SettingsView;
pub use shortcuts::ShortcutsView;

use dmux_compositor::{CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{ClickMap, Theme};

/// Commands views hand back to the app loop for execution.
#[derive(Debug, Clone, PartialEq)]
pub enum AppCmd {
    Quit,
    FocusPane(usize),
    OpenPaneMenu,
    OpenSettings,
    OpenNewAgent,
    OpenShortcuts,
    OpenLogs,
    PromptRename(usize),
    ConfirmClose(usize),
    RenamePane { idx: usize, name: String },
    ToggleHidden(usize),
    ClosePane(usize),
    CopyPath(usize),
    OpenInEditor(usize),
    NewTerminal,
    /// Terminal in a specific directory (welcome-screen worktree cards).
    NewTerminalAt { path: String, name: String },
    /// Ask for a project path, then open it.
    PromptAddProject,
    OpenProjectAt(String),
    /// Reopen a worktree and resume its agent's most recent session.
    ResumeWorktree { path: String, slug: String, agent: String },
    /// Merge flow: entry point, then execution (message = commit-first), then
    /// post-merge cleanup.
    MergeStart(usize),
    MergeExec { slug: String, message: Option<String> },
    MergeCleanup { slug: String },
    LaunchAgents { prompt: String, allocations: Vec<(String, u8)>, mode: String },
    SetSetting { key: String, value: serde_json::Value, scope: dmux_core::SettingsScope },
}

/// What a view wants after handling an event.
pub enum ViewResult {
    Stay,
    Close,
    Push(Box<dyn View>),
    /// Execute and stay open (e.g. live setting changes).
    Cmd(AppCmd),
    /// Execute and close.
    CloseAnd(AppCmd),
}

/// Click targets across the whole composed frame. Views register
/// `Overlay(tag)` regions; the rest belong to the base scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickTarget {
    SidebarRow(usize),
    SidebarNewAgent,
    SidebarNewTerminal,
    SidebarNewProject,
    SidebarSettings,
    SidebarHelp,
    PaneBody(usize),
    PaneTitle(usize),
    TitleRename(usize),
    TitleHide(usize),
    TitleClose(usize),
    WelcomeCard(usize),
    Overlay(u64),
}

pub struct ViewCtx<'a> {
    pub theme: &'a Theme,
    /// Shared animation tick for view spinners (loading states).
    #[allow(dead_code)]
    pub anim: u64,
}

pub trait View {
    /// Draw into `buf` within `area` (the full host area — views place their
    /// own panel) and register click regions. Returns a hardware-cursor
    /// position when the view owns a focused text input.
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)>;

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult;

    fn on_click(&mut self, _tag: u64) -> ViewResult {
        ViewResult::Stay
    }

    fn on_wheel(&mut self, _delta: i32) -> ViewResult {
        ViewResult::Stay
    }

    /// Whether this view wants continuous animation frames (spinners).
    fn animating(&self) -> bool {
        false
    }
}

/// Shared key classification helpers for views.
pub mod vkeys {
    use dmux_host::{KeyCode, KeyEvent, Modifiers};
    use dmux_ui::InputKey;

    pub fn is_esc(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::Escape)
    }

    pub fn is_enter(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::Enter)
    }

    pub fn is_up(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::UpArrow) || (matches!(k.key, KeyCode::Char('k')) && k.modifiers.is_empty())
    }

    pub fn is_down(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::DownArrow) || (matches!(k.key, KeyCode::Char('j')) && k.modifiers.is_empty())
    }

    pub fn is_left(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::LeftArrow)
    }

    pub fn is_right(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::RightArrow)
    }

    pub fn is_tab(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::Tab)
    }

    pub fn is_space(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::Char(' '))
    }

    /// Map a key event to a text-input edit, if it is one. Arrow up/down and
    /// friends are intentionally NOT input keys so lists can keep them.
    pub fn as_input_key(k: &KeyEvent) -> Option<InputKey> {
        let ctrl = k.modifiers.contains(Modifiers::CTRL);
        let alt = k.modifiers.contains(Modifiers::ALT);
        Some(match (&k.key, ctrl, alt) {
            (KeyCode::Char('a'), true, _) => InputKey::Home,
            (KeyCode::Char('e'), true, _) => InputKey::End,
            (KeyCode::Char('u'), true, _) => InputKey::KillToStart,
            (KeyCode::Char('k'), true, _) => InputKey::KillToEnd,
            (KeyCode::Char('w'), true, _) => InputKey::DeleteWordBack,
            (KeyCode::Char(c), false, false) => InputKey::Char(*c),
            (KeyCode::LeftArrow, false, false) => InputKey::Left,
            (KeyCode::RightArrow, false, false) => InputKey::Right,
            (KeyCode::Home, ..) => InputKey::Home,
            (KeyCode::End, ..) => InputKey::End,
            (KeyCode::Backspace, false, false) => InputKey::Backspace,
            (KeyCode::Backspace, _, true) | (KeyCode::Backspace, true, _) => InputKey::DeleteWordBack,
            (KeyCode::Delete, ..) => InputKey::Delete,
            _ => return None,
        })
    }
}
