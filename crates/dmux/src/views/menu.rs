use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::{KeyCode, KeyEvent};
use dmux_ui::{centered, draw_hint_bar, draw_panel, ClickMap, ListState, PanelStyle};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    /// Shortcut hint shown right-aligned (the leader key that also does it).
    pub hint: String,
    pub cmd: AppCmd,
    pub danger: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, hint: impl Into<String>, cmd: AppCmd) -> Self {
        Self {
            label: label.into(),
            hint: hint.into(),
            cmd,
            danger: false,
        }
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

/// Generic list menu — the pane menu and any future context menus.
pub struct MenuView {
    title: String,
    items: Vec<MenuItem>,
    list: ListState,
    /// Anchor cell for a row-attached flyout (#14); None = centered modal.
    anchor: Option<(u16, u16)>,
    /// Originating pane surface or sidebar row, kept undimmed by the scrim.
    source: Option<Rect>,
}

impl MenuView {
    pub fn new(title: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self {
            title: title.into(),
            items,
            list: ListState::default(),
            anchor: None,
            source: None,
        }
    }

    /// Render as a flyout whose top-left sits at (x, y), clamped to the
    /// terminal so edge rows stay fully usable.
    pub fn anchored(mut self, x: u16, y: u16) -> Self {
        self.anchor = Some((x, y));
        self
    }

    /// The source surface this flyout belongs to, excluded from the scrim.
    pub fn with_source(mut self, source: Rect) -> Self {
        self.source = Some(source);
        self
    }

    fn activate(&mut self, idx: usize) -> ViewResult {
        match self.items.get(idx) {
            Some(item) => ViewResult::CloseAnd(item.cmd.clone()),
            None => ViewResult::Stay,
        }
    }
}

/// Clamp a `w`×`h` flyout anchored at (x, y) into `area` — shifted left/up
/// as needed so rows near the right or bottom edge keep the whole panel on
/// screen.
pub fn flyout_rect(area: Rect, (x, y): (u16, u16), w: u16, h: u16) -> Rect {
    let w = w.min(area.w);
    let h = h.min(area.h);
    let x = x.min(area.right().saturating_sub(w)).max(area.x);
    let y = y.min(area.bottom().saturating_sub(h)).max(area.y);
    Rect::new(x, y, w, h)
}

impl View for MenuView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let w = (self
            .items
            .iter()
            .map(|i| i.label.chars().count() + i.hint.chars().count() + 8)
            .max()
            .unwrap_or(20)
            .max(self.title.chars().count() + 6) as u16)
            .clamp(28, area.w);
        let h = (self.items.len() as u16 + 4).min(area.h);
        let rect = match self.anchor {
            Some(anchor) => flyout_rect(area, anchor, w, h),
            None => centered(area, w, h),
        };
        let inner = draw_panel(buf, rect, &self.title, ctx.theme, PanelStyle::Modal);

        let visible = inner.h.saturating_sub(1) as usize;
        self.list.clamp(self.items.len());
        self.list.ensure_visible(visible);
        for (row, (i, item)) in self
            .items
            .iter()
            .enumerate()
            .skip(self.list.scroll)
            .take(visible)
            .enumerate()
        {
            let y = inner.y + row as u16;
            let selected = ctx.active_overlay(i as u64, i == self.list.selected);
            let line_rect = Rect::new(inner.x, y, inner.w, 1);
            let bg = if selected {
                ctx.theme.bg_selected
            } else {
                ctx.theme.bg_panel
            };
            buf.fill(
                line_rect,
                &dmux_compositor::Cell {
                    bg,
                    ..Default::default()
                },
            );
            let fg = if item.danger {
                ctx.theme.danger
            } else if selected {
                ctx.theme.text
            } else {
                ctx.theme.text_dim
            };
            let caret = if selected { "▸ " } else { "  " };
            buf.draw_text(
                inner.x,
                y,
                caret,
                ctx.theme.accent,
                bg,
                AttrFlags::BOLD,
                line_rect,
            );
            buf.draw_text(
                inner.x + 2,
                y,
                &item.label,
                fg,
                bg,
                if selected {
                    AttrFlags::BOLD
                } else {
                    AttrFlags::empty()
                },
                line_rect,
            );
            if !item.hint.is_empty() {
                let hx = inner
                    .right()
                    .saturating_sub(item.hint.chars().count() as u16 + 1);
                buf.draw_text(
                    hx,
                    y,
                    &item.hint,
                    ctx.theme.text_faint,
                    bg,
                    AttrFlags::empty(),
                    line_rect,
                );
            }
            clicks.add(line_rect, ClickTarget::Overlay(i as u64));
        }
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &[("↑↓", "select"), ("⏎", "run"), ("esc", "close")],
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_up(key) {
            self.list.step(-1, self.items.len());
            return ViewResult::Stay;
        }
        if vkeys::is_down(key) {
            self.list.step(1, self.items.len());
            return ViewResult::Stay;
        }
        if vkeys::is_enter(key) {
            return self.activate(self.list.selected);
        }
        // First-letter activation.
        if let KeyCode::Char(c) = key.key {
            if let Some(i) = self
                .items
                .iter()
                .position(|it| it.label.to_lowercase().starts_with(c.to_ascii_lowercase()))
            {
                self.list.selected = i;
                return self.activate(i);
            }
        }
        ViewResult::Stay
    }

    fn on_click(&mut self, tag: u64) -> ViewResult {
        self.list.selected = tag as usize;
        self.activate(tag as usize)
    }

    fn on_hover(&mut self, tag: u64) -> u64 {
        if (tag as usize) < self.items.len() {
            self.list.selected = tag as usize;
        }
        tag
    }

    fn on_wheel(&mut self, delta: i32) -> ViewResult {
        self.list.step(delta, self.items.len());
        ViewResult::Stay
    }

    fn scrim_exception(&self) -> Option<Rect> {
        self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::Modifiers;

    #[test]
    fn flyout_placement_stays_in_bounds() {
        let area = Rect::new(0, 0, 120, 40);
        // Interior anchor: exact position.
        assert_eq!(flyout_rect(area, (30, 5), 30, 10), Rect::new(30, 5, 30, 10));
        // Bottom-edge row: shifted up so the panel stays on screen.
        let r = flyout_rect(area, (30, 38), 30, 10);
        assert_eq!(r.bottom(), 40);
        assert_eq!(r.y, 30);
        // Right-edge anchor: shifted left.
        let r = flyout_rect(area, (118, 5), 30, 10);
        assert_eq!(r.right(), 120);
        // Panel larger than the terminal: clamped to it.
        let r = flyout_rect(area, (10, 10), 200, 200);
        assert_eq!((r.w, r.h), (120, 40));
    }

    #[test]
    fn escape_dismisses_the_flyout() {
        let mut v =
            MenuView::new("pane", vec![MenuItem::new("Rename", "", AppCmd::Quit)]).anchored(30, 5);
        let esc = KeyEvent {
            key: KeyCode::Escape,
            modifiers: Modifiers::NONE,
        };
        assert!(matches!(v.on_key(&esc), ViewResult::Close));
    }

    #[test]
    fn enter_runs_the_selected_item() {
        let mut v =
            MenuView::new("pane", vec![MenuItem::new("Rename", "", AppCmd::Quit)]).anchored(30, 5);
        let enter = KeyEvent {
            key: KeyCode::Enter,
            modifiers: Modifiers::NONE,
        };
        assert!(matches!(v.on_key(&enter), ViewResult::CloseAnd(_)));
    }

    #[test]
    fn hover_moves_selection_before_keyboard_navigation() {
        let mut v = MenuView::new(
            "pane",
            vec![
                MenuItem::new("First", "", AppCmd::Quit),
                MenuItem::new("Second", "", AppCmd::Noop),
                MenuItem::new("Third", "", AppCmd::Noop),
            ],
        );
        assert_eq!(v.on_hover(1), 1);
        assert_eq!(v.list.selected, 1);
        let down = KeyEvent {
            key: KeyCode::DownArrow,
            modifiers: Modifiers::NONE,
        };
        v.on_key(&down);
        assert_eq!(v.list.selected, 2);
    }
}
