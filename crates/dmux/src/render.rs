//! Frame composition: sidebar, pane title bars (with click buttons), pane
//! bodies, overlays, and the debug HUD, painted into the back `CellBuffer`.
//! Every interactive region is registered in the frame's `ClickMap` as it is
//! drawn — whatever you see is what you can click.

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::{spinner_frame, ClickMap, Theme};

use crate::layout::{Layout, TITLE_ROWS};
use crate::metrics::Metrics;
use crate::session::{LogicalPane, PaneStatus};
use crate::views::ClickTarget;

pub struct Scene<'a> {
    pub panes: &'a [LogicalPane],
    pub layout: &'a Layout,
    pub focused: usize,
    pub selected: usize,
    pub session_name: &'a str,
    pub hud: Option<&'a Metrics>,
    pub status_line: &'a str,
    pub theme: &'a Theme,
    pub anim: u64,
    pub leader_armed: bool,
}

pub fn compose(buf: &mut CellBuffer, scene: &Scene<'_>, clicks: &mut ClickMap<ClickTarget>) {
    draw_sidebar(buf, scene, clicks);
    for (i, pane) in scene.panes.iter().enumerate() {
        let Some(rect) = pane.rect else { continue };
        draw_pane_title(buf, scene, pane, i, rect, clicks);
        pane.term.render_into(buf, rect);
        clicks.add(rect, ClickTarget::PaneBody(i));
        if pane.paused || pane.throttled || pane.status == PaneStatus::Dead || pane.term.display_offset() > 0 {
            draw_pane_badge(buf, scene.theme, pane, rect);
        }
    }
    if let Some(metrics) = scene.hud {
        draw_hud(buf, metrics);
    }
}

/// Content area to the right of the sidebar (the welcome screen's canvas).
pub fn content_area(buf: &CellBuffer, layout: &Layout) -> Rect {
    let x = layout.sidebar.right() + 1;
    Rect::new(x, 0, buf.cols().saturating_sub(x), buf.rows())
}

fn draw_sidebar(buf: &mut CellBuffer, scene: &Scene<'_>, clicks: &mut ClickMap<ClickTarget>) {
    let t = scene.theme;
    let area = scene.layout.sidebar;
    if area.is_empty() {
        return;
    }
    buf.fill(area, &Cell { bg: t.bg, ..Cell::default() });

    // Header: session name between braille fills.
    let header = format!("⣿⣿ {} ", truncate(scene.session_name, area.w.saturating_sub(6) as usize));
    let end = buf.draw_text(area.x, 0, &header, t.accent, t.bg, AttrFlags::BOLD, area);
    let mut fill = String::new();
    for _ in end..area.right() {
        fill.push('⣿');
    }
    buf.draw_text(end, 0, &fill, t.accent, t.bg, AttrFlags::BOLD, area);

    // Pane rows.
    let mut row = 2u16;
    for (i, pane) in scene.panes.iter().enumerate() {
        if row >= area.bottom().saturating_sub(5) {
            break;
        }
        let selected = i == scene.selected;
        let focused = i == scene.focused;
        let caret = if selected { "▸" } else { " " };
        let glyph = status_glyph(pane, scene.anim);
        let hidden_tag = if pane.hidden { " (hidden)" } else { "" };
        let name = truncate(pane.display_title(), area.w.saturating_sub(12 + hidden_tag.len() as u16) as usize);
        let line = format!("{caret} {glyph} {name}{hidden_tag}");
        let (fg, attrs) = if pane.hidden {
            (t.text_faint, AttrFlags::empty())
        } else if focused {
            (t.accent, AttrFlags::BOLD)
        } else if selected {
            (t.text, AttrFlags::empty())
        } else {
            (t.text_dim, AttrFlags::empty())
        };
        let bg = if selected { t.bg_selected } else { t.bg };
        let row_rect = Rect::new(area.x, row, area.w, 1);
        if selected {
            buf.fill(row_rect, &Cell { bg, ..Cell::default() });
        }
        let end = buf.draw_text(area.x, row, &line, fg, bg, attrs, area);
        if let Some(agent) = &pane.agent {
            let short = crate::agents::agent(agent).map(|d| d.short).unwrap_or("??");
            let tag = format!("[{short}]");
            let tag_x = area.right().saturating_sub(tag.len() as u16 + 1);
            if tag_x > end {
                buf.draw_text(tag_x, row, &tag, t.text_faint, bg, AttrFlags::empty(), area);
            }
        }
        clicks.add(row_rect, ClickTarget::SidebarRow(i));
        row += 1;
    }

    // Action rows: always-visible click targets so nothing needs a manual.
    let actions_row = area.bottom().saturating_sub(4);
    let x = buf.draw_text(area.x + 1, actions_row, "+ agent", t.accent, t.bg, AttrFlags::BOLD, area);
    clicks.add(Rect::new(area.x + 1, actions_row, x - area.x - 1, 1), ClickTarget::SidebarNewAgent);
    let x2 = buf.draw_text(x + 2, actions_row, "+ terminal", t.text_dim, t.bg, AttrFlags::empty(), area);
    clicks.add(Rect::new(x + 2, actions_row, x2 - x - 2, 1), ClickTarget::SidebarNewTerminal);
    let x3 = buf.draw_text(x2 + 2, actions_row, "+ proj", t.text_dim, t.bg, AttrFlags::empty(), area);
    clicks.add(Rect::new(x2 + 2, actions_row, x3 - x2 - 2, 1), ClickTarget::SidebarNewProject);

    let tools_row = area.bottom().saturating_sub(3);
    let sx = buf.draw_text(area.x + 1, tools_row, "⚙ settings", t.text_dim, t.bg, AttrFlags::empty(), area);
    clicks.add(Rect::new(area.x + 1, tools_row, sx - area.x - 1, 1), ClickTarget::SidebarSettings);
    let hx = buf.draw_text(sx + 2, tools_row, "? shortcuts", t.text_dim, t.bg, AttrFlags::empty(), area);
    clicks.add(Rect::new(sx + 2, tools_row, hx - sx - 2, 1), ClickTarget::SidebarHelp);

    // Footer: leader hint or status.
    let footer_row = area.bottom().saturating_sub(1);
    let footer = if scene.leader_armed {
        crate::input::LEADER_HINT
    } else {
        scene.status_line
    };
    let footer_fg = if scene.leader_armed { t.warn } else { t.text_faint };
    buf.draw_text(area.x, footer_row, &truncate(footer, area.w as usize), footer_fg, t.bg, AttrFlags::empty(), area);
}

fn status_glyph(pane: &LogicalPane, anim: u64) -> String {
    match pane.status {
        PaneStatus::Working => spinner_frame(anim).to_string(),
        PaneStatus::Idle => "◌".to_string(),
        PaneStatus::Dead => "✗".to_string(),
    }
}

fn draw_pane_title(
    buf: &mut CellBuffer,
    scene: &Scene<'_>,
    pane: &LogicalPane,
    idx: usize,
    body: Rect,
    clicks: &mut ClickMap<ClickTarget>,
) {
    let t = scene.theme;
    if body.y < TITLE_ROWS {
        return;
    }
    let focused = idx == scene.focused;
    let bar = Rect::new(body.x, body.y - TITLE_ROWS, body.w, TITLE_ROWS);
    let bg = if focused { t.accent_soft } else { t.bg_raised };
    let fg = if focused { Color::Indexed(255) } else { t.text_dim };
    buf.fill(bar, &Cell { bg, ..Cell::default() });

    let glyph = status_glyph(pane, scene.anim);
    let label = format!(" {glyph} {} ", truncate(pane.display_title(), bar.w.saturating_sub(16) as usize));
    buf.draw_text(bar.x, bar.y, &label, fg, bg, if focused { AttrFlags::BOLD } else { AttrFlags::empty() }, bar);
    clicks.add(bar, ClickTarget::PaneTitle(idx));

    // Right side: size + the affordances tmux could never give us — rename,
    // hide, close buttons on every pane header.
    let size = format!("{}×{}", pane.cols, pane.rows);
    let buttons = ["✎", "–", "✕"];
    let btn_targets = [
        ClickTarget::TitleRename(idx),
        ClickTarget::TitleHide(idx),
        ClickTarget::TitleClose(idx),
    ];
    let total_w = size.chars().count() as u16 + 2 + buttons.len() as u16 * 2 + 1;
    let mut x = bar.right().saturating_sub(total_w);
    x = buf.draw_text(x, bar.y, &size, t.text_faint, bg, AttrFlags::empty(), bar);
    x += 2;
    for (b, target) in buttons.iter().zip(btn_targets) {
        let bx = x;
        let btn_fg = match target {
            ClickTarget::TitleClose(_) if focused => t.danger,
            _ if focused => Color::Indexed(255),
            _ => t.text_faint,
        };
        x = buf.draw_text(x, bar.y, b, btn_fg, bg, AttrFlags::empty(), bar);
        clicks.add(Rect::new(bx, bar.y, x - bx + 1, 1), target);
        x += 1;
    }
}

fn draw_pane_badge(buf: &mut CellBuffer, theme: &Theme, pane: &LogicalPane, body: Rect) {
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
    buf.draw_text(x, body.y, &text, Color::Indexed(0), theme.warn, AttrFlags::BOLD, body);
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
