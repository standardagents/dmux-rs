//! Frame composition: sidebar, pane title bars, pane bodies, and the debug
//! HUD, painted into the back `CellBuffer`. Pure — no I/O — so it is testable
//! headlessly; the app loop diffs and writes the result.

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};

use crate::layout::{Layout, TITLE_ROWS};
use crate::metrics::Metrics;
use crate::session::{LogicalPane, PaneStatus};

const ACCENT: Color = Color::Indexed(135); // violet, matches dmux's default theme family
const DIM_TEXT: Color = Color::Indexed(244);
const SIDEBAR_BG: Color = Color::Indexed(233);
const TITLE_BG: Color = Color::Indexed(236);
const TITLE_BG_FOCUSED: Color = Color::Indexed(97);

pub struct Scene<'a> {
    pub panes: &'a [LogicalPane],
    pub layout: &'a Layout,
    pub focused: usize,
    pub selected: usize,
    pub session_name: &'a str,
    pub hud: Option<&'a Metrics>,
    pub status_line: &'a str,
}

pub fn compose(buf: &mut CellBuffer, scene: &Scene<'_>) {
    draw_sidebar(buf, scene);
    for (i, pane) in scene.panes.iter().enumerate() {
        let Some(rect) = pane.rect else { continue };
        draw_pane_title(buf, pane, rect, i == scene.focused);
        pane.term.render_into(buf, rect);
        if pane.paused || pane.throttled || pane.status == PaneStatus::Dead || pane.term.display_offset() > 0 {
            draw_pane_badge(buf, pane, rect);
        }
    }
    if let Some(metrics) = scene.hud {
        draw_hud(buf, metrics);
    }
}

fn draw_sidebar(buf: &mut CellBuffer, scene: &Scene<'_>) {
    let area = scene.layout.sidebar;
    if area.is_empty() {
        return;
    }
    buf.fill(area, &Cell { bg: SIDEBAR_BG, ..Cell::default() });

    // Header: session name between braille fills, dmux-style.
    let header = format!("⣿⣿ {} ", truncate(scene.session_name, area.w.saturating_sub(6) as usize));
    let end = buf.draw_text(area.x, 0, &header, ACCENT, SIDEBAR_BG, AttrFlags::BOLD, area);
    let mut fill = String::new();
    for _ in end..area.right() {
        fill.push('⣿');
    }
    buf.draw_text(end, 0, &fill, ACCENT, SIDEBAR_BG, AttrFlags::BOLD, area);

    // Pane rows.
    let mut row = 2u16;
    for (i, pane) in scene.panes.iter().enumerate() {
        if row >= area.bottom().saturating_sub(2) {
            break;
        }
        let selected = i == scene.selected;
        let focused = i == scene.focused;
        let caret = if selected { "▸" } else { " " };
        let glyph = pane.status_glyph();
        let name = truncate(&pane.title, area.w.saturating_sub(10) as usize);
        let line = format!("{caret} {glyph} {name}");
        let (fg, attrs) = if focused {
            (ACCENT, AttrFlags::BOLD)
        } else if selected {
            (Color::Indexed(255), AttrFlags::empty())
        } else {
            (Color::Indexed(250), AttrFlags::empty())
        };
        let bg = if selected { Color::Indexed(235) } else { SIDEBAR_BG };
        if selected {
            buf.fill(Rect::new(area.x, row, area.w, 1), &Cell { bg, ..Cell::default() });
        }
        let end = buf.draw_text(area.x, row, &line, fg, bg, attrs, area);
        // Right-aligned agent tag.
        if let Some(agent) = &pane.agent {
            let tag = format!("[{}]", &agent[..agent.len().min(2)]);
            let tag_x = area.right().saturating_sub(tag.len() as u16 + 1);
            if tag_x > end {
                buf.draw_text(tag_x, row, &tag, DIM_TEXT, bg, AttrFlags::empty(), area);
            }
        }
        row += 1;
    }

    // Footer.
    let footer_row = area.bottom().saturating_sub(1);
    buf.draw_text(
        area.x,
        footer_row,
        &truncate(scene.status_line, area.w as usize),
        DIM_TEXT,
        SIDEBAR_BG,
        AttrFlags::empty(),
        area,
    );
}

fn draw_pane_title(buf: &mut CellBuffer, pane: &LogicalPane, body: Rect, focused: bool) {
    if body.y < TITLE_ROWS {
        return;
    }
    let bar = Rect::new(body.x, body.y - TITLE_ROWS, body.w, TITLE_ROWS);
    let bg = if focused { TITLE_BG_FOCUSED } else { TITLE_BG };
    let fg = if focused { Color::Indexed(255) } else { Color::Indexed(250) };
    buf.fill(bar, &Cell { bg, ..Cell::default() });
    let label = format!(" {} {} ", pane.status_glyph(), truncate(&pane.title, bar.w.saturating_sub(6) as usize));
    buf.draw_text(bar.x, bar.y, &label, fg, bg, if focused { AttrFlags::BOLD } else { AttrFlags::empty() }, bar);
    let size = format!("{}×{} ", pane.cols, pane.rows);
    let size_x = bar.right().saturating_sub(size.len() as u16);
    buf.draw_text(size_x, bar.y, &size, DIM_TEXT, bg, AttrFlags::empty(), bar);
}

fn draw_pane_badge(buf: &mut CellBuffer, pane: &LogicalPane, body: Rect) {
    let text = if pane.status == PaneStatus::Dead {
        " exited ".to_string()
    } else if pane.throttled {
        " ≫ fast output ".to_string()
    } else if pane.paused {
        " catching up… ".to_string()
    } else {
        format!(" scroll +{} ", pane.term.display_offset())
    };
    let x = body.right().saturating_sub(text.len() as u16 + 1);
    buf.draw_text(x, body.y, &text, Color::Indexed(0), Color::Indexed(214), AttrFlags::BOLD, body);
}

fn draw_hud(buf: &mut CellBuffer, metrics: &Metrics) {
    let lines = metrics.hud_lines();
    let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16 + 2;
    let h = lines.len() as u16 + 1;
    let area = buf.area();
    let rect = Rect::new(area.w.saturating_sub(w + 1), 1, w, h).intersect(&area);
    buf.fill(rect, &Cell { bg: Color::Indexed(17), ..Cell::default() });
    buf.draw_text(rect.x + 1, rect.y, "── perf ──", Color::Indexed(45), Color::Indexed(17), AttrFlags::BOLD, rect);
    for (i, line) in lines.iter().enumerate() {
        buf.draw_text(
            rect.x + 1,
            rect.y + 1 + i as u16,
            line,
            Color::Indexed(153),
            Color::Indexed(17),
            AttrFlags::empty(),
            rect,
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
