//! Overlay stack transitions and context-menu presentation shared by every
//! native view.

use std::path::PathBuf;

use dmux_compositor::Rect;
use dmux_core::i18n::t;

use crate::hooks;
use crate::pane_actions;
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
            Self::Global => None,
            Self::SidebarTarget { target, .. } => Some(*target),
            Self::SidebarProject { project, .. } => project_click_target(project, groups),
        }
    }

    fn align(&self) -> Option<VerticalAlign> {
        match self {
            Self::Global => None,
            Self::SidebarTarget { align, .. } | Self::SidebarProject { align, .. } => Some(*align),
        }
    }

    pub(crate) fn resolve(
        &self,
        clicks: &ClickMap<ClickTarget>,
        groups: &[SidebarGroup],
    ) -> Anchor {
        self.target(groups)
            .and_then(|target| clicks.rect_for(&target))
            .zip(self.align())
            .map(|(rect, align)| Anchor::SidebarRow { row: rect.y, align })
            .unwrap_or(Anchor::SidebarTop)
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
    pub(crate) fn push(&mut self, view: Box<dyn crate::views::View>) {
        self.push_at(view, OverlayOrigin::Global);
    }

    pub(crate) fn push_at(&mut self, view: Box<dyn crate::views::View>, origin: OverlayOrigin) {
        self.0.push(OverlayEntry { view, origin });
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
            self.views.push(Box::new(menu));
            self.dirty = true;
        }
        true
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
