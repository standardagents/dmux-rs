//! Overlay stack transitions and context-menu presentation shared by every
//! native view.

use std::path::PathBuf;

use dmux_compositor::Rect;
use dmux_core::i18n::t;

use crate::hooks;
use crate::pane_actions;
use crate::prototype;
use crate::render::SidebarGroup;
use crate::sidebar::{project_click_target, ProjectAction, ProjectSelection};
use crate::views::{
    AppCmd, ClickTarget, ContextMenuTarget, MenuItem, MenuView, ViewCtx, ViewResult,
};
use crate::App;
use dmux_ui::{Anchor, ClickMap, VerticalAlign};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OverlayOrigin {
    Global,
    SidebarTarget {
        target: ClickTarget,
        align: VerticalAlign,
    },
    SidebarProject {
        project: ProjectSelection,
        align: VerticalAlign,
    },
    /// A pointer gesture (#105): dialogs open where the user clicked.
    Pointer {
        x: u16,
        y: u16,
    },
    /// A pane-surface control (#105): resolved from the control's CURRENT
    /// rect each frame, so relayout re-anchors follow-up dialogs to the
    /// pane rather than a stale position.
    PaneControl {
        target: ClickTarget,
    },
}

impl OverlayOrigin {
    pub(crate) fn project(root: String, action: ProjectAction, align: VerticalAlign) -> Self {
        Self::SidebarProject {
            project: ProjectSelection { root, action },
            align,
        }
    }

    fn target(&self, groups: &[SidebarGroup]) -> Option<ClickTarget> {
        match self {
            Self::Global | Self::Pointer { .. } => None,
            Self::SidebarTarget { target, .. } => Some(*target),
            Self::SidebarProject { project, .. } => project_click_target(project, groups),
            Self::PaneControl { target } => Some(*target),
        }
    }

    fn align(&self) -> Option<VerticalAlign> {
        match self {
            Self::Global | Self::Pointer { .. } | Self::PaneControl { .. } => None,
            Self::SidebarTarget { align, .. } | Self::SidebarProject { align, .. } => Some(*align),
        }
    }

    pub(crate) fn resolve(
        &self,
        clicks: &ClickMap<ClickTarget>,
        groups: &[SidebarGroup],
    ) -> Anchor {
        match self {
            // #105: pointer origins are the click itself; pane controls
            // re-resolve from the control's current rect every frame and
            // degrade to the global surface when the control is gone.
            Self::Pointer { x, y } => Anchor::Pointer { x: *x, y: *y },
            Self::PaneControl { target } => clicks
                .rect_for(target)
                .map(|rect| Anchor::Pointer {
                    x: rect.x,
                    y: rect.y,
                })
                .unwrap_or(Anchor::SidebarTop),
            _ => self
                .target(groups)
                .and_then(|target| clicks.rect_for(&target))
                .zip(self.align())
                .map(|(rect, align)| Anchor::SidebarRow { row: rect.y, align })
                .unwrap_or(Anchor::SidebarTop),
        }
    }

    pub(crate) fn source(
        &self,
        clicks: &ClickMap<ClickTarget>,
        groups: &[SidebarGroup],
    ) -> Option<Rect> {
        self.target(groups)
            .and_then(|target| clicks.rect_for(&target))
    }

    pub(crate) fn theme(&self, base: dmux_ui::Theme, groups: &[SidebarGroup]) -> dmux_ui::Theme {
        let Self::SidebarProject { project, .. } = self else {
            return base;
        };
        let Some(group) = groups.iter().find(|group| group.root == project.root) else {
            return base;
        };
        dmux_ui::Theme {
            accent: group.accent,
            accent_soft: group.accent_soft,
            ..base
        }
    }
}

pub(crate) struct OverlayEntry {
    pub(crate) view: Box<dyn crate::views::View>,
    pub(crate) origin: OverlayOrigin,
    kind: OverlayKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayKind {
    Flow,
    ContextMenu,
}

impl std::ops::Deref for OverlayEntry {
    type Target = dyn crate::views::View;

    fn deref(&self) -> &Self::Target {
        self.view.as_ref()
    }
}

impl std::ops::DerefMut for OverlayEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.view.as_mut()
    }
}

#[derive(Default)]
pub(crate) struct OverlayStack(Vec<OverlayEntry>);

impl OverlayStack {
    pub(crate) fn blocks_reload(&self) -> bool {
        self.0.iter().any(|entry| entry.view.blocks_reload())
    }

    pub(crate) fn push(&mut self, view: Box<dyn crate::views::View>) {
        self.push_at(view, OverlayOrigin::Global);
    }

    /// The open Add Project picker, wherever it sits in the stack (#129):
    /// async project creation delivers failures back into it.
    pub(crate) fn path_picker_mut(&mut self) -> Option<&mut crate::views::PathPickerView> {
        self.0
            .iter_mut()
            .find_map(|entry| entry.view.as_path_picker())
    }

    /// Close the Add Project picker after its confirmed creation succeeded.
    pub(crate) fn remove_path_picker(&mut self) {
        let mut i = 0;
        while i < self.0.len() {
            if self.0[i].view.as_path_picker().is_some() {
                self.0.remove(i);
            } else {
                i += 1;
            }
        }
    }

    pub(crate) fn push_at(&mut self, view: Box<dyn crate::views::View>, origin: OverlayOrigin) {
        self.0.push(OverlayEntry {
            view,
            origin,
            kind: OverlayKind::Flow,
        });
    }

    fn replace_context_menu_at(
        &mut self,
        view: Box<dyn crate::views::View>,
        origin: OverlayOrigin,
    ) {
        self.0
            .retain(|entry| entry.kind != OverlayKind::ContextMenu);
        self.0.push(OverlayEntry {
            view,
            origin,
            kind: OverlayKind::ContextMenu,
        });
    }

    pub(crate) fn pop(&mut self) -> Option<OverlayEntry> {
        self.0.pop()
    }
}

impl std::ops::Deref for OverlayStack {
    type Target = [OverlayEntry];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for OverlayStack {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaneMenuClose {
    Confirm,
    Immediate,
}

impl PaneMenuClose {
    fn command(self, idx: usize) -> AppCmd {
        match self {
            Self::Confirm => AppCmd::ConfirmClose(idx),
            Self::Immediate => AppCmd::ClosePane(idx),
        }
    }
}

impl App {
    /// Draw the overlay stack with each entry's current source geometry.
    /// Source targets are looked up after base-scene composition, so a
    /// terminal resize or sidebar reflow cannot leave a popup at a stale row.
    pub(super) fn render_overlays(&mut self) {
        self.view_cursor = None;
        if self.views.is_empty() {
            return;
        }
        let area = self.back.area();
        let contexts: Vec<_> = self
            .views
            .iter()
            .map(|entry| {
                (
                    entry.origin.resolve(&self.click_map, &self.sidebar_groups),
                    entry.origin.theme(self.theme, &self.sidebar_groups),
                )
            })
            .collect();
        let except = self.views.last().and_then(|entry| {
            entry
                .scrim_exception()
                .or_else(|| entry.origin.source(&self.click_map, &self.sidebar_groups))
        });
        dmux_ui::draw_scrim_except(&mut self.back, area, except);
        let full = Rect::new(0, 0, self.size.0, self.size.1);
        let last = self.views.len() - 1;
        for (index, (view, (anchor, theme))) in self.views.iter_mut().zip(contexts).enumerate() {
            let ctx = ViewCtx {
                theme: &theme,
                anim: self.anim,
                hovered: self.hovered,
                sidebar_right: self.layout.sidebar.right(),
                anchor,
            };
            let cursor = view.render(&mut self.back, full, &ctx, &mut self.click_map);
            if index == last {
                self.view_cursor = cursor;
            }
        }
    }

    /// Pane-scoped actions shared by leader-key and pointer menus. The caller
    /// supplies the close behavior associated with the menu's opening gesture.
    pub(super) fn pane_menu_items(&self, idx: usize, close: PaneMenuClose) -> Vec<MenuItem> {
        let mut items = Vec::new();
        if let Some(pane) = self.panes.get(idx) {
            let hide_label = if pane.hidden {
                t("menu.show")
            } else {
                t("menu.hide")
            };
            items.push(MenuItem::new(
                t("menu.rename"),
                "^b r",
                AppCmd::PromptRename(idx),
            ));
            items.push(MenuItem::new(hide_label, "^b h", AppCmd::ToggleHidden(idx)));
            if pane.worktree_path.is_some() {
                items.push(MenuItem::new(t("menu.merge"), "", AppCmd::MergeStart(idx)));
                items.push(MenuItem::new(t("menu.pr"), "", AppCmd::CreatePr(idx)));
                items.push(MenuItem::new(t("menu.diff"), "", AppCmd::ShowDiff(idx)));
                if pane.agent.is_some() {
                    items.push(MenuItem::new(
                        t("menu.duplicate"),
                        "",
                        AppCmd::DuplicatePane(idx),
                    ));
                }
            }
            if let Some(worktree) = pane
                .worktree_path
                .as_deref()
                .and_then(|path| prototype::dmux_worktree(std::path::Path::new(path)))
            {
                let active =
                    prototype::active_worktree().is_some_and(|current| current == worktree);
                let (label, command) = if active {
                    (
                        "Unload prototype, return to default",
                        AppCmd::UnloadPrototype,
                    )
                } else {
                    (
                        "Load this worktree as dmux-rs",
                        AppCmd::LoadPrototypeWorktree(worktree.to_string_lossy().into_owned()),
                    )
                };
                items.push(MenuItem::new(label, "", command).special());
            }
            let hook_root = pane
                .project_root
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.project_root.clone());
            for (hook, label) in [
                ("run_test", t("menu.run_test")),
                ("run_dev", t("menu.run_dev")),
            ] {
                if hooks::hook_path(&hook_root, hook).is_some() {
                    items.push(MenuItem::new(
                        label,
                        "",
                        AppCmd::RunHook {
                            idx,
                            name: hook.into(),
                        },
                    ));
                }
            }
            items.push(MenuItem::new(
                t("menu.copy_path"),
                "",
                AppCmd::CopyPath(idx),
            ));
            items.push(MenuItem::new(
                t("menu.editor"),
                "",
                AppCmd::OpenInEditor(idx),
            ));
            items.push(MenuItem::new(t("menu.close"), "^b x", close.command(idx)).danger());
        }
        items
    }

    fn open_pane_flyout(
        &mut self,
        idx: usize,
        anchor: Anchor,
        source: Option<Rect>,
        close: PaneMenuClose,
    ) -> bool {
        if let Some(pane) = self.panes.get(idx) {
            let title = pane.display_title().to_string();
            let items = self.pane_menu_items(idx, close);
            let mut menu = MenuView::new(title, items);
            menu = match anchor {
                Anchor::Pointer { x, y } => menu.anchored(x, y),
                Anchor::SidebarRow { row, .. } => menu.beside_row(row),
                Anchor::SidebarTop => menu,
            };
            if let Some(source) = source {
                menu = menu.with_source(source);
            }
            // Pointer menus hand their origin to follow-up dialogs (#105);
            // sidebar flyouts keep the existing global dialog placement.
            let origin = match anchor {
                Anchor::Pointer { x, y } => OverlayOrigin::Pointer { x, y },
                _ => OverlayOrigin::Global,
            };
            if matches!(anchor, Anchor::Pointer { .. }) {
                self.views.replace_context_menu_at(Box::new(menu), origin);
            } else {
                self.views.push_at(Box::new(menu), origin);
            }
            self.dirty = true;
        }
        true
    }

    /// Press on a pane title-bar control (#105): actions that open a
    /// dialog anchor it beside the pane title via a live PaneControl
    /// origin, so relayout keeps the dialog with its pane. Double-click on
    /// the title renames; a plain click focuses.
    pub(super) fn title_control_press(
        &mut self,
        target: ClickTarget,
        idx: usize,
        is_double: bool,
    ) -> bool {
        let beside_title = OverlayOrigin::PaneControl {
            target: ClickTarget::PaneTitle(idx),
        };
        match target {
            ClickTarget::PaneTitle(_) if is_double => {
                self.execute_cmd_at(AppCmd::PromptRename(idx), beside_title)
            }
            ClickTarget::PaneTitle(_) => self.execute_cmd(AppCmd::FocusPane(idx)),
            ClickTarget::TitleRename(_) => {
                self.execute_cmd_at(AppCmd::PromptRename(idx), beside_title)
            }
            ClickTarget::TitleHide(_) => self.execute_cmd(AppCmd::ToggleHidden(idx)),
            ClickTarget::TitleClose(_) => {
                // Menu-launched utility panes close in one action (#104).
                let cmd = match self.panes.get(idx) {
                    Some(pane) => crate::pane_actions::title_close_cmd(pane, idx),
                    None => return true,
                };
                self.execute_cmd_at(cmd, beside_title)
            }
            _ => true,
        }
    }

    fn select_sidebar_pane(&mut self, idx: usize) {
        if self.selected != idx || self.sidebar_project.is_some() {
            self.selected = idx;
            self.sidebar_project = None;
            self.rebuild_sidebar_groups();
        }
    }

    /// Sidebar-item action (not a right click): flyout beside the row,
    /// with the confirmed close of a non-pointer gesture (#85).
    pub(super) fn open_sidebar_pane_flyout(&mut self, idx: usize, y: u16) -> bool {
        self.select_sidebar_pane(idx);
        let source = Rect::new(self.layout.sidebar.x, y, self.layout.sidebar.w, 1);
        self.open_pane_flyout(
            idx,
            Anchor::SidebarRow {
                row: y,
                align: VerticalAlign::Top,
            },
            Some(source),
            PaneMenuClose::Confirm,
        )
    }

    pub(super) fn open_context_menu(
        &mut self,
        target: Option<ClickTarget>,
        col: u16,
        row: u16,
    ) -> bool {
        // Every right-click menu opens at the pointer (#91); the source
        // rect (kept undimmed) still marks where the action came from.
        match target.and_then(ClickTarget::context_menu) {
            Some(ContextMenuTarget::Pane(idx)) => {
                let source = self
                    .panes
                    .get(idx)
                    .and_then(|pane| pane.rect)
                    .map(pane_actions::surface_rect);
                self.open_pane_flyout(
                    idx,
                    Anchor::Pointer { x: col, y: row },
                    source,
                    PaneMenuClose::Immediate,
                )
            }
            Some(ContextMenuTarget::SidebarPane(idx)) => {
                self.select_sidebar_pane(idx);
                let source = Rect::new(self.layout.sidebar.x, row, self.layout.sidebar.w, 1);
                self.open_pane_flyout(
                    idx,
                    Anchor::Pointer { x: col, y: row },
                    Some(source),
                    PaneMenuClose::Immediate,
                )
            }
            None => true,
        }
    }

    pub(super) fn apply_view_result(&mut self, result: ViewResult) -> bool {
        let origin = self
            .views
            .last()
            .map(|entry| entry.origin.clone())
            .unwrap_or(OverlayOrigin::Global);
        if !matches!(&result, ViewResult::Stay)
            && matches!(self.hovered, Some(ClickTarget::Overlay(_)))
        {
            self.hovered = None;
        }
        match result {
            ViewResult::Stay => true,
            ViewResult::Close => {
                self.views.pop();
                self.dirty = true;
                true
            }
            ViewResult::Push(view) => {
                self.views.push_at(view, origin);
                self.dirty = true;
                true
            }
            ViewResult::Cmd(cmd) => self.execute_cmd_at(cmd, origin),
            ViewResult::CloseAnd(cmd) => {
                self.views.pop();
                self.dirty = true;
                self.execute_cmd_at(cmd, origin)
            }
            ViewResult::CloseTwoAnd(cmd) => {
                self.views.pop();
                self.views.pop();
                self.dirty = true;
                self.execute_cmd_at(cmd, origin)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_menu() -> Box<dyn crate::views::View> {
        Box::new(MenuView::new("pane", vec![]))
    }

    #[test]
    fn text_entry_anywhere_in_the_stack_blocks_reload() {
        let mut stack = OverlayStack::default();
        stack.push(empty_menu());
        assert!(!stack.blocks_reload());
        stack.push(Box::new(crate::views::InputView::new(
            "Rename",
            "pane",
            "",
            crate::views::InputPurpose::RenamePane(0),
        )));
        assert!(stack.blocks_reload());
        stack.pop();
        assert!(!stack.blocks_reload());
    }

    #[test]
    fn pointer_context_menu_replaces_only_the_previous_context_menu() {
        let mut stack = OverlayStack::default();
        stack.push(empty_menu());
        stack.replace_context_menu_at(empty_menu(), OverlayOrigin::Pointer { x: 20, y: 5 });
        stack.replace_context_menu_at(empty_menu(), OverlayOrigin::Pointer { x: 70, y: 12 });

        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].kind, OverlayKind::Flow);
        assert_eq!(stack[1].kind, OverlayKind::ContextMenu);
        assert_eq!(stack[1].origin, OverlayOrigin::Pointer { x: 70, y: 12 });
    }

    fn group(root: &str, theme: &str) -> SidebarGroup {
        let (accent, accent_soft) = dmux_ui::project_theme(theme);
        SidebarGroup {
            name: root.rsplit('/').next().unwrap_or(root).into(),
            root: root.into(),
            accent,
            accent_soft,
            pane_indices: vec![],
            issue_label: "1 issue".into(),
            active: false,
        }
    }

    #[test]
    fn pane_menu_close_policy_distinguishes_context_and_confirmed_paths() {
        assert_eq!(PaneMenuClose::Immediate.command(4), AppCmd::ClosePane(4));
        assert_eq!(PaneMenuClose::Confirm.command(4), AppCmd::ConfirmClose(4));
    }

    #[test]
    fn pane_action_dialogs_anchor_to_pointer_and_live_title_geometry() {
        // Pointer origin (#105): the dialog opens where the user clicked —
        // a pane far from the sidebar keeps its dialog there.
        let clicks = ClickMap::new();
        assert_eq!(
            OverlayOrigin::Pointer { x: 130, y: 20 }.resolve(&clicks, &[]),
            Anchor::Pointer { x: 130, y: 20 }
        );
        // Title origin resolves from the control's CURRENT rect…
        let target = ClickTarget::PaneTitle(1);
        let origin = OverlayOrigin::PaneControl { target };
        let mut clicks = ClickMap::new();
        clicks.add(Rect::new(120, 0, 40, 1), target);
        assert_eq!(
            origin.resolve(&clicks, &[]),
            Anchor::Pointer { x: 120, y: 0 }
        );
        // …and follows relayout: a moved title re-anchors the dialog.
        clicks.clear();
        clicks.add(Rect::new(60, 0, 40, 1), target);
        assert_eq!(
            origin.resolve(&clicks, &[]),
            Anchor::Pointer { x: 60, y: 0 }
        );
        // A vanished control degrades to the global surface, never a stale
        // position.
        clicks.clear();
        assert_eq!(origin.resolve(&clicks, &[]), Anchor::SidebarTop);
    }

    #[test]
    fn sidebar_origins_follow_current_click_geometry() {
        let target = ClickTarget::SidebarSettings;
        let origin = OverlayOrigin::SidebarTarget {
            target,
            align: VerticalAlign::Bottom,
        };
        let mut clicks = ClickMap::new();
        clicks.add(Rect::new(12, 26, 10, 1), target);
        assert_eq!(
            origin.resolve(&clicks, &[]),
            Anchor::SidebarRow {
                row: 26,
                align: VerticalAlign::Bottom,
            }
        );
        assert_eq!(origin.source(&clicks, &[]), Some(Rect::new(12, 26, 10, 1)));

        clicks.clear();
        clicks.add(Rect::new(12, 16, 10, 1), target);
        assert_eq!(
            origin.resolve(&clicks, &[]),
            Anchor::SidebarRow {
                row: 16,
                align: VerticalAlign::Bottom,
            }
        );
    }

    #[test]
    fn project_origins_follow_identity_for_position_and_theme() {
        let base = dmux_ui::Theme::named("violet");
        let mut groups = vec![
            group("/work/first", "orange"),
            group("/work/second", "cyan"),
        ];
        let origin = OverlayOrigin::project(
            "/work/second".into(),
            ProjectAction::NewAgent,
            VerticalAlign::Top,
        );
        let mut clicks = ClickMap::new();
        clicks.add(
            Rect::new(0, 12, 32, 1),
            ClickTarget::SidebarGroupNewAgent(1),
        );

        assert_eq!(
            origin.resolve(&clicks, &groups),
            Anchor::SidebarRow {
                row: 12,
                align: VerticalAlign::Top,
            }
        );
        assert_eq!(origin.theme(base, &groups).accent, groups[1].accent);

        groups.swap(0, 1);
        let (updated, updated_soft) = dmux_ui::project_theme("green");
        groups[0].accent = updated;
        groups[0].accent_soft = updated_soft;
        clicks.clear();
        clicks.add(Rect::new(0, 4, 32, 1), ClickTarget::SidebarGroupNewAgent(0));

        assert_eq!(
            origin.resolve(&clicks, &groups),
            Anchor::SidebarRow {
                row: 4,
                align: VerticalAlign::Top,
            }
        );
        let updated_theme = origin.theme(base, &groups);
        assert_eq!(updated_theme.accent, updated);
        assert_eq!(updated_theme.accent_soft, updated_soft);
    }

    #[test]
    fn issue_origins_use_project_accents_and_global_origins_keep_the_app_accent() {
        let base = dmux_ui::Theme::named("violet");
        let groups = vec![group("/work/issues", "blue")];
        let project = OverlayOrigin::project(
            "/work/issues".into(),
            ProjectAction::Issues,
            VerticalAlign::Top,
        );

        assert_eq!(project.theme(base, &groups).accent, groups[0].accent);
        assert_eq!(
            OverlayOrigin::Global.theme(base, &groups).accent,
            base.accent
        );
    }
}
