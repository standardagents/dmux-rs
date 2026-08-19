use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_hint_bar, draw_panel, ClickMap, PanelStyle};

use super::{vkeys, ClickTarget, View, ViewCtx, ViewResult};

/// Log viewer: tails the dmux-rs log file (errors and warnings highlighted).
pub struct LogsView {
    lines: Vec<String>,
    scroll: usize,
    path: std::path::PathBuf,
}

impl LogsView {
    pub fn new(path: std::path::PathBuf) -> Self {
        let mut view = Self {
            lines: Vec::new(),
            scroll: 0,
            path,
        };
        view.reload();
        view
    }

    fn reload(&mut self) {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        self.lines = text.lines().rev().take(500).map(String::from).collect();
        self.lines.reverse();
        self.scroll = usize::MAX; // snap to bottom on (re)load
    }
}

impl View for LogsView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let rect = centered(
            area,
            area.w.saturating_sub(8).min(140),
            area.h.saturating_sub(4),
        );
        let inner = draw_panel(buf, rect, "Logs", ctx.theme, PanelStyle::Modal);
        let visible = inner.h.saturating_sub(1) as usize;
        let max_scroll = self.lines.len().saturating_sub(visible);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        let bg = ctx.theme.bg_panel;
        for (row, line) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(visible)
            .enumerate()
        {
            let fg = if line.contains("ERROR") {
                ctx.theme.danger
            } else if line.contains("WARN") {
                ctx.theme.warn
            } else if line.contains("DEBUG") || line.contains("TRACE") {
                ctx.theme.text_faint
            } else {
                ctx.theme.text_dim
            };
            let max = inner.w as usize;
            let clipped: String = line.chars().take(max).collect();
            buf.draw_text(
                inner.x,
                inner.y + row as u16,
                &clipped,
                fg,
                bg,
                AttrFlags::empty(),
                inner,
            );
        }
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &[("↑↓/wheel", "scroll"), ("r", "reload"), ("esc", "close")],
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_up(key) {
            self.scroll = self.scroll.saturating_sub(1);
        } else if vkeys::is_down(key) {
            self.scroll = self.scroll.saturating_add(1);
        } else if matches!(key.key, dmux_host::KeyCode::PageUp) {
            self.scroll = self.scroll.saturating_sub(20);
        } else if matches!(key.key, dmux_host::KeyCode::PageDown) {
            self.scroll = self.scroll.saturating_add(20);
        } else if matches!(key.key, dmux_host::KeyCode::Char('r')) {
            self.reload();
        }
        ViewResult::Stay
    }

    fn on_wheel(&mut self, delta: i32) -> ViewResult {
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(3);
        } else {
            self.scroll = self.scroll.saturating_add(3);
        }
        ViewResult::Stay
    }
}
