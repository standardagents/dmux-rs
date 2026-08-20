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
#[cfg(test)]
mod issues_preview;
mod issues_table;
mod logs;
mod menu;
mod path_picker;
mod prototype_build;
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
pub use prototype_build::PrototypeBuildView;
pub use settings_view::SettingsView;
pub use shortcuts::ShortcutsView;
pub use sounds_view::SoundsView;

use dmux_compositor::{CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{ClickMap, Theme};

/// Stable naming supplied by a source flow such as the Issues pane. The
/// launch planner applies one base identity across pane, branch, and worktree
/// surfaces while retaining a reader-facing title for the pane toolbar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLaunchIdentity {
    pub slug: String,
    pub display: String,
}

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
        identity: AgentLaunchIdentity,
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
    /// Build and load a dmux-rs worktree as the current renderer.
    LoadPrototypeWorktree(String),
    /// Return from a prototype binary to the default dmux-rs executable.
    UnloadPrototype,
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
        identity: Option<AgentLaunchIdentity>,
    },
    SetSetting {
        key: crate::settings::SettingKey,
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
    /// Profiler title row: the drag handle (#103).
    ProfilerTitle,
    /// Profiler close icon.
    ProfilerClose,
    SidebarHelp,
    /// The 🐛 issues chip: opens the newest filed issue in the browser.
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
    /// Right edge of the sidebar — the shared reference for overlay
    /// placement (#91).
    pub sidebar_right: u16,
    /// Source-relative origin assigned when this overlay entered the stack.
    pub anchor: dmux_ui::Anchor,
}

impl ViewCtx<'_> {
    /// Resolve an overlay placement (#91); panel clamping keeps it on
    /// screen. Views receive their origin from the action that opened them.
    pub fn place(&self, area: Rect, anchor: dmux_ui::Anchor, w: u16, h: u16) -> Rect {
        dmux_ui::place(area, self.sidebar_right, anchor, w, h)
    }

    /// Stack-assigned placement beside the sidebar.
    pub fn overlay(&self, area: Rect, w: u16, h: u16) -> Rect {
        self.place(area, self.anchor, w, h)
    }

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

    /// A region the overlay scrim must leave undimmed. An anchored flyout
    /// returns its source pane or sidebar row. Asked of the top view only,
    /// so a closed or replaced view cannot leave a stale carve-out.
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

    /// Plain Left only: a modified arrow (Option/Command word and line
    /// navigation, #96) must fall through to `as_input_key`, never trigger
    /// view navigation like the picker's go-to-parent.
    pub fn is_left(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::LeftArrow) && k.modifiers.is_empty()
    }

    /// Plain Right only; see `is_left`.
    pub fn is_right(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::RightArrow) && k.modifiers.is_empty()
    }

    pub fn is_tab(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::Tab)
    }

    pub fn is_space(k: &KeyEvent) -> bool {
        matches!(k.key, KeyCode::Char(' '))
    }

    /// Map a key event to a text-input edit, if it is one. Arrow up/down and
    /// friends are intentionally NOT input keys so lists can keep them.
    /// The one translation from host key events to text edits (#96):
    /// every view with a TextInput routes through here, so macOS word
    /// (Option+arrow) and line (Command+arrow) navigation work everywhere.
    pub fn as_input_key(k: &KeyEvent) -> Option<InputKey> {
        let ctrl = k.modifiers.contains(Modifiers::CTRL);
        let alt = k.modifiers.contains(Modifiers::ALT);
        let cmd = k.modifiers.contains(Modifiers::SUPER);
        Some(match (&k.key, ctrl, alt) {
            (KeyCode::Char('a'), true, _) => InputKey::Home,
            (KeyCode::Char('e'), true, _) => InputKey::End,
            (KeyCode::Char('u'), true, _) => InputKey::KillToStart,
            (KeyCode::Char('k'), true, _) => InputKey::KillToEnd,
            (KeyCode::Char('w'), true, _) => InputKey::DeleteWordBack,
            (KeyCode::Char(c), false, false) if !cmd => InputKey::Char(*c),
            (KeyCode::LeftArrow, false, true) => InputKey::WordLeft,
            (KeyCode::RightArrow, false, true) => InputKey::WordRight,
            // macOS terminals commonly send Option+arrows as ESC b / ESC f
            // (readline word motions); accept both spellings.
            (KeyCode::Char('b'), false, true) => InputKey::WordLeft,
            (KeyCode::Char('f'), false, true) => InputKey::WordRight,
            (KeyCode::LeftArrow, false, false) if cmd => InputKey::Home,
            (KeyCode::RightArrow, false, false) if cmd => InputKey::End,
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
    fn shared_input_path_translates_modified_navigation_once() {
        use dmux_host::{KeyCode, KeyEvent, Modifiers};
        use dmux_ui::InputKey;
        let key = |code, mods| KeyEvent {
            key: code,
            modifiers: mods,
        };
        // Option+arrows → word movement; Command+arrows → line ends.
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::LeftArrow, Modifiers::ALT)),
            Some(InputKey::WordLeft)
        ));
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::RightArrow, Modifiers::ALT)),
            Some(InputKey::WordRight)
        ));
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::LeftArrow, Modifiers::SUPER)),
            Some(InputKey::Home)
        ));
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::RightArrow, Modifiers::SUPER)),
            Some(InputKey::End)
        ));
        // macOS byte spelling of Option+arrows: ESC b / ESC f.
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::Char('b'), Modifiers::ALT)),
            Some(InputKey::WordLeft)
        ));
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::Char('f'), Modifiers::ALT)),
            Some(InputKey::WordRight)
        ));
        // Modified arrows never leak into view navigation predicates.
        assert!(!vkeys::is_left(&key(KeyCode::LeftArrow, Modifiers::ALT)));
        assert!(!vkeys::is_right(&key(
            KeyCode::RightArrow,
            Modifiers::SUPER
        )));
        // Plain and Control behavior is unchanged.
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::LeftArrow, Modifiers::NONE)),
            Some(InputKey::Left)
        ));
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::Char('a'), Modifiers::CTRL)),
            Some(InputKey::Home)
        ));
        assert!(matches!(
            vkeys::as_input_key(&key(KeyCode::Char('w'), Modifiers::CTRL)),
            Some(InputKey::DeleteWordBack)
        ));
        // Command+char is a chord, not text.
        assert!(vkeys::as_input_key(&key(KeyCode::Char('c'), Modifiers::SUPER)).is_none());
    }

    #[test]
    fn overlay_hover_visually_overrides_keyboard_selection() {
        let theme = Theme::named("violet");
        let hovered = ViewCtx {
            theme: &theme,
            anim: 0,
            hovered: Some(ClickTarget::Overlay(7)),
            sidebar_right: 0,
            anchor: dmux_ui::Anchor::SidebarTop,
        };
        assert!(hovered.active_overlay(7, false));
        assert!(!hovered.active_overlay(3, true));

        let keyboard = ViewCtx {
            hovered: None,
            sidebar_right: 0,
            anchor: dmux_ui::Anchor::SidebarTop,
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
