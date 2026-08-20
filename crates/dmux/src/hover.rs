//! Pointer-driven navigation state.

use crate::sidebar::{self, SidebarNavTarget};
use crate::views::{self, ClickTarget};
use crate::App;

/// Clamp a one-row tooltip beside its pointer anchor and inside `area`.
pub(super) fn tooltip_rect(
    area: dmux_compositor::Rect,
    (x, y): (u16, u16),
    w: u16,
) -> dmux_compositor::Rect {
    let w = w.min(area.w);
    let ty = if y > area.y {
        y - 1
    } else {
        y.min(area.bottom().saturating_sub(1))
    };
    let tx = x.min(area.right().saturating_sub(w)).max(area.x);
    dmux_compositor::Rect::new(tx, ty.min(area.bottom().saturating_sub(1)), w, 1)
}

impl App {
    pub(super) fn update_hover_target(&mut self, target: ClickTarget) {
        if self.hovered == Some(target) {
            return;
        }
        let target = self.adopt_hover_target(target);
        if views::update_hover(&mut self.hovered, Some(target)) {
            self.dirty = true;
            self.interactions.local_changed();
        }
    }

    /// Move the keyboard selection to a hovered control and return the visual
    /// target that represents that canonical position.
    pub(super) fn adopt_hover_target(&mut self, target: ClickTarget) -> ClickTarget {
        let mut rebuild_sidebar = false;
        if let Some(selection) = sidebar::hover_navigation(target, &self.sidebar_groups) {
            match selection {
                SidebarNavTarget::Pane(index) if index < self.panes.len() => {
                    rebuild_sidebar = self.selected != index
                        || self.sidebar_project.is_some()
                        || !self.sidebar_focused;
                    self.selected = index;
                    self.sidebar_project = None;
                }
                SidebarNavTarget::Project(project) => {
                    rebuild_sidebar =
                        self.sidebar_project.as_ref() != Some(&project) || !self.sidebar_focused;
                    self.sidebar_project = Some(project);
                }
                SidebarNavTarget::Pane(_) => {}
            }
            self.sidebar_focused = true;
        } else {
            match target {
                ClickTarget::WelcomeCard(index) if index < self.welcome_cards.len() => {
                    self.welcome_sel = index;
                }
                ClickTarget::Overlay(tag) => {
                    if let Some(view) = self.views.last_mut() {
                        return ClickTarget::Overlay(view.on_hover(tag));
                    }
                }
                _ => {}
            }
        }
        if rebuild_sidebar {
            self.rebuild_sidebar_groups();
        }
        target
    }
}
