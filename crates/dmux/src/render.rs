//! Frame composition: sidebar, pane title bars (with click buttons), pane
//! bodies, overlays, and the debug HUD, painted into the back `CellBuffer`.
//! Every interactive region is registered in the frame's `ClickMap` as it is
//! drawn — whatever you see is what you can click.

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::{spinner_frame, ClickMap, Theme};

use crate::layout::{Layout, TITLE_ROWS};
use crate::metrics::Metrics;
use crate::session::{LogicalPane, PaneStatus};
use crate::sidebar::{ProjectAction, ProjectSelection};
use crate::style::{
    action_labels, agent_tag_color, group_fill_color, header_shows_active, issue_action_label,
    pane_border_fg, row_tag, sidebar_edge_highlight, sidebar_surface, title_bar_style,
};
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
    /// A selected project action row and its active action.
    pub sidebar_project: Option<&'a ProjectSelection>,
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
    /// Active sidebar reorder drag (#26): (source pane index, pointer row).
    pub reorder: Option<(usize, u16)>,
}

/// One sidebar project group, precomputed by the app.
pub struct SidebarGroup {
    pub name: String,
    #[allow(dead_code)]
    pub root: String,
    pub accent: Color,
    pub accent_soft: Color,
    pub pane_indices: Vec<usize>,
    pub issue_label: String,
    /// Owns the selected pane (or is the main project when none is).
    pub active: bool,
}

pub fn compose(buf: &mut CellBuffer, scene: &Scene<'_>, clicks: &mut ClickMap<ClickTarget>) {
    draw_sidebar(buf, scene, clicks);
    // With panes open, tint the unused content area so free space reads as
    // canvas; pane bodies repaint their own rects over it.
    if scene.panes.iter().any(|p| p.rect.is_some()) {
        let content = content_area(buf, scene.layout);
        buf.fill(
            content,
            &Cell {
                bg: scene.theme.canvas,
                ..Cell::default()
            },
        );
    }
    for (i, pane) in scene.panes.iter().enumerate() {
        let Some(rect) = pane.rect else { continue };
        draw_pane_title(buf, scene, pane, i, rect, clicks);
        pane.term.render_into(buf, rect);
        clicks.add(rect, ClickTarget::PaneBody(i));
        // Right-edge border in the gutter column: separates neighboring panes
        // and marks the pane's edge against the empty background. Ownership
        // (#38): the FOCUSED pane claims every border segment it touches —
        // including its left edge, which is drawn by the left neighbor —
        // so the active pane is outlined in its own project color all round.
        let border_x = rect.right();
        if border_x < buf.cols() {
            let focused_rect = scene.panes.get(scene.focused).and_then(|p| p.rect);
            let focused_accent = scene
                .pane_accents
                .get(scene.focused)
                .copied()
                .map(|(fa, _)| fa)
                .unwrap_or(scene.theme.accent);
            let border_fg = pane_border_fg(
                scene.theme,
                rect,
                scene.focused == i,
                focused_rect,
                focused_accent,
            );
            for row in rect.y.saturating_sub(TITLE_ROWS)..rect.bottom() {
                buf.set(
                    border_x,
                    row,
                    Cell {
                        ch: '│',
                        fg: border_fg,
                        bg: scene.theme.canvas,
                        ..Cell::default()
                    },
                );
            }
        }
        if pane.paused
            || pane.throttled
            || pane.status == PaneStatus::Dead
            || pane.term.display_offset() > 0
        {
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
    // Focused sidebar reads as the active input area: the whole surface
    // lifts to the raised background, not just the selected row (#15).
    let sb_bg = sidebar_surface(t, scene.sidebar_focused);
    buf.fill(
        area,
        &Cell {
            bg: sb_bg,
            ..Cell::default()
        },
    );

    // Header: session name between braille fills.
    let header = format!(
        "⣿⣿ {} ",
        truncate(scene.session_name, area.w.saturating_sub(6) as usize)
    );
    let end = buf.draw_text(area.x, 0, &header, t.accent, sb_bg, AttrFlags::BOLD, area);
    let mut fill = String::new();
    for _ in end..area.right() {
        fill.push('⣿');
    }
    buf.draw_text(end, 0, &fill, t.accent, sb_bg, AttrFlags::BOLD, area);

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
            let text = format!(
                "⣿ {} ",
                truncate(&group.name, area.w.saturating_sub(8) as usize)
            );
            let end = buf.draw_text(
                area.x,
                row,
                &text,
                group.accent,
                sb_bg,
                AttrFlags::BOLD,
                area,
            );
            let mut fill = String::new();
            for _ in end..area.right() {
                fill.push('⣿');
            }
            buf.draw_text(
                end,
                row,
                &fill,
                group_fill_color(group),
                sb_bg,
                AttrFlags::empty(),
                area,
            );
            row += 1;
        }
        for &i in &group.pane_indices {
            if row >= bottom_limit {
                break;
            }
            let pane = &scene.panes[i];
            let selected = pane_is_selected(i, scene.selected, scene.sidebar_project.is_some());
            let focused = i == scene.focused;
            let caret = if selected { "▸" } else { " " };
            let glyph = if pane.closing {
                spinner_frame(scene.anim).to_string()
            } else {
                status_glyph(pane, scene.anim)
            };
            let attn = if pane.needs_attention && !pane.closing {
                "!"
            } else {
                " "
            };
            let hidden_tag = row_tag(pane.closing, pane.hidden);
            let name = truncate(
                pane.display_title(),
                area.w.saturating_sub(14 + hidden_tag.len() as u16) as usize,
            );
            let line = format!("{caret}{attn}{glyph} {name}{hidden_tag}");
            // Reorder drag (#26): the row under the pointer is the insertion
            // target (selection surface + marker below); the dragged source
            // row dims so the movement reads.
            let reorder_target = scene.reorder.map(|(_, pr)| pr == row).unwrap_or(false);
            let reorder_src = scene.reorder.map(|(srci, _)| srci == i).unwrap_or(false);
            let (fg, attrs) = if pane.closing || pane.hidden {
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
            let fg = if reorder_src && !reorder_target {
                t.text_faint
            } else {
                fg
            };
            let bg = if selected || reorder_target {
                t.bg_selected
            } else {
                sb_bg
            };
            let row_rect = Rect::new(area.x, row, area.w, 1);
            if selected || reorder_target {
                buf.fill(
                    row_rect,
                    &Cell {
                        bg,
                        ..Cell::default()
                    },
                );
            }
            let end = buf.draw_text(area.x, row, &line, fg, bg, attrs, area);
            if selected && scene.sidebar_focused {
                buf.set(
                    area.x,
                    row,
                    Cell {
                        ch: '▍',
                        fg: group.accent,
                        bg,
                        ..Cell::default()
                    },
                );
            }
            // Reorder drag (#26): accent marker on the insertion target row.
            if reorder_target {
                buf.set(
                    area.x,
                    row,
                    Cell {
                        ch: '➤',
                        fg: group.accent,
                        bg,
                        ..Cell::default()
                    },
                );
            }
            if let Some(agent) = &pane.agent {
                let short = crate::agents::agent(agent).map(|d| d.short).unwrap_or("??");
                let tag = format!("[{short}]");
                let tag_x = area.right().saturating_sub(tag.len() as u16 + 1);
                if tag_x > end {
                    buf.draw_text(
                        tag_x,
                        row,
                        &tag,
                        agent_tag_color(t),
                        bg,
                        AttrFlags::empty(),
                        area,
                    );
                }
            }
            clicks.add(row_rect, ClickTarget::SidebarRow(i));
            row += 1;
        }
        // Per-project creation actions, right-aligned like the TS sidebar;
        // the active project shows its hotkeys.
        if row < bottom_limit {
            let (na, term) = action_labels(group.active, scene.sidebar_focused);
            let total = na.chars().count() as u16 + term.chars().count() as u16 + 2;
            let x0 = area.right().saturating_sub(total + 1);
            let color = if group.active {
                group.accent
            } else {
                t.text_faint
            };
            let selected_action = scene
                .sidebar_project
                .filter(|project| project.root == group.root)
                .map(|project| project.action);
            let issue_max = x0.saturating_sub(area.x + 2) as usize;
            let issue_action =
                issue_action_label(&group.issue_label, group.active, scene.sidebar_focused);
            let issue_label = truncate(&issue_action, issue_max);
            let ix = area.x + 1;
            let issue_end = draw_sidebar_action(
                buf,
                ix,
                row,
                &issue_label,
                selected_action == Some(ProjectAction::Issues),
                color,
                group.accent,
                sb_bg,
                t,
                area,
            );
            clicks.add(
                Rect::new(ix, row, issue_end.saturating_sub(ix), 1),
                ClickTarget::SidebarGroupIssues(gi),
            );
            let ax = draw_sidebar_action(
                buf,
                x0,
                row,
                &na,
                selected_action == Some(ProjectAction::NewAgent),
                color,
                group.accent,
                sb_bg,
                t,
                area,
            );
            clicks.add(
                Rect::new(x0, row, ax - x0, 1),
                ClickTarget::SidebarGroupNewAgent(gi),
            );
            let tx = draw_sidebar_action(
                buf,
                ax + 2,
                row,
                &term,
                selected_action == Some(ProjectAction::NewTerminal),
                color,
                group.accent,
                sb_bg,
                t,
                area,
            );
            clicks.add(
                Rect::new(ax + 2, row, tx - ax - 2, 1),
                ClickTarget::SidebarGroupNewTerminal(gi),
            );
            row += 1;
        }
        // Breathing room between groups.
        if multi {
            row += 1;
        }
    }

    // Right-hand sidebar border. The rows alongside a focused pane that
    // starts at the sidebar are that pane's LEFT edge — they take its
    // project accent so the active outline is complete (#39); the rest of
    // the column keeps the neutral border.
    let border_x = area.right();
    if border_x < buf.cols() {
        let focused_rect = scene.panes.get(scene.focused).and_then(|p| p.rect);
        let highlight = sidebar_edge_highlight(border_x, focused_rect);
        let focused_accent = scene
            .pane_accents
            .get(scene.focused)
            .map(|(a, _)| *a)
            .unwrap_or(t.accent);
        for y in 0..area.bottom() {
            let fg = match highlight {
                Some((top, bottom)) if y >= top && y < bottom => focused_accent,
                _ => t.border,
            };
            buf.set(
                border_x,
                y,
                Cell {
                    ch: '│',
                    fg,
                    bg: sb_bg,
                    ..Cell::default()
                },
            );
        }
    }

    // Footer utility block (#40): a full-width divider separates the pane
    // list from three global utilities — project creation, settings, and
    // shortcuts. Agent/terminal creation lives in the per-project action
    // rows, never here. Skipped entirely on very short sidebars so rows
    // and metadata keep priority.
    if area.h >= 10 {
        let divider_row = area.bottom().saturating_sub(4);
        let dash: String = "─".repeat(area.w as usize);
        buf.draw_text(
            area.x,
            divider_row,
            &dash,
            t.border,
            sb_bg,
            AttrFlags::empty(),
            area,
        );

        let utility_row = area.bottom().saturating_sub(3);
        let px = buf.draw_text(
            area.x + 1,
            utility_row,
            "+ project",
            t.accent,
            sb_bg,
            AttrFlags::BOLD,
            area,
        );
        clicks.add(
            Rect::new(area.x + 1, utility_row, px - area.x - 1, 1),
            ClickTarget::SidebarNewProject,
        );
        let sx = buf.draw_text(
            px + 2,
            utility_row,
            "⚙ settings",
            t.text_dim,
            sb_bg,
            AttrFlags::empty(),
            area,
        );
        clicks.add(
            Rect::new(px + 2, utility_row, sx - px - 2, 1),
            ClickTarget::SidebarSettings,
        );
        let hx = buf.draw_text(
            sx + 2,
            utility_row,
            "? shortcuts",
            t.text_dim,
            sb_bg,
            AttrFlags::empty(),
            area,
        );
        clicks.add(
            Rect::new(sx + 2, utility_row, hx - sx - 2, 1),
            ClickTarget::SidebarHelp,
        );
    }

    // Build + auto-filed issues (first-party diagnostics ring).
    let ver_row = area.bottom().saturating_sub(2);
    let vx = buf.draw_text(
        area.x + 1,
        ver_row,
        scene.version,
        t.text_faint,
        sb_bg,
        AttrFlags::empty(),
        area,
    );
    let (total, fresh) = scene.issues;
    if total > 0 {
        let label = if fresh > 0 {
            format!(" · 🐛 {total} ({fresh} new)")
        } else {
            format!(" · 🐛 {total}")
        };
        let color = if fresh > 0 { t.warn } else { t.text_faint };
        let ix = buf.draw_text(vx, ver_row, &label, color, sb_bg, AttrFlags::empty(), area);
        clicks.add(
            Rect::new(vx, ver_row, ix.saturating_sub(vx), 1),
            ClickTarget::SidebarIssues,
        );
    }

    // Footer: leader hint or status.
    let footer_row = area.bottom().saturating_sub(1);
    let footer = if scene.leader_armed {
        crate::input::LEADER_HINT
    } else {
        scene.status_line
    };
    let footer_fg = if scene.leader_armed {
        t.warn
    } else {
        t.text_faint
    };
    buf.draw_text(
        area.x,
        footer_row,
        &truncate(footer, area.w as usize),
        footer_fg,
        sb_bg,
        AttrFlags::empty(),
        area,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_sidebar_action(
    buf: &mut CellBuffer,
    x: u16,
    row: u16,
    label: &str,
    selected: bool,
    color: Color,
    accent: Color,
    background: Color,
    theme: &Theme,
    area: Rect,
) -> u16 {
    let (fg, bg, attrs) = if selected {
        (accent, theme.bg_selected, AttrFlags::BOLD)
    } else {
        (color, background, AttrFlags::empty())
    };
    buf.draw_text(x, row, label, fg, bg, attrs, area)
}

fn pane_is_selected(index: usize, selected: usize, project_selected: bool) -> bool {
    !project_selected && index == selected
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
    let selected = pane_is_selected(idx, scene.selected, scene.sidebar_project.is_some());
    let (pa, ps) = scene
        .pane_accents
        .get(idx)
        .copied()
        .unwrap_or((t.accent, t.accent_soft));
    let bar = Rect::new(body.x, body.y - TITLE_ROWS, body.w, TITLE_ROWS);
    // #21: sidebar navigation previews the full active header on the
    // selected pane — input focus doesn't move until Enter.
    let full = header_shows_active(focused, selected, scene.sidebar_focused);
    let (fg, bg) = title_bar_style(t, (pa, ps), full, selected);
    buf.fill(
        bar,
        &Cell {
            bg,
            ..Cell::default()
        },
    );

    let glyph = status_glyph(pane, scene.anim);
    let label = format!(
        " {glyph} {} ",
        truncate(pane.display_title(), bar.w.saturating_sub(16) as usize)
    );
    let attrs = if full || selected {
        AttrFlags::BOLD
    } else {
        AttrFlags::empty()
    };
    buf.draw_text(bar.x, bar.y, &label, fg, bg, attrs, bar);
    if selected && !focused {
        // Mirror the sidebar's selection bar so the eye can pair them (#13).
        buf.set(
            bar.x,
            bar.y,
            Cell {
                ch: '▍',
                fg: pa,
                bg,
                ..Cell::default()
            },
        );
    }
    clicks.add(bar, ClickTarget::PaneTitle(idx));

    // Right side: size + macOS-style traffic lights on the bar itself —
    // green rename, yellow hide, red close. Vivid on the focused pane, dim
    // dots elsewhere; each gets a 2-cell click target.
    let size = format!("{}×{}", pane.cols, pane.rows);
    let dots = [
        (
            ClickTarget::TitleRename(idx),
            Color::Rgb(0x2e, 0xc2, 0x4e),
            Color::Indexed(65),
        ),
        (
            ClickTarget::TitleHide(idx),
            Color::Rgb(0xfe, 0xbc, 0x2e),
            Color::Indexed(136),
        ),
        (
            ClickTarget::TitleClose(idx),
            Color::Rgb(0xff, 0x5f, 0x57),
            Color::Indexed(131),
        ),
    ];
    let dots_w = dots.len() as u16 * 2 + 1;
    let total_w = size.chars().count() as u16 + 2 + dots_w;
    let mut x = bar.right().saturating_sub(total_w);
    x = buf.draw_text(
        x,
        bar.y,
        &size,
        if focused {
            Color::Indexed(255)
        } else {
            t.text_faint
        },
        bg,
        AttrFlags::empty(),
        bar,
    );
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
    buf.draw_text(
        x,
        body.y,
        &text,
        Color::Indexed(0),
        theme.warn,
        AttrFlags::BOLD,
        body,
    );
}

fn draw_hud(buf: &mut CellBuffer, metrics: &Metrics) {
    let lines = metrics.hud_lines();
    let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16 + 2;
    let h = lines.len() as u16 + 1;
    let area = buf.area();
    let rect = Rect::new(area.w.saturating_sub(w + 1), 1, w, h).intersect(&area);
    buf.fill(
        rect,
        &Cell {
            bg: Color::Indexed(17),
            ..Cell::default()
        },
    );
    buf.draw_text(
        rect.x + 1,
        rect.y,
        "── perf ──",
        Color::Indexed(45),
        Color::Indexed(17),
        AttrFlags::BOLD,
        rect,
    );
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
mod action_tests;
#[cfg(test)]
mod tests {
    use super::*;

    fn footer_scene<'a>(
        layout: &'a Layout,
        groups: &'a [SidebarGroup],
        theme: &'a Theme,
    ) -> Scene<'a> {
        Scene {
            panes: &[],
            layout,
            focused: 0,
            selected: 0,
            session_name: "s",
            project_name: "p",
            hud: None,
            status_line: "",
            theme,
            anim: 0,
            leader_armed: false,
            sidebar_focused: false,
            version: "v0.0.0",
            issues: (0, 0),
            groups,
            pane_accents: &[],
            reorder: None,
            sidebar_project: None,
        }
    }

    fn buffer_text(buf: &CellBuffer, w: u16, h: u16) -> String {
        let mut out = String::new();
        for row in 0..h {
            for col in 0..w {
                out.push(buf.get(col, row).ch);
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn footer_holds_three_utilities_only() {
        // #40: New Project, Settings, Shortcuts — never agent/terminal —
        // above a full-width divider, with live click targets.
        let theme = Theme::named("violet");
        let layout = Layout {
            sidebar: Rect::new(0, 0, 40, 30),
            ..Default::default()
        };
        let mut buf = CellBuffer::new(40, 30);
        let mut clicks = ClickMap::new();
        draw_sidebar(&mut buf, &footer_scene(&layout, &[], &theme), &mut clicks);
        let text = buffer_text(&buf, 40, 30);
        assert!(text.contains("+ project"));
        assert!(text.contains("⚙ settings"));
        assert!(text.contains("? shortcuts"));
        assert!(!text.contains("+ agent"), "global agent link removed");
        assert!(!text.contains("+ terminal"), "global terminal link removed");
        // Divider row above the utilities.
        assert!(text.lines().nth(26).unwrap_or("").starts_with("────"));
        // Click targets resolve on the utility row.
        assert!(matches!(
            clicks.hit(2, 27),
            Some(ClickTarget::SidebarNewProject)
        ));
        assert!(matches!(
            clicks.hit(13, 27),
            Some(ClickTarget::SidebarSettings)
        ));
        assert!(matches!(clicks.hit(25, 27), Some(ClickTarget::SidebarHelp)));
    }

    #[test]
    fn footer_utilities_skip_very_short_sidebars() {
        // #40: below the height budget the utility block yields to rows and
        // metadata instead of overlapping them.
        let theme = Theme::named("violet");
        let layout = Layout {
            sidebar: Rect::new(0, 0, 40, 8),
            ..Default::default()
        };
        let mut buf = CellBuffer::new(40, 8);
        let mut clicks = ClickMap::new();
        draw_sidebar(&mut buf, &footer_scene(&layout, &[], &theme), &mut clicks);
        let text = buffer_text(&buf, 40, 8);
        assert!(!text.contains("+ project"));
        assert!(!text.contains("⚙ settings"));
    }
}
