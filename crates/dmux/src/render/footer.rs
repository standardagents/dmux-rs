//! Structured sidebar footer: global actions, build state, and command status.

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::ClickMap;

use crate::views::ClickTarget;

use super::{draw_sidebar_action, truncate, Scene};

const SIDE_INSET: u16 = 1;
const FULL_ACTION_WIDTH: u16 = 36;
const COMPACT_ACTION_WIDTH: u16 = 31;

pub(super) fn draw(
    buf: &mut CellBuffer,
    scene: &Scene<'_>,
    clicks: &mut ClickMap<ClickTarget>,
    area: Rect,
    background: Color,
) {
    if area.is_empty() {
        return;
    }

    let inner_width = area.w.saturating_sub(SIDE_INSET * 2);
    if area.h >= 10 && inner_width >= COMPACT_ACTION_WIDTH {
        draw_actions(buf, scene, clicks, area, background, inner_width);
    }
    draw_build_state(buf, scene, clicks, area, background);
    draw_status(buf, scene, area);
}

fn draw_actions(
    buf: &mut CellBuffer,
    scene: &Scene<'_>,
    clicks: &mut ClickMap<ClickTarget>,
    area: Rect,
    background: Color,
    inner_width: u16,
) {
    let theme = scene.theme;
    let divider_row = area.bottom().saturating_sub(5);
    let divider: String = "─".repeat(area.w as usize);
    buf.draw_text(
        area.x,
        divider_row,
        &divider,
        theme.border,
        background,
        AttrFlags::empty(),
        area,
    );

    let row = area.bottom().saturating_sub(4);
    let help_label = if inner_width >= FULL_ACTION_WIDTH {
        "? shortcuts"
    } else {
        "? help"
    };
    let actions = [
        ("+ project", ClickTarget::SidebarNewProject, theme.accent),
        ("⚙ settings", ClickTarget::SidebarSettings, theme.text_dim),
        (help_label, ClickTarget::SidebarHelp, theme.text_dim),
    ];
    let mut x = area.x + SIDE_INSET;
    for (index, (label, target, color)) in actions.into_iter().enumerate() {
        if index > 0 {
            x = buf.draw_text(
                x + 1,
                row,
                "│",
                theme.border,
                background,
                AttrFlags::empty(),
                area,
            ) + 1;
        }
        let start = x;
        x = draw_sidebar_action(
            buf,
            start,
            row,
            label,
            scene.hovered == Some(target),
            color,
            theme.accent,
            background,
            theme,
            area,
        );
        clicks.add(Rect::new(start, row, x.saturating_sub(start), 1), target);
    }
}

fn draw_build_state(
    buf: &mut CellBuffer,
    scene: &Scene<'_>,
    clicks: &mut ClickMap<ClickTarget>,
    area: Rect,
    background: Color,
) {
    let row = area.bottom().saturating_sub(2);
    let left = area.x + SIDE_INSET;
    let right = area.right().saturating_sub(SIDE_INSET);
    let (total, fresh) = scene.issues;

    if total == 0 {
        let width = right.saturating_sub(left) as usize;
        buf.draw_text(
            left,
            row,
            &truncate(scene.version, width),
            scene.theme.text_faint,
            background,
            AttrFlags::empty(),
            area,
        );
        return;
    }

    let issue_label = if fresh > 0 {
        format!("issues {total} ({fresh} new)")
    } else {
        format!("issues {total}")
    };
    let issue_width = issue_label.chars().count() as u16;
    let issue_x = right.saturating_sub(issue_width).max(left);
    let separator_x = issue_x.saturating_sub(2);
    let version_width = separator_x.saturating_sub(left + 1) as usize;
    if version_width > 0 {
        buf.draw_text(
            left,
            row,
            &truncate(scene.version, version_width),
            scene.theme.text_faint,
            background,
            AttrFlags::empty(),
            area,
        );
        buf.draw_text(
            separator_x,
            row,
            "│",
            scene.theme.border,
            background,
            AttrFlags::empty(),
            area,
        );
    }
    let color = if fresh > 0 {
        scene.theme.warn
    } else {
        scene.theme.text_faint
    };
    let target = ClickTarget::SidebarIssues;
    let end = draw_sidebar_action(
        buf,
        issue_x,
        row,
        &issue_label,
        scene.hovered == Some(target),
        color,
        scene.theme.accent,
        background,
        scene.theme,
        area,
    );
    clicks.add(
        Rect::new(issue_x, row, end.saturating_sub(issue_x), 1),
        target,
    );
}

fn draw_status(buf: &mut CellBuffer, scene: &Scene<'_>, area: Rect) {
    let row = area.bottom().saturating_sub(1);
    buf.fill(
        Rect::new(area.x, row, area.w, 1),
        &Cell {
            bg: scene.theme.bg_raised,
            ..Cell::default()
        },
    );
    let color = if scene.leader_armed {
        scene.theme.warn
    } else {
        scene.theme.text_dim
    };
    let text = if scene.leader_armed {
        crate::input::LEADER_HINT
    } else {
        scene.status_line
    };
    buf.draw_text(
        area.x + SIDE_INSET,
        row,
        &truncate(text, area.w.saturating_sub(SIDE_INSET * 2) as usize),
        color,
        scene.theme.bg_raised,
        AttrFlags::empty(),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;
    use crate::render::SidebarGroup;
    use dmux_ui::Theme;

    fn scene<'a>(layout: &'a Layout, theme: &'a Theme, status: &'a str) -> Scene<'a> {
        Scene {
            panes: &[],
            layout,
            focused: 0,
            selected: 0,
            session_name: "s",
            project_name: "p",
            hud: None,
            status_line: status,
            theme,
            anim: 0,
            leader_armed: false,
            sidebar_focused: false,
            version: "dmux-rs v0.18.2",
            issues: (1, 0),
            groups: &[] as &[SidebarGroup],
            pane_accents: &[],
            reorder: None,
            sidebar_project: None,
            hovered: None,
        }
    }

    fn row_text(buf: &CellBuffer, width: u16, row: u16) -> String {
        (0..width).map(|col| buf.get(col, row).ch).collect()
    }

    #[test]
    fn footer_rows_have_distinct_structure_and_shared_inset() {
        let theme = Theme::named("violet");
        let layout = Layout {
            sidebar: Rect::new(0, 0, 40, 30),
            ..Default::default()
        };
        let mut buf = CellBuffer::new(40, 30);
        let mut clicks = ClickMap::new();
        draw(
            &mut buf,
            &scene(&layout, &theme, "^b for commands · ^b ? help"),
            &mut clicks,
            layout.sidebar,
            theme.bg,
        );

        assert!(row_text(&buf, 40, 25).starts_with("────"));
        assert_eq!(
            row_text(&buf, 40, 26).trim_end(),
            " + project │ ⚙ settings │ ? shortcuts"
        );
        assert!(row_text(&buf, 40, 27).trim().is_empty());
        assert_eq!(
            row_text(&buf, 40, 28).trim_end(),
            " dmux-rs v0.18.2             │ issues 1"
        );
        assert!(row_text(&buf, 40, 29).starts_with(" ^b for commands"));
        assert_eq!(buf.get(0, 29).bg, theme.bg_raised);
        assert_eq!(buf.get(39, 29).bg, theme.bg_raised);
        assert_eq!(clicks.hit(2, 26), Some(&ClickTarget::SidebarNewProject));
        assert_eq!(clicks.hit(15, 26), Some(&ClickTarget::SidebarSettings));
        assert_eq!(clicks.hit(31, 26), Some(&ClickTarget::SidebarHelp));
        assert_eq!(clicks.hit(12, 26), None);
        assert_eq!(clicks.hit(35, 28), Some(&ClickTarget::SidebarIssues));
    }

    #[test]
    fn constrained_footer_uses_complete_compact_labels() {
        let theme = Theme::named("violet");
        let layout = Layout {
            sidebar: Rect::new(0, 0, 35, 20),
            ..Default::default()
        };
        let mut buf = CellBuffer::new(35, 20);
        let mut clicks = ClickMap::new();
        draw(
            &mut buf,
            &scene(&layout, &theme, "complete status"),
            &mut clicks,
            layout.sidebar,
            theme.bg,
        );
        let actions = row_text(&buf, 35, 16);
        assert!(actions.contains("? help"));
        assert!(!actions.contains("short"));
        assert_eq!(clicks.hit(31, 16), Some(&ClickTarget::SidebarHelp));
    }

    #[test]
    fn tiny_footer_omits_actions_as_one_unit() {
        let theme = Theme::named("violet");
        let layout = Layout {
            sidebar: Rect::new(0, 0, 30, 8),
            ..Default::default()
        };
        let mut buf = CellBuffer::new(30, 8);
        let mut clicks = ClickMap::new();
        draw(
            &mut buf,
            &scene(&layout, &theme, "complete status"),
            &mut clicks,
            layout.sidebar,
            theme.bg,
        );
        let all: String = (0..8).map(|row| row_text(&buf, 30, row)).collect();
        assert!(!all.contains("project"));
        assert!(!all.contains("settings"));
        assert!(!all.contains("shortcuts"));
        assert!(row_text(&buf, 30, 7).contains("complete status"));
    }
}
