//! Overlay views: every modal surface (menus, settings, dialogs, the agent
//! allocator) is a `View` on the app's overlay stack, rendered above the pane
//! grid from `dmux-ui` components. This replaces all tmux `display-popup`
//! machinery from the TS implementation.

mod agent_select;
mod agents_enabled;
mod confirm;
mod diff_view;
mod hooks_view;
mod infer_view;
mod input_view;
mod issues;
mod logs;
mod menu;
mod path_picker;
mod settings_view;
mod shortcuts;
mod sounds_view;

pub use agent_select::AgentSelectView;
pub use agents_enabled::EnabledAgentsView;
pub use confirm::ConfirmView;
pub use diff_view::DiffView;
pub use hooks_view::HooksView;
pub use infer_view::InferProvidersView;
pub use input_view::{InputPurpose, InputView};
pub use issues::IssueBrowserView;
pub use logs::LogsView;
pub use menu::{MenuItem, MenuView};
pub use path_picker::PathPickerView;
pub use settings_view::SettingsView;
pub use shortcuts::ShortcutsView;
pub use sounds_view::SoundsView;

use dmux_compositor::{CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{ClickMap, Theme};

/// Commands views hand back to the app loop for execution.
#[derive(Debug, Clone, PartialEq)]
pub enum AppCmd {
    Quit,
    FocusPane(usize),
    OpenPaneMenu,
    /// Execute the accepted session-recovery plans (#20).
    RestoreSession,
    /// Row-anchored pane-actions flyout from the sidebar (#14): opens beside
    /// the clicked row, without activating the pane.
    OpenPaneFlyout {
        idx: usize,
        y: u16,
    },
    OpenSettings,
    OpenNewAgent,
    /// Open the agent allocator for a specific sidebar project.
    OpenNewAgentAt {
        project_root: String,
    },
    ChooseAgentForIssues {
        project_root: String,
        prompt: String,
    },
    RefreshIssues {
        project_root: String,
    },
    OpenUrl(String),
    OpenShortcuts,
    OpenLogs,
    PromptRename(usize),
    ConfirmClose(usize),
    RenamePane {
        idx: usize,
        name: String,
    },
    ToggleHidden(usize),
    ClosePane(usize),
    CopyPath(usize),
    OpenInEditor(usize),
    NewTerminal,
    /// Create a terminal for a specific sidebar project.
    NewTerminalInProject {
        project_root: String,
    },
    /// Terminal in a specific directory (welcome-screen worktree cards).
    NewTerminalAt {
        path: String,
        name: String,
    },
    /// Ask for a project path, then open it.
    PromptAddProject,
    OpenProjectAt(String),
    /// Reopen a worktree and resume its agent's most recent session.
    ResumeWorktree {
        path: String,
        slug: String,
        agent: String,
    },
    /// Merge flow: entry point, then execution (message = commit-first), then
    /// post-merge cleanup.
    MergeStart(usize),
    MergeExec {
        slug: String,
        message: Option<String>,
    },
    MergeCleanup {
        slug: String,
    },
    /// Re-establish merge conflicts at the root and launch an agent to
    /// resolve them.
    ResolveConflicts {
        branch: String,
    },
    /// Auto-resolve the conflicts with the configured inference provider.
    AiMerge {
        branch: String,
    },
    Noop,
    SearchScrollback(String),
    /// Diff peek for a worktree pane.
    ShowDiff(usize),
    /// New worktree pane with the same agent + prompt as this one.
    DuplicatePane(usize),
    /// Run a project hook (`run_test` / `run_dev`) in a new terminal pane.
    RunHook {
        idx: usize,
        name: String,
    },
    /// Push the worktree branch and open `gh pr create` in a terminal pane.
    CreatePr(usize),
    LaunchAgents {
        prompt: String,
        allocations: Vec<(String, u8)>,
        mode: String,
        project_root: Option<String>,
    },
    SetSetting {
        key: String,
        value: serde_json::Value,
        scope: dmux_core::SettingsScope,
    },
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
    /// Execute after closing this view and its parent flow view.
    CloseTwoAnd(AppCmd),
}

/// Click targets across the whole composed frame. Views register
/// `Overlay(tag)` regions; the rest belong to the base scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickTarget {
    SidebarRow(usize),
    SidebarNewProject,
    SidebarSettings,
    SidebarHelp,
    /// The 🐛 issues chip: opens the newest filed issue in the browser.
    SidebarIssues,
    /// Per-project creation actions (index into the sidebar groups).
    SidebarGroupIssues(usize),
    SidebarGroupNewAgent(usize),
    SidebarGroupNewTerminal(usize),
    PaneBody(usize),
    PaneTitle(usize),
    TitleRename(usize),
    TitleHide(usize),
    TitleClose(usize),
    WelcomeCard(usize),
    Overlay(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuTarget {
    Pane(usize),
    SidebarPane(usize),
}

impl ClickTarget {
    pub fn context_menu(self) -> Option<ContextMenuTarget> {
        match self {
            Self::PaneBody(i)
            | Self::PaneTitle(i)
            | Self::TitleRename(i)
            | Self::TitleHide(i)
            | Self::TitleClose(i) => Some(ContextMenuTarget::Pane(i)),
            Self::SidebarRow(i) => Some(ContextMenuTarget::SidebarPane(i)),
            _ => None,
        }
    }

    pub fn is_hoverable(self) -> bool {
        !matches!(self, Self::PaneBody(_))
    }
}

pub fn hover_target(target: Option<ClickTarget>, overlay_open: bool) -> Option<ClickTarget> {
    target.filter(|target| {
        target.is_hoverable() && (!overlay_open || matches!(target, ClickTarget::Overlay(_)))
    })
}

pub fn update_hover(current: &mut Option<ClickTarget>, next: Option<ClickTarget>) -> bool {
    let Some(next) = next else {
        return false;
    };
    if *current == Some(next) {
        return false;
    }
    *current = Some(next);
    true
}

pub struct ViewCtx<'a> {
    pub theme: &'a Theme,
    /// Shared animation tick for view spinners (loading states).
    #[allow(dead_code)]
    pub anim: u64,
    pub hovered: Option<ClickTarget>,
}

impl ViewCtx<'_> {
    pub fn active_overlay(&self, tag: u64, selected: bool) -> bool {
        match self.hovered {
            Some(ClickTarget::Overlay(hovered)) => hovered == tag,
            _ => selected,
        }
    }

    pub fn hovered_overlay(&self, tag: u64) -> bool {
        self.hovered == Some(ClickTarget::Overlay(tag))
    }
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

    fn on_paste(&mut self, _text: &str) -> ViewResult {
        ViewResult::Stay
    }

    fn on_click(&mut self, _tag: u64) -> ViewResult {
        ViewResult::Stay
    }

    /// Update canonical keyboard selection from a pointer target. The tag is
    /// returned unchanged so controls with sub-targets, such as the agent
    /// allocator counters, retain their precise hover treatment.
    fn on_hover(&mut self, tag: u64) -> u64 {
        tag
    }

    fn on_wheel(&mut self, _delta: i32) -> ViewResult {
        ViewResult::Stay
    }

    /// A region the overlay scrim must leave undimmed (#16) — an anchored
    /// flyout returns its originating sidebar row so the pair reads as
    /// connected. Asked of the TOP view only, so a closed or replaced view
    /// can never leave a stale carve-out.
    fn scrim_exception(&self) -> Option<Rect> {
        None
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
        matches!(k.key, KeyCode::UpArrow)
            || (matches!(k.key, KeyCode::Char('k')) && k.modifiers.is_empty())
    }

    pub fn is_down(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::DownArrow)
            || (matches!(k.key, KeyCode::Char('j')) && k.modifiers.is_empty())
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
            (KeyCode::Backspace, _, true) | (KeyCode::Backspace, true, _) => {
                InputKey::DeleteWordBack
            }
            (KeyCode::Delete, ..) => InputKey::Delete,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod hover_tests {
    use super::*;

    #[test]
    fn overlay_hover_visually_overrides_keyboard_selection() {
        let theme = Theme::named("violet");
        let hovered = ViewCtx {
            theme: &theme,
            anim: 0,
            hovered: Some(ClickTarget::Overlay(7)),
        };
        assert!(hovered.active_overlay(7, false));
        assert!(!hovered.active_overlay(3, true));

        let keyboard = ViewCtx {
            hovered: None,
            ..hovered
        };
        assert!(keyboard.active_overlay(3, true));
    }

    #[test]
    fn hover_resolution_ignores_pane_bodies_and_covered_base_targets() {
        assert_eq!(hover_target(Some(ClickTarget::PaneBody(2)), false), None);
        assert_eq!(
            hover_target(Some(ClickTarget::SidebarSettings), false),
            Some(ClickTarget::SidebarSettings)
        );
        assert_eq!(hover_target(Some(ClickTarget::SidebarSettings), true), None);
        assert_eq!(
            hover_target(Some(ClickTarget::Overlay(9)), true),
            Some(ClickTarget::Overlay(9))
        );

        let mut current = Some(ClickTarget::Overlay(9));
        assert!(!update_hover(&mut current, Some(ClickTarget::Overlay(9))));
        assert!(!update_hover(&mut current, None));
        assert_eq!(current, Some(ClickTarget::Overlay(9)));
    }

    #[test]
    fn context_menus_exist_only_for_pane_surfaces() {
        for target in [
            ClickTarget::PaneBody(2),
            ClickTarget::PaneTitle(2),
            ClickTarget::TitleRename(2),
            ClickTarget::TitleHide(2),
            ClickTarget::TitleClose(2),
        ] {
            assert_eq!(target.context_menu(), Some(ContextMenuTarget::Pane(2)));
        }
        assert_eq!(
            ClickTarget::SidebarRow(4).context_menu(),
            Some(ContextMenuTarget::SidebarPane(4))
        );
        for target in [
            ClickTarget::SidebarSettings,
            ClickTarget::SidebarGroupNewAgent(0),
            ClickTarget::WelcomeCard(0),
            ClickTarget::Overlay(1),
        ] {
            assert_eq!(target.context_menu(), None);
        }
    }
}
