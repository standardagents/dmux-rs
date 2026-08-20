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
    #[allow(dead_code)]
    pub project_name: &'a str,
    pub hud: Option<&'a Metrics>,
    /// Dragged HUD origin (#103); None = default top-right anchor.
    pub hud_pos: Option<(u16, u16)>,
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
    // With panes open, texture the unused content area with the dim dot
    // grid (#90, matching the TS spacer pane) so free space reads as
    // canvas; pane bodies, titles, and borders repaint their own cells
    // over it, leaving dots only where nothing else is drawn.
    if scene.panes.iter().any(|p| p.rect.is_some()) {
        let content = content_area(buf, scene.layout);
        buf.fill(
            content,
            &Cell {
                ch: '·',
                fg: scene.theme.canvas_dot,
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
        draw_hud(buf, metrics, scene.hud_pos, clicks);
    }
}

/// Hardware cursor: an overlay input first (steady block, #96 — text
/// inputs read better than a bar), else the focused pane's own cursor in
/// the pane's shape. Hidden when neither applies.
pub(crate) fn place_hardware_cursor(
    emitter: &mut dmux_compositor::Emitter,
    view_cursor: Option<(u16, u16)>,
    no_overlays: bool,
    focused: Option<&LogicalPane>,
) {
    if let Some((cx, cy)) = view_cursor {
        emitter.move_to(cx, cy);
        emitter.cursor_shape(2);
        emitter.show_cursor();
    } else if no_overlays {
        if let Some(p) = focused {
            if let (Some(rect), cur) = (p.rect, p.term.cursor()) {
                if let Some((cx, cy)) = cur.position {
                    if cx < rect.w && cy < rect.h {
                        emitter.move_to(rect.x + cx, rect.y + cy);
                        emitter.cursor_shape(cur.shape);
                        emitter.show_cursor();
                    }
                }
            }
        }
    }
}

/// Content area to the right of the sidebar (the welcome screen's canvas).
pub fn content_area(buf: &CellBuffer, layout: &Layout) -> Rect {
    let x = layout.sidebar.right() + 1;
    Rect::new(x, 0, buf.cols().saturating_sub(x), buf.rows())
}

pub(super) fn status_glyph(pane: &LogicalPane, anim: u64) -> String {
    if pane.needs_attention && !pane.closing {
        return "●".to_string();
    }
    match pane.status {
        PaneStatus::Working => spinner_frame(anim).to_string(),
        PaneStatus::Waiting => "△".to_string(),
        PaneStatus::Idle => "◌".to_string(),
        PaneStatus::Dead => "✗".to_string(),
    }
}

pub(super) fn attention_color(theme: &Theme, anim: u64) -> Color {
    if anim % 6 < 3 {
        theme.warn
    } else {
        theme.warn_soft
    }
}

pub(crate) fn pane_status_animating(pane: &LogicalPane) -> bool {
    pane.needs_attention || (!pane.hidden && (pane.status == PaneStatus::Working || pane.closing))
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
    let dots_w = dots.len() as u16 * 3;
    let dots_x = bar.right().saturating_sub(dots_w).max(bar.x);
    let title_width = dots_x.saturating_sub(bar.x).saturating_sub(4);
    let glyph = status_glyph(pane, scene.anim);
    let label = format!(
        " {glyph} {} ",
        truncate(pane.display_title(), title_width as usize)
    );
    let attrs = if full || selected {
        AttrFlags::BOLD
    } else {
        AttrFlags::empty()
    };
    buf.draw_text(bar.x, bar.y, &label, fg, bg, attrs, bar);
    if pane.needs_attention && !pane.closing && bar.w > 1 {
        buf.set(
            bar.x + 1,
            bar.y,
            Cell {
                ch: '●',
                fg: attention_color(t, scene.anim),
                bg,
                attrs: AttrFlags::BOLD,
                ..Cell::default()
            },
        );
    }
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

    // Right side: macOS-style traffic lights on the bar itself. Green
    // renames, yellow hides, and red closes. Each dot is CENTERED in a
    // 3-cell slot (#98 round 2): ` ● ` per slot, so the glyph sits in the
    // middle of its own click target instead of reading right-aligned.
    let mut x = dots_x;
    for (target, vivid, dim) in dots {
        let hovered = scene.hovered == Some(target);
        let fg = if focused || hovered { vivid } else { dim };
        let bx = x;
        let hit = Rect::new(bx, bar.y, 3, 1);
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
        buf.draw_text(
            bx + 1,
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
        x = bx + 3;
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

/// Clamp a HUD origin so the whole card stays inside `area` (#103): the
/// overlay must remain recoverable at every viewport size.
pub(crate) fn hud_clamp(pos: (u16, u16), size: (u16, u16), area: Rect) -> (u16, u16) {
    (
        pos.0.min(area.w.saturating_sub(size.0)),
        pos.1.min(area.h.saturating_sub(size.1)),
    )
}

/// The HUD's on-screen rect: the dragged position when one is stored
/// (clamped), else the default top-right anchor. Shared by the renderer
/// and the drag logic so grab offsets can never drift from the drawing.
pub(crate) fn hud_layout(area: Rect, metrics: &Metrics, pos: Option<(u16, u16)>) -> Rect {
    let lines = metrics.hud_lines();
    let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16 + 2;
    let h = lines.len() as u16 + 1;
    let default = (
        area.w.saturating_sub(w + 1),
        1.min(area.h.saturating_sub(h)),
    );
    let (x, y) = hud_clamp(pos.unwrap_or(default), (w, h), area);
    Rect::new(x, y, w, h).intersect(&area)
}

fn draw_hud(
    buf: &mut CellBuffer,
    metrics: &Metrics,
    pos: Option<(u16, u16)>,
    clicks: &mut ClickMap<ClickTarget>,
) {
    let lines = metrics.hud_lines();
    let area = buf.area();
    let rect = hud_layout(area, metrics, pos);
    // Distinct title-bar surface (#109): the drag handle reads as a bar
    // above the metrics, not one flat slab. Diagnostic blues, no project
    // theming.
    const HUD_BAR_BG: Color = Color::Indexed(24);
    const HUD_BODY_BG: Color = Color::Indexed(17);
    buf.fill(
        rect,
        &Cell {
            bg: HUD_BODY_BG,
            ..Cell::default()
        },
    );
    buf.fill(
        Rect::new(rect.x, rect.y, rect.w, 1),
        &Cell {
            bg: HUD_BAR_BG,
            ..Cell::default()
        },
    );
    buf.draw_text(
        rect.x + 1,
        rect.y,
        "perf",
        Color::Indexed(159),
        HUD_BAR_BG,
        AttrFlags::BOLD,
        rect,
    );
    // Title row is the drag handle; the ✕ dismisses (#103).
    buf.draw_text(
        rect.right().saturating_sub(2),
        rect.y,
        "✕",
        Color::Indexed(210),
        HUD_BAR_BG,
        AttrFlags::BOLD,
        rect,
    );
    let close = Rect::new(rect.right().saturating_sub(2), rect.y, 2, 1);
    clicks.add(
        Rect::new(rect.x, rect.y, rect.w.saturating_sub(2), 1),
        ClickTarget::HudTitle,
    );
    clicks.add(close, ClickTarget::HudClose);
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
mod attention_tests;
#[cfg(test)]
mod canvas_tests;
#[cfg(test)]
mod hud_tests;
#[cfg(test)]
mod sidebar_preview;
#[cfg(test)]
mod title_tests;
