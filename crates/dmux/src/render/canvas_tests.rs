//! Unused-workspace texture (#90): the dot grid fills only the canvas —
//! never pane bodies, title bars, borders, or the sidebar — and follows
//! layout/resize because the fill is recomputed every compose.

use super::*;
use crate::registry::adopt_panes;
use crate::session::TmuxPaneInfo;
use dmux_cc::{PaneId, WindowId};
use dmux_ui::ClickMap;

fn pane_with_rect(rect: Rect) -> LogicalPane {
    let info = TmuxPaneInfo {
        pane: PaneId(1),
        window: WindowId(1),
        title: "term-1".into(),
        width: rect.w,
        height: rect.h,
        alternate_on: false,
        current_command: "zsh".into(),
        window_name: "w".into(),
        pane_pid: 1,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: String::new(),
    };
    let mut pane = adopt_panes(None, &[info]).remove(0);
    pane.rect = Some(rect);
    pane
}

fn compose_scene(cols: u16, rows: u16, panes: &[LogicalPane]) -> (CellBuffer, Theme) {
    let theme = Theme::named("violet");
    let layout = Layout {
        sidebar: Rect::new(0, 0, 40, rows),
        ..Default::default()
    };
    let scene = Scene {
        panes,
        layout: &layout,
        focused: 0,
        selected: 0,
        project_name: "dmux-rs",
        profiler: None,
        profiler_pos: None,
        status_line: "",
        theme: &theme,
        anim: 0,
        leader_armed: false,
        sidebar_focused: false,
        sidebar_project: None,
        version: "test",
        groups: &[],
        pane_accents: &[(theme.accent, theme.accent_soft)],
        reorder: None,
        hovered: None,
    };
    let mut buf = CellBuffer::new(cols, rows);
    let mut clicks = ClickMap::new();
    compose(&mut buf, &scene, &mut clicks);
    (buf, theme)
}

#[test]
fn dot_grid_fills_only_the_unused_workspace() {
    // Pane body at x 41..=80 (title row above, border column at 81):
    // everything right of the border is unused workspace.
    let body = Rect::new(41, 1, 40, 22);
    let panes = [pane_with_rect(body)];
    let (buf, theme) = compose_scene(120, 24, &panes);

    // Unused workspace right of the final pane: textured.
    let dot = buf.get(90, 10);
    assert_eq!(dot.ch, '·');
    assert_eq!(dot.fg, theme.canvas_dot);
    assert_eq!(dot.bg, theme.canvas);
    // Pane body, title bar, and border column: never dots.
    assert_ne!(buf.get(60, 10).ch, '·', "pane body must repaint the fill");
    assert_ne!(buf.get(60, 0).ch, '·', "title bar must repaint the fill");
    assert_eq!(buf.get(body.right(), 10).ch, '│', "border owns its column");
    // The sidebar is left of the content area and untouched.
    for x in 0..40 {
        assert_ne!(buf.get(x, 5).ch, '·', "sidebar column {x}");
    }
}

#[test]
fn dot_grid_follows_resize_and_disappears_without_panes() {
    let body = Rect::new(41, 1, 40, 22);
    // Wider terminal: the newly exposed workspace is textured too, because
    // the fill derives from the current buffer size every compose.
    let panes = [pane_with_rect(body)];
    let (wide, theme) = compose_scene(160, 24, &panes);
    assert_eq!(wide.get(150, 12).ch, '·');
    assert_eq!(wide.get(150, 12).fg, theme.canvas_dot);

    // No open panes (welcome screen): the canvas stays untextured.
    let (empty, _) = compose_scene(120, 24, &[]);
    for x in 41..120 {
        assert_ne!(empty.get(x, 10).ch, '·', "welcome canvas column {x}");
    }
}
