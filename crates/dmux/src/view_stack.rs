//! Overlay stack transitions and context-menu presentation shared by every
//! native view.

use std::path::PathBuf;

use dmux_compositor::Rect;
use dmux_core::i18n::t;

use crate::hooks;
use crate::views::{AppCmd, ClickTarget, ContextMenuTarget, MenuItem, MenuView, ViewResult};
use crate::App;

impl App {
    /// Pane-scoped actions shared by leader-key and pointer menus.
    pub(super) fn pane_menu_items(&self, idx: usize) -> Vec<MenuItem> {
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
            items.push(MenuItem::new(t("menu.close"), "^b x", AppCmd::ConfirmClose(idx)).danger());
        }
        items
    }

    fn open_pane_flyout(&mut self, idx: usize, x: u16, y: u16, source: Option<Rect>) -> bool {
        if let Some(pane) = self.panes.get(idx) {
            let title = pane.display_title().to_string();
            let items = self.pane_menu_items(idx);
            let mut menu = MenuView::new(title, items).anchored(x, y);
            if let Some(source) = source {
                menu = menu.with_source(source);
            }
            self.views.push(Box::new(menu));
            self.dirty = true;
        }
        true
    }

    pub(super) fn open_sidebar_pane_flyout(&mut self, idx: usize, y: u16) -> bool {
        if self.selected != idx || self.sidebar_project.is_some() {
            self.selected = idx;
            self.sidebar_project = None;
            self.rebuild_sidebar_groups();
        }
        let source = Rect::new(self.layout.sidebar.x, y, self.layout.sidebar.w, 1);
        self.open_pane_flyout(idx, self.layout.sidebar.right() + 1, y, Some(source))
    }

    pub(super) fn open_context_menu(
        &mut self,
        target: Option<ClickTarget>,
        col: u16,
        row: u16,
    ) -> bool {
        match target.and_then(ClickTarget::context_menu) {
            Some(ContextMenuTarget::Pane(idx)) => self.open_pane_flyout(idx, col, row, None),
            Some(ContextMenuTarget::SidebarPane(idx)) => self.open_sidebar_pane_flyout(idx, row),
            None => true,
        }
    }

    pub(super) fn apply_view_result(&mut self, result: ViewResult) -> bool {
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
                self.views.push(view);
                self.dirty = true;
                true
            }
            ViewResult::Cmd(cmd) => self.execute_cmd(cmd),
            ViewResult::CloseAnd(cmd) => {
                self.views.pop();
                self.dirty = true;
                self.execute_cmd(cmd)
            }
            ViewResult::CloseTwoAnd(cmd) => {
                self.views.pop();
                self.views.pop();
                self.dirty = true;
                self.execute_cmd(cmd)
            }
        }
    }
}
