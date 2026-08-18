use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};

use crate::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelStyle {
    /// Accent-bordered modal panel.
    Modal,
    /// Quiet panel (menus, tooltips).
    Flat,
}

/// Dim everything under an overlay so the modal reads as a layer. Cheap trick:
/// repaint every cell's colors to the faint ramp, keeping glyphs.
pub fn draw_scrim(buf: &mut CellBuffer, area: Rect) {
    let clip = area.intersect(&buf.area());
    for row in clip.y..clip.bottom() {
        for col in clip.x..clip.right() {
            let mut cell = buf.get(col, row).clone();
            cell.fg = Color::Indexed(238);
            cell.bg = Color::Indexed(232);
            cell.attrs.remove(AttrFlags::BOLD);
            buf.set(col, row, cell);
        }
    }
}

/// Draw a rounded panel with a title in the top border and return the inner
/// content rect. The consistent chrome for every dmux overlay.
pub fn draw_panel(buf: &mut CellBuffer, rect: Rect, title: &str, theme: &Theme, style: PanelStyle) -> Rect {
    let rect = rect.intersect(&buf.area());
    if rect.w < 4 || rect.h < 3 {
        return Rect::default();
    }
    let border_fg = match style {
        PanelStyle::Modal => theme.accent,
        PanelStyle::Flat => theme.border,
    };
    let bg = theme.bg_raised;

    buf.fill(rect, &Cell { bg, ..Cell::default() });

    let (x0, y0, x1, y1) = (rect.x, rect.y, rect.right() - 1, rect.bottom() - 1);
    let horiz = Cell { ch: '─', fg: border_fg, bg, ..Cell::default() };
    for col in x0 + 1..x1 {
        buf.set(col, y0, horiz.clone());
        buf.set(col, y1, horiz.clone());
    }
    let vert = Cell { ch: '│', fg: border_fg, bg, ..Cell::default() };
    for row in y0 + 1..y1 {
        buf.set(x0, row, vert.clone());
        buf.set(x1, row, vert.clone());
    }
    buf.set(x0, y0, Cell { ch: '╭', fg: border_fg, bg, ..Cell::default() });
    buf.set(x1, y0, Cell { ch: '╮', fg: border_fg, bg, ..Cell::default() });
    buf.set(x0, y1, Cell { ch: '╰', fg: border_fg, bg, ..Cell::default() });
    buf.set(x1, y1, Cell { ch: '╯', fg: border_fg, bg, ..Cell::default() });

    if !title.is_empty() {
        let label = format!(" {title} ");
        buf.draw_text(x0 + 2, y0, &label, theme.accent, bg, AttrFlags::BOLD, rect);
    }

    Rect::new(rect.x + 2, rect.y + 1, rect.w.saturating_sub(4), rect.h.saturating_sub(2))
}

/// Center a `w`×`h` panel within `area` (clamped).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.w);
    let h = h.min(area.h);
    Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
}
