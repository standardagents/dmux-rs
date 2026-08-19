use dmux_compositor::{AttrFlags, Cell, CellBuffer, Rect};

use crate::Theme;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Braille spinner frame for an animation tick (any monotonically increasing
/// counter). The whole UI shares one tick so spinners phase together.
pub fn spinner_frame(tick: u64) -> char {
    SPINNER[(tick % SPINNER.len() as u64) as usize]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Quiet,
    Danger,
}

/// Draw `[ label ]`-style button; returns its rect for click registration.
#[allow(clippy::too_many_arguments)]
pub fn draw_button(
    buf: &mut CellBuffer,
    x: u16,
    y: u16,
    label: &str,
    theme: &Theme,
    style: ButtonStyle,
    focused: bool,
    clip: Rect,
) -> Rect {
    let text = format!(" {label} ");
    let (fg, bg, attrs) = match (style, focused) {
        (ButtonStyle::Primary, true) => (
            dmux_compositor::Color::Indexed(233),
            theme.accent,
            AttrFlags::BOLD,
        ),
        (ButtonStyle::Primary, false) => (theme.accent, theme.bg_selected, AttrFlags::BOLD),
        (ButtonStyle::Danger, true) => (
            dmux_compositor::Color::Indexed(233),
            theme.danger,
            AttrFlags::BOLD,
        ),
        (ButtonStyle::Danger, false) => (theme.danger, theme.bg_selected, AttrFlags::empty()),
        (ButtonStyle::Quiet, true) => (theme.text, theme.bg_selected, AttrFlags::BOLD),
        (ButtonStyle::Quiet, false) => (theme.text_dim, theme.bg_raised, AttrFlags::empty()),
    };
    let end = buf.draw_text(x, y, &text, fg, bg, attrs, clip);
    Rect::new(x, y, end.saturating_sub(x), 1)
}

/// `label ......... value` row used by settings and menus; returns the rect of
/// the value control for click registration.
#[allow(clippy::too_many_arguments)]
pub fn draw_kv_row(
    buf: &mut CellBuffer,
    rect: Rect,
    label: &str,
    value: &str,
    theme: &Theme,
    selected: bool,
    enabled: bool,
) -> Rect {
    let bg = if selected {
        theme.bg_selected
    } else {
        theme.bg_panel
    };
    buf.fill(
        Rect::new(rect.x, rect.y, rect.w, 1),
        &Cell {
            bg,
            ..Cell::default()
        },
    );
    let label_fg = if !enabled {
        theme.text_faint
    } else if selected {
        theme.text
    } else {
        theme.text_dim
    };
    buf.draw_text(
        rect.x + 1,
        rect.y,
        label,
        label_fg,
        bg,
        if selected {
            AttrFlags::BOLD
        } else {
            AttrFlags::empty()
        },
        rect,
    );
    let value_fg = if enabled {
        theme.accent
    } else {
        theme.text_faint
    };
    let vw: u16 = value.chars().count() as u16;
    let vx = rect.right().saturating_sub(vw + 2);
    buf.draw_text(vx, rect.y, value, value_fg, bg, AttrFlags::empty(), rect);
    Rect::new(vx, rect.y, vw, 1)
}

/// `‹ value ›` select control text.
pub fn draw_select_value(current: &str) -> String {
    format!("‹ {current} ›")
}

/// Checkbox glyphs.
pub fn draw_checkbox(checked: bool) -> &'static str {
    if checked {
        "◼"
    } else {
        "◻"
    }
}

/// Radio glyphs.
pub fn draw_radio(on: bool) -> &'static str {
    if on {
        "◉"
    } else {
        "○"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterHighlight {
    None,
    Row,
    Minus,
    Plus,
}

/// `[-] n [+]` counter control; returns (minus_rect, plus_rect).
pub fn draw_counter(
    buf: &mut CellBuffer,
    x: u16,
    y: u16,
    value: u8,
    theme: &Theme,
    highlight: CounterHighlight,
    clip: Rect,
) -> (Rect, Rect) {
    let bg = if highlight == CounterHighlight::Row {
        theme.bg_selected
    } else {
        theme.bg_panel
    };
    let minus_hovered = highlight == CounterHighlight::Minus;
    let plus_hovered = highlight == CounterHighlight::Plus;
    let minus_fg = if minus_hovered && value > 0 {
        theme.accent
    } else if value > 0 {
        theme.text
    } else {
        theme.text_faint
    };
    let minus_bg = if minus_hovered { theme.bg_selected } else { bg };
    let mut cx = buf.draw_text(
        x,
        y,
        "[-]",
        minus_fg,
        minus_bg,
        if minus_hovered {
            AttrFlags::BOLD
        } else {
            AttrFlags::empty()
        },
        clip,
    );
    let minus = Rect::new(x, y, cx - x, 1);
    let val_fg = if value > 0 {
        theme.accent
    } else {
        theme.text_dim
    };
    cx = buf.draw_text(
        cx,
        y,
        &format!(" {value} "),
        val_fg,
        bg,
        AttrFlags::BOLD,
        clip,
    );
    let plus_x = cx;
    let plus_bg = if plus_hovered { theme.bg_selected } else { bg };
    cx = buf.draw_text(
        cx,
        y,
        "[+]",
        if plus_hovered {
            theme.accent
        } else {
            theme.text
        },
        plus_bg,
        if plus_hovered {
            AttrFlags::BOLD
        } else {
            AttrFlags::empty()
        },
        clip,
    );
    (minus, Rect::new(plus_x, y, cx - plus_x, 1))
}

/// Bottom hint bar: `key desc  ·  key desc …` in consistent styling.
pub fn draw_hint_bar(buf: &mut CellBuffer, rect: Rect, hints: &[(&str, &str)], theme: &Theme) {
    if rect.is_empty() {
        return;
    }
    let bg = theme.bg_panel;
    buf.fill(
        Rect::new(rect.x, rect.y, rect.w, 1),
        &Cell {
            bg,
            ..Cell::default()
        },
    );
    let mut x = rect.x + 1;
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            x = buf.draw_text(
                x,
                rect.y,
                "  ·  ",
                theme.text_faint,
                bg,
                AttrFlags::empty(),
                rect,
            );
        }
        x = buf.draw_text(x, rect.y, key, theme.accent, bg, AttrFlags::BOLD, rect);
        x = buf.draw_text(x, rect.y, " ", theme.text_dim, bg, AttrFlags::empty(), rect);
        x = buf.draw_text(
            x,
            rect.y,
            desc,
            theme.text_dim,
            bg,
            AttrFlags::empty(),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_hover_highlights_only_the_targeted_control() {
        let theme = Theme::named("violet");
        let mut buf = CellBuffer::new(20, 1);
        let clip = buf.area();
        let (minus, plus) = draw_counter(&mut buf, 1, 0, 1, &theme, CounterHighlight::Plus, clip);
        assert_ne!(buf.get(minus.x, 0).bg, theme.bg_selected);
        assert_eq!(buf.get(plus.x, 0).bg, theme.bg_selected);
        assert!(buf.get(plus.x, 0).attrs.contains(AttrFlags::BOLD));
    }
}
