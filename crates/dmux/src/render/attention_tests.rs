use super::*;
use crate::registry::adopt_panes;
use crate::session::TmuxPaneInfo;
use dmux_cc::{PaneId, WindowId};

fn attention_pane() -> LogicalPane {
    let info = TmuxPaneInfo {
        pane: PaneId(1),
        window: WindowId(1),
        title: "attention-pane".into(),
        width: 40,
        height: 8,
        alternate_on: false,
        current_command: "zsh".into(),
        window_name: "attention-pane".into(),
        pane_pid: 1,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: String::new(),
    };
    let mut pane = adopt_panes(None, &[info]).remove(0);
    pane.rect = Some(Rect::new(31, 1, 40, 8));
    pane.status = PaneStatus::Idle;
    pane.needs_attention = true;
    pane
}

fn render_phase(anim: u64) -> (CellBuffer, Theme) {
    let panes = [attention_pane()];
    let theme = Theme::named("violet");
    let layout = Layout {
        sidebar: Rect::new(0, 0, 30, 10),
        ..Default::default()
    };
    let groups = [SidebarGroup {
        name: "project".into(),
        root: "/project".into(),
        accent: theme.accent,
        accent_soft: theme.accent_soft,
        pane_indices: vec![0],
        issue_label: String::new(),
        active: true,
    }];
    let scene = Scene {
        panes: &panes,
        layout: &layout,
        focused: 0,
        selected: 0,
        project_name: "project",
        hud: None,
        hud_pos: None,
        status_line: "",
        theme: &theme,
        anim,
        leader_armed: false,
        sidebar_focused: false,
        sidebar_project: None,
        version: "v0.0.0",
        groups: &groups,
        pane_accents: &[(theme.accent, theme.accent_soft)],
        reorder: None,
        hovered: None,
    };
    let mut buf = CellBuffer::new(80, 10);
    let mut clicks = ClickMap::new();
    compose(&mut buf, &scene, &mut clicks);
    (buf, theme)
}

#[test]
fn attention_uses_filled_orange_status_circles_in_both_blink_phases() {
    let (bright, theme) = render_phase(0);
    let (dim, _) = render_phase(3);

    for (x, y) in [(2, 2), (41, 0)] {
        assert_eq!(bright.get(x, y).ch, '●');
        assert_eq!(bright.get(x, y).fg, theme.warn);
        assert!(bright.get(x, y).attrs.contains(AttrFlags::BOLD));
        assert_eq!(dim.get(x, y).ch, '●');
        assert_eq!(dim.get(x, y).fg, theme.warn_soft);
    }

    let sidebar_row: String = (0..30).map(|x| bright.get(x, 2).ch).collect();
    assert!(sidebar_row.starts_with("▸ ● attention-pane"));
    assert!(!sidebar_row.contains('!'));
}

#[test]
fn attention_keeps_the_animation_clock_running_for_hidden_sidebar_rows() {
    let mut pane = attention_pane();
    pane.hidden = true;
    assert!(pane_status_animating(&pane));

    pane.needs_attention = false;
    assert!(!pane_status_animating(&pane));
}
