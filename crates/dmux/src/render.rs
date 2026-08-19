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
    #[allow(dead_code)]
    pub project_name: &'a str,
    pub hud: Option<&'a Metrics>,
    pub status_line: &'a str,
    pub theme: &'a Theme,
    pub anim: u64,
    pub leader_armed: bool,
    /// The sidebar holds keyboard focus: selection renders with the accent
    /// bar so the active area is unmistakable.
    pub sidebar_focused: bool,
    /// Build identity (sidebar bottom line).
    pub version: &'a str,
    /// (total filed issues, filed this session) for the sidebar bottom line.
    pub issues: (usize, usize),
    /// Project groups in TS order (main first, then config, then
    /// pane-derived), with their resolved colors.
    pub groups: &'a [SidebarGroup],
    /// Per-pane (accent, soft) from the owning project's color theme;
    /// parallel to `panes`.
    pub pane_accents: &'a [(Color, Color)],
}

/// One sidebar project group, precomputed by the app.
pub struct SidebarGroup {
    pub name: String,
    #[allow(dead_code)]
    pub root: String,
    pub accent: Color,
    pub accent_soft: Color,
    pub pane_indices: Vec<usize>,
    /// Owns the selected pane (or is the main project when none is).
    pub active: bool,
}

pub fn compose(buf: &mut CellBuffer, scene: &Scene<'_>, clicks: &mut ClickMap<ClickTarget>) {
    draw_sidebar(buf, scene, clicks);
    // With panes open, tint the unused content area so free space reads as
    // canvas; pane bodies repaint their own rects over it.
    if scene.panes.iter().any(|p| p.rect.is_some()) {
        let content = content_area(buf, scene.layout);
        buf.fill(content, &Cell { bg: scene.theme.canvas, ..Cell::default() });
    }
    for (i, pane) in scene.panes.iter().enumerate() {
        let Some(rect) = pane.rect else { continue };
        draw_pane_title(buf, scene, pane, i, rect, clicks);
        pane.term.render_into(buf, rect);
        clicks.add(rect, ClickTarget::PaneBody(i));
        // Right-edge border in the gutter column: separates neighboring panes
        // and marks the pane's edge against the empty background, colored by
        // the pane's project.
        let border_x = rect.right();
        if border_x < buf.cols() {
            let (pa, ps) = scene.pane_accents.get(i).copied().unwrap_or((scene.theme.accent, scene.theme.border));
            let border_fg = if i == scene.focused { pa } else { ps };
            for row in rect.y.saturating_sub(TITLE_ROWS)..rect.bottom() {
                buf.set(
                    border_x,
                    row,
                    Cell { ch: '│', fg: border_fg, bg: scene.theme.canvas, ..Cell::default() },
                );
            }
        }
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

    // Project groups (TS parity: main project first, per-group colors,
    // per-group creation actions, blank spacing between groups).
    let multi = scene.groups.len() > 1;
    let mut row = 2u16;
    let bottom_limit = area.bottom().saturating_sub(5);
    for (gi, group) in scene.groups.iter().enumerate() {
        if row >= bottom_limit {
            break;
        }
        if multi {
            let text = format!("⣿ {} ", truncate(&group.name, area.w.saturating_sub(8) as usize));
            let end = buf.draw_text(area.x, row, &text, group.accent, t.bg, AttrFlags::BOLD, area);
            let mut fill = String::new();
            for _ in end..area.right() {
                fill.push('⣿');
            }
            buf.draw_text(end, row, &fill, group.accent_soft, t.bg, AttrFlags::empty(), area);
            row += 1;
        }
        for &i in &group.pane_indices {
            if row >= bottom_limit {
                break;
            }
            let pane = &scene.panes[i];
            let selected = i == scene.selected;
            let focused = i == scene.focused;
            let caret = if selected { "▸" } else { " " };
            let glyph = status_glyph(pane, scene.anim);
            let attn = if pane.needs_attention { "!" } else { " " };
            let hidden_tag = if pane.hidden { " (hidden)" } else { "" };
            let ap_tag = if pane.autopilot { " (ap)" } else { "" };
            let name = truncate(
                pane.display_title(),
                area.w.saturating_sub(14 + hidden_tag.len() as u16 + ap_tag.len() as u16) as usize,
            );
            let line = format!("{caret}{attn}{glyph} {name}{hidden_tag}");
            let (fg, attrs) = if pane.hidden {
                (t.text_faint, AttrFlags::empty())
            } else if focused {
                (group.accent, AttrFlags::BOLD)
            } else if pane.status == PaneStatus::Waiting || pane.needs_attention {
                (t.warn, AttrFlags::empty())
            } else if group.active {
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
            if !ap_tag.is_empty() {
                buf.draw_text(end, row, ap_tag, group.accent, bg, AttrFlags::empty(), area);
            }
            if selected && scene.sidebar_focused {
                buf.set(area.x, row, Cell { ch: '▍', fg: group.accent, bg, ..Cell::default() });
            }
            if let Some(agent) = &pane.agent {
                let short = crate::agents::agent(agent).map(|d| d.short).unwrap_or("??");
                let tag = format!("[{short}]");
                let tag_x = area.right().saturating_sub(tag.len() as u16 + 1);
                if tag_x > end {
                    buf.draw_text(tag_x, row, &tag, group.accent_soft, bg, AttrFlags::empty(), area);
                }
            }
            clicks.add(row_rect, ClickTarget::SidebarRow(i));
            row += 1;
        }
        // Per-project creation actions, right-aligned like the TS sidebar;
        // the active project shows its hotkeys.
        if row < bottom_limit {
            let (na, term) = if group.active {
                ("[n]ew agent".to_string(), "[t]erminal".to_string())
            } else {
                ("new agent".to_string(), "terminal".to_string())
            };
            let total = na.chars().count() as u16 + term.chars().count() as u16 + 2;
            let x0 = area.right().saturating_sub(total + 1);
            let color = if group.active { group.accent } else { t.text_faint };
            let ax = buf.draw_text(x0, row, &na, color, t.bg, AttrFlags::empty(), area);
            clicks.add(Rect::new(x0, row, ax - x0, 1), ClickTarget::SidebarGroupNewAgent(gi));
            let tx = buf.draw_text(ax + 2, row, &term, color, t.bg, AttrFlags::empty(), area);
            clicks.add(Rect::new(ax + 2, row, tx - ax - 2, 1), ClickTarget::SidebarGroupNewTerminal(gi));
            row += 1;
        }
        // Breathing room between groups.
        if multi {
            row += 1;
        }
    }

    // Right-hand sidebar border.
    let border_x = area.right();
    if border_x < buf.cols() {
        for y in 0..area.bottom() {
            buf.set(border_x, y, Cell { ch: '│', fg: t.border, bg: t.bg, ..Cell::default() });
        }
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

    // Build + auto-filed issues (first-party diagnostics ring).
    let ver_row = area.bottom().saturating_sub(2);
    let vx = buf.draw_text(area.x + 1, ver_row, scene.version, t.text_faint, t.bg, AttrFlags::empty(), area);
    let (total, fresh) = scene.issues;
    if total > 0 {
        let label = if fresh > 0 {
            format!(" · 🐛 {total} ({fresh} new)")
        } else {
            format!(" · 🐛 {total}")
        };
        let color = if fresh > 0 { t.warn } else { t.text_faint };
        let ix = buf.draw_text(vx, ver_row, &label, color, t.bg, AttrFlags::empty(), area);
        clicks.add(Rect::new(vx, ver_row, ix.saturating_sub(vx), 1), ClickTarget::SidebarIssues);
    }

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

/// Title-bar colors: focus (activation) and sidebar selection are distinct
/// states (#13). Focused wins with the solid soft-accent band; a pane
/// selected in the sidebar gets the sidebar's neutral selection surface
/// with accent text; everything else sits on the raised surface.
fn title_bar_style(
    theme: &Theme,
    (accent, accent_soft): (Color, Color),
    focused: bool,
    selected: bool,
) -> (Color, Color) {
    if focused {
        (Color::Indexed(255), accent_soft)
    } else if selected {
        (accent, theme.bg_selected)
    } else {
        (accent, theme.bg_raised)
    }
}

fn status_glyph(pane: &LogicalPane, anim: u64) -> String {
    match pane.status {
        PaneStatus::Working => spinner_frame(anim).to_string(),
        PaneStatus::Waiting => "△".to_string(),
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
    let selected = idx == scene.selected;
    let (pa, ps) = scene.pane_accents.get(idx).copied().unwrap_or((t.accent, t.accent_soft));
    let bar = Rect::new(body.x, body.y - TITLE_ROWS, body.w, TITLE_ROWS);
    let (fg, bg) = title_bar_style(t, (pa, ps), focused, selected);
    buf.fill(bar, &Cell { bg, ..Cell::default() });

    let glyph = status_glyph(pane, scene.anim);
    let label = format!(" {glyph} {} ", truncate(pane.display_title(), bar.w.saturating_sub(16) as usize));
    let attrs = if focused || selected { AttrFlags::BOLD } else { AttrFlags::empty() };
    buf.draw_text(bar.x, bar.y, &label, fg, bg, attrs, bar);
    if selected && !focused {
        // Mirror the sidebar's selection bar so the eye can pair them (#13).
        buf.set(bar.x, bar.y, Cell { ch: '▍', fg: pa, bg, ..Cell::default() });
    }
    clicks.add(bar, ClickTarget::PaneTitle(idx));

    // Right side: size + macOS-style traffic lights on the bar itself —
    // green rename, yellow hide, red close. Vivid on the focused pane, dim
    // dots elsewhere; each gets a 2-cell click target.
    let size = format!("{}×{}", pane.cols, pane.rows);
    let dots = [
        (ClickTarget::TitleRename(idx), Color::Rgb(0x2e, 0xc2, 0x4e), Color::Indexed(65)),
        (ClickTarget::TitleHide(idx), Color::Rgb(0xfe, 0xbc, 0x2e), Color::Indexed(136)),
        (ClickTarget::TitleClose(idx), Color::Rgb(0xff, 0x5f, 0x57), Color::Indexed(131)),
    ];
    let dots_w = dots.len() as u16 * 2 + 1;
    let total_w = size.chars().count() as u16 + 2 + dots_w;
    let mut x = bar.right().saturating_sub(total_w);
    x = buf.draw_text(x, bar.y, &size, if focused { Color::Indexed(255) } else { t.text_faint }, bg, AttrFlags::empty(), bar);
    x += 2;
    for (target, vivid, dim) in dots {
        let fg = if focused { vivid } else { dim };
        let bx = x;
        x = buf.draw_text(x, bar.y, "●", fg, bg, AttrFlags::empty(), bar);
        clicks.add(Rect::new(bx, bar.y, 2, 1), target);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_and_focus_are_distinct_states() {
        // #13: sidebar selection must be visible on the body pane without
        // stealing activation's treatment.
        let theme = Theme::named("violet");
        let accents = (theme.accent, theme.accent_soft);
        let focused = title_bar_style(&theme, accents, true, false);
        let selected = title_bar_style(&theme, accents, false, true);
        let plain = title_bar_style(&theme, accents, false, false);
        assert_ne!(focused, selected, "selection must not look like focus");
        assert_ne!(selected, plain, "selection must be visible");
        assert_ne!(focused, plain);
        // Focus wins when a pane is both focused and selected.
        assert_eq!(title_bar_style(&theme, accents, true, true), focused);
        // Selection uses the sidebar's selection surface for pairing.
        assert_eq!(selected.1, theme.bg_selected);
    }
}
