//! Perf-HUD pointer contract (#103): hit targets for drag/close, dragged
//! placement honored, and clamping keeps the card recoverable everywhere.

use super::*;
use dmux_ui::ClickMap;

fn hud_scene<'a>(metrics: &'a Metrics, theme: &'a Theme, layout: &'a Layout) -> Scene<'a> {
    Scene {
        panes: &[],
        layout,
        focused: 0,
        selected: 0,
        project_name: "p",
        hud: Some(metrics),
        hud_pos: None,
        status_line: "",
        theme,
        anim: 0,
        leader_armed: false,
        sidebar_focused: false,
        sidebar_project: None,
        version: "v0.0.0",
        groups: &[],
        pane_accents: &[],
        reorder: None,
        hovered: None,
    }
}

#[test]
fn hud_registers_drag_handle_and_close_targets() {
    let theme = Theme::named("violet");
    let layout = Layout {
        sidebar: Rect::new(0, 0, 40, 30),
        ..Default::default()
    };
    let metrics = Metrics::new();
    let scene = hud_scene(&metrics, &theme, &layout);
    let mut buf = CellBuffer::new(120, 30);
    let mut clicks = ClickMap::new();
    compose(&mut buf, &scene, &mut clicks);

    let rect = hud_layout(buf.area(), &metrics, None);
    // Title row: drag handle everywhere except the ✕ cells at the right.
    assert_eq!(
        clicks.hit(rect.x, rect.y),
        Some(&crate::views::ClickTarget::HudTitle)
    );
    assert_eq!(
        clicks.hit(rect.right() - 3, rect.y),
        Some(&crate::views::ClickTarget::HudTitle)
    );
    assert_eq!(
        clicks.hit(rect.right() - 2, rect.y),
        Some(&crate::views::ClickTarget::HudClose)
    );
    // The ✕ glyph is visible on the title row.
    let title: String = (rect.x..rect.right())
        .map(|x| buf.get(x, rect.y).ch)
        .collect();
    assert!(title.contains("perf"), "{title:?}");
    assert!(title.contains('✕'), "{title:?}");
}

#[test]
fn dragged_position_is_honored_and_clamped() {
    let metrics = Metrics::new();
    let area = Rect::new(0, 0, 120, 30);
    let default = hud_layout(area, &metrics, None);
    assert_eq!(default.right(), 119, "default anchors to the top-right");
    assert_eq!(default.y, 1);
    // A stored drag position places the card exactly there.
    let moved = hud_layout(area, &metrics, Some((10, 5)));
    assert_eq!((moved.x, moved.y), (10, 5));
    assert_eq!((moved.w, moved.h), (default.w, default.h));
    // Positions past the edges clamp so the card stays fully on screen.
    let clamped = hud_layout(area, &metrics, Some((500, 500)));
    assert_eq!(clamped.right(), area.w);
    assert_eq!(clamped.bottom(), area.h);
    // Tiny viewport: the card is cut to fit, never lost.
    let tiny = Rect::new(0, 0, 10, 4);
    let small = hud_layout(tiny, &metrics, Some((9, 3)));
    assert!(small.right() <= 10 && small.bottom() <= 4);
    assert!(!small.is_empty());
    // Pure clamp: interior positions pass through untouched.
    assert_eq!(hud_clamp((3, 4), (5, 5), area), (3, 4));
    assert_eq!(hud_clamp((119, 29), (5, 5), area), (115, 25));
}
