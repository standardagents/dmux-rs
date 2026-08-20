//! Profiler pointer contract (#103): hit targets for drag/close, dragged
//! placement honored, and clamping keeps the card recoverable everywhere.

use super::*;
use dmux_ui::ClickMap;

fn profiler_scene<'a>(metrics: &'a Metrics, theme: &'a Theme, layout: &'a Layout) -> Scene<'a> {
    Scene {
        panes: &[],
        layout,
        focused: 0,
        selected: 0,
        project_name: "p",
        profiler: Some(metrics),
        profiler_pos: None,
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
fn profiler_registers_drag_handle_and_close_targets() {
    let theme = Theme::named("violet");
    let layout = Layout {
        sidebar: Rect::new(0, 0, 40, 30),
        ..Default::default()
    };
    let metrics = Metrics::new();
    let scene = profiler_scene(&metrics, &theme, &layout);
    let mut buf = CellBuffer::new(120, 30);
    let mut clicks = ClickMap::new();
    compose(&mut buf, &scene, &mut clicks);

    let rect = profiler_layout(buf.area(), &metrics, None, layout.sidebar.right());
    // Title row: the close slot is on the left and the remainder drags.
    assert_eq!(
        clicks.hit(rect.x, rect.y),
        Some(&crate::views::ClickTarget::ProfilerClose)
    );
    assert_eq!(
        clicks.hit(rect.x + 2, rect.y),
        Some(&crate::views::ClickTarget::ProfilerClose)
    );
    assert_eq!(
        clicks.hit(rect.x + 3, rect.y),
        Some(&crate::views::ClickTarget::ProfilerTitle)
    );
    assert_eq!(
        clicks.hit(rect.right() - 1, rect.y),
        Some(&crate::views::ClickTarget::ProfilerTitle)
    );
    // The ✕ glyph is visible on the title row.
    let title: String = (rect.x..rect.right())
        .map(|x| buf.get(x, rect.y).ch)
        .collect();
    assert!(title.contains("profiler"), "{title:?}");
    assert!(title.contains('✕'), "{title:?}");
}

#[test]
fn dragged_position_is_honored_and_clamped() {
    let metrics = Metrics::new();
    let area = Rect::new(0, 0, 120, 30);
    let sidebar_right = 40;
    let default = profiler_layout(area, &metrics, None, sidebar_right);
    assert_eq!(default.x, 41, "default clears the sidebar gutter");
    assert_eq!(default.y, 0, "default attaches to the viewport top");
    // A stored drag position places the card exactly there.
    let moved = profiler_layout(area, &metrics, Some((50, 5)), sidebar_right);
    assert_eq!((moved.x, moved.y), (50, 5));
    assert_eq!((moved.w, moved.h), (default.w, default.h));
    // Positions on either side clamp to the workspace beside the sidebar.
    let left = profiler_layout(area, &metrics, Some((0, 5)), sidebar_right);
    assert_eq!(left.x, 41);
    let clamped = profiler_layout(area, &metrics, Some((500, 500)), sidebar_right);
    assert_eq!(clamped.right(), area.w);
    assert_eq!(clamped.bottom(), area.h);
    // A viewport fully occupied by the sidebar has no profiler surface.
    let tiny = Rect::new(0, 0, 10, 4);
    assert!(profiler_layout(tiny, &metrics, None, 10).is_empty());
    // Clamping respects non-zero workspace origins.
    let workspace = Rect::new(41, 0, 79, 30);
    assert_eq!(profiler_clamp((50, 4), (5, 5), workspace), (50, 4));
    assert_eq!(profiler_clamp((0, 29), (5, 5), workspace), (41, 25));
}
