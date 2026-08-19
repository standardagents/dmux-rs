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
    draw_scrim_except(buf, area, None);
}

/// `draw_scrim` with a carve-out: cells inside `except` keep their styling —
/// used to keep a flyout's originating sidebar row visually connected to the
/// menu beside it while the rest of the scene dims.
pub fn draw_scrim_except(buf: &mut CellBuffer, area: Rect, except: Option<Rect>) {
    let clip = area.intersect(&buf.area());
    for row in clip.y..clip.bottom() {
        for col in clip.x..clip.right() {
            if let Some(keep) = except {
                if col >= keep.x && col < keep.right() && row >= keep.y && row < keep.bottom() {
                    continue;
                }
            }
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
pub fn draw_panel(
    buf: &mut CellBuffer,
    rect: Rect,
    title: &str,
    theme: &Theme,
    style: PanelStyle,
) -> Rect {
    let rect = rect.intersect(&buf.area());
    if rect.w < 4 || rect.h < 3 {
        return Rect::default();
    }
    let border_fg = match style {
        PanelStyle::Modal => theme.accent,
        PanelStyle::Flat => theme.border,
    };
    let bg = theme.bg_panel;

    buf.fill(
        rect,
        &Cell {
            bg,
            ..Cell::default()
        },
    );

    let (x0, y0, x1, y1) = (rect.x, rect.y, rect.right() - 1, rect.bottom() - 1);
    let horiz = Cell {
        ch: '─',
        fg: border_fg,
        bg,
        ..Cell::default()
    };
    for col in x0 + 1..x1 {
        buf.set(col, y0, horiz.clone());
        buf.set(col, y1, horiz.clone());
    }
    let vert = Cell {
        ch: '│',
        fg: border_fg,
        bg,
        ..Cell::default()
    };
    for row in y0 + 1..y1 {
        buf.set(x0, row, vert.clone());
        buf.set(x1, row, vert.clone());
    }
    buf.set(
        x0,
        y0,
        Cell {
            ch: '╭',
            fg: border_fg,
            bg,
            ..Cell::default()
        },
    );
    buf.set(
        x1,
        y0,
        Cell {
            ch: '╮',
            fg: border_fg,
            bg,
            ..Cell::default()
        },
    );
    buf.set(
        x0,
        y1,
        Cell {
            ch: '╰',
            fg: border_fg,
            bg,
            ..Cell::default()
        },
    );
    buf.set(
        x1,
        y1,
        Cell {
            ch: '╯',
            fg: border_fg,
            bg,
            ..Cell::default()
        },
    );

    if !title.is_empty() {
        let label = format!(" {title} ");
        buf.draw_text(x0 + 2, y0, &label, theme.accent, bg, AttrFlags::BOLD, rect);
    }

    Rect::new(
        rect.x + 2,
        rect.y + 1,
        rect.w.saturating_sub(4),
        rect.h.saturating_sub(2),
    )
}

/// Shared overlay geometry (#42): every modal splits its `draw_panel` inner
/// rect the same way — one padding row under the title border, the body,
/// one blank row, then the bottom hint row.
#[derive(Debug, Clone, Copy, Default)]
pub struct PanelFrame {
    /// Body region: lists, fields, sections, action rows.
    pub content: Rect,
    /// Single bottom row for `draw_hint_bar`.
    pub footer: Rect,
}

/// Split a panel's inner rect into the shared frame.
pub fn panel_frame(inner: Rect) -> PanelFrame {
    if inner.h < 4 {
        return PanelFrame {
            content: inner,
            footer: Rect::default(),
        };
    }
    PanelFrame {
        content: Rect::new(inner.x, inner.y + 1, inner.w, inner.h - 3),
        footer: Rect::new(inner.x, inner.bottom() - 1, inner.w, 1),
    }
}

/// Total panel height for `content_rows` body rows plus the shared chrome:
/// two border rows, the top padding row, the blank row, and the hint row.
pub fn frame_height(content_rows: u16) -> u16 {
    content_rows + 5
}

/// Center a `w`×`h` panel within `area` (clamped).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.w);
    let h = h.min(area.h);
    Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
}

#[cfg(test)]
mod panel_tests {
    use super::*;

    #[test]
    fn panel_surface_inherits_the_terminal_background() {
        // #42: overlays sit on the user's terminal background like the
        // sidebar and canvas — no fixed gray slab.
        let mut buf = CellBuffer::new(30, 10);
        buf.fill(
            buf.area(),
            &Cell {
                bg: Color::Indexed(17),
                ..Cell::default()
            },
        );
        let theme = crate::theme::Theme::default();
        let rect = Rect::new(2, 1, 24, 8);
        let inner = draw_panel(&mut buf, rect, "Title", &theme, PanelStyle::Modal);
        // Interior and border cells both carry the terminal default.
        assert_eq!(buf.get(inner.x, inner.y).bg, Color::Default);
        assert_eq!(buf.get(rect.x, rect.y).bg, Color::Default);
        assert_eq!(
            buf.get(rect.right() - 1, rect.bottom() - 1).bg,
            Color::Default
        );
        // Outside the panel the underlying scene is untouched.
        assert_eq!(buf.get(0, 0).bg, Color::Indexed(17));
    }

    #[test]
    fn panel_frame_reserves_padding_and_footer_rows() {
        // #42 spacing contract: one padding row under the title border, one
        // blank row, then the single-row footer at the panel bottom.
        let inner = Rect::new(4, 2, 40, 12);
        let frame = panel_frame(inner);
        assert_eq!(frame.content, Rect::new(4, 3, 40, 9));
        assert_eq!(frame.footer, Rect::new(4, 13, 40, 1));
        // The blank row sits between them.
        assert_eq!(frame.content.bottom() + 1, frame.footer.y);
        // frame_height sizes a panel so the frame fits its body exactly.
        let h = frame_height(9);
        let sized = Rect::new(0, 0, 40, h - 2); // inner = panel minus borders
        assert_eq!(panel_frame(sized).content.h, 9);
        // Degenerate panels don't underflow.
        let tiny = panel_frame(Rect::new(0, 0, 10, 3));
        assert!(tiny.footer.is_empty());
    }
}

#[cfg(test)]
mod scrim_tests {
    use super::*;

    #[test]
    fn scrim_exception_keeps_the_row_bright() {
        // #16: the flyout's originating sidebar row stays undimmed.
        let mut buf = CellBuffer::new(20, 5);
        let styled = crate::theme::Theme::named("violet");
        buf.fill(
            buf.area(),
            &dmux_compositor::Cell {
                fg: styled.text,
                bg: styled.bg_selected,
                ..Default::default()
            },
        );
        let keep = Rect::new(0, 2, 20, 1);
        let area = buf.area();
        draw_scrim_except(&mut buf, area, Some(keep));
        // Inside the carve-out: original colors.
        assert_eq!(buf.get(5, 2).bg, styled.bg_selected);
        assert_eq!(buf.get(5, 2).fg, styled.text);
        // Outside: dimmed to the scrim ramp.
        assert_eq!(buf.get(5, 1).bg, Color::Indexed(232));
        assert_eq!(buf.get(5, 3).fg, Color::Indexed(238));
        // No exception: everything dims (existing behavior unchanged).
        let mut buf2 = CellBuffer::new(4, 2);
        let area2 = buf2.area();
        buf2.fill(
            area2,
            &dmux_compositor::Cell {
                bg: styled.bg_selected,
                ..Default::default()
            },
        );
        draw_scrim(&mut buf2, area2);
        assert_eq!(buf2.get(0, 0).bg, Color::Indexed(232));
    }
}
