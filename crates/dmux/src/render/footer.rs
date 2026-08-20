//! Structured sidebar footer: global actions and command status. Build
//! identity lives in the sidebar title (#101).

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
    if area.h >= 9 && inner_width >= COMPACT_ACTION_WIDTH {
        draw_actions(buf, scene, clicks, area, background, inner_width);
    }
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
    let divider_row = area.bottom().saturating_sub(4);
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

    let row = area.bottom().saturating_sub(3);
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
            project_name: "p",
            profiler: None,
            profiler_pos: None,
            status_line: status,
            theme,
            anim: 0,
            leader_armed: false,
            sidebar_focused: false,
            version: "dmux-rs v0.18.2",
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

        // #101: the build-state row is gone — divider, actions, one blank
        // spacer, then the status row; no version or issue aggregate here.
        assert!(row_text(&buf, 40, 26).starts_with("────"));
        assert_eq!(
            row_text(&buf, 40, 27).trim_end(),
            " + project │ ⚙ settings │ ? shortcuts"
        );
        assert!(row_text(&buf, 40, 28).trim().is_empty());
        assert!(row_text(&buf, 40, 29).starts_with(" ^b for commands"));
        assert_eq!(buf.get(0, 29).bg, theme.bg_raised);
        assert_eq!(buf.get(39, 29).bg, theme.bg_raised);
        for row in 25..30 {
            let text = row_text(&buf, 40, row);
            assert!(!text.contains("v0.18.2"), "row {row}: {text:?}");
            assert!(!text.contains("issues"), "row {row}: {text:?}");
        }
        assert_eq!(clicks.hit(2, 27), Some(&ClickTarget::SidebarNewProject));
        assert_eq!(clicks.hit(15, 27), Some(&ClickTarget::SidebarSettings));
        assert_eq!(clicks.hit(31, 27), Some(&ClickTarget::SidebarHelp));
        assert_eq!(clicks.hit(12, 27), None);
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
        let actions = row_text(&buf, 35, 17);
        assert!(actions.contains("? help"));
        assert!(!actions.contains("short"));
        assert_eq!(clicks.hit(31, 17), Some(&ClickTarget::SidebarHelp));
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
