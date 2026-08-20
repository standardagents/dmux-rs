//! Frame composition: sidebar, pane title bars (with click buttons), pane
//! bodies, overlays, and the debug HUD, painted into the back `CellBuffer`.
//! Every interactive region is registered in the frame's `ClickMap` as it is
//! drawn — whatever you see is what you can click.

mod footer;

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::{spinner_frame, ClickMap, Theme};

use crate::layout::{Layout, TITLE_ROWS};
use crate::metrics::Metrics;
use crate::session::{LogicalPane, PaneStatus};
use crate::sidebar::ProjectSelection;
use crate::style::{header_shows_active, pane_border_fg, title_bar_style};
use crate::views::ClickTarget;
mod sidebar_render;
pub(crate) use self::sidebar_render::{
    active_target, draw_sidebar, draw_sidebar_action, pane_is_selected,
};

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
    pub hovered: Option<ClickTarget>,
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

pub(super) fn status_glyph(pane: &LogicalPane, anim: u64) -> String {
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
    let selected = active_target(
        scene.hovered,
        ClickTarget::PaneTitle(idx),
        pane_is_selected(idx, scene.selected, scene.sidebar_project.is_some()),
    );
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
        let hovered = scene.hovered == Some(target);
        let fg = if focused || hovered { vivid } else { dim };
        let bx = x;
        let hit = Rect::new(bx, bar.y, 2, 1);
        let dot_bg = if hovered { t.bg_selected } else { bg };
        if hovered {
            buf.fill(
                hit,
                &Cell {
                    bg: dot_bg,
                    ..Cell::default()
                },
            );
        }
        x = buf.draw_text(
            x,
            bar.y,
            "●",
            fg,
            dot_bg,
            if hovered {
                AttrFlags::BOLD
            } else {
                AttrFlags::empty()
            },
            bar,
        );
        clicks.add(hit, target);
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

pub(crate) fn truncate(s: &str, max: usize) -> String {
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
mod sidebar_preview;
#[cfg(test)]
mod title_tests;
