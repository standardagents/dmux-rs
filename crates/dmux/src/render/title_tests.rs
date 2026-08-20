use super::*;

fn group(name: &str, theme: &Theme) -> SidebarGroup {
    SidebarGroup {
        name: name.into(),
        root: format!("/{name}"),
        accent: theme.accent,
        accent_soft: theme.accent_soft,
        pane_indices: vec![],
        issue_label: String::new(),
        active: false,
    }
}

#[test]
fn build_identity_title_uses_pane_bar_surface_without_project_braille() {
    let theme = Theme::named("violet");
    let layout = Layout {
        sidebar: Rect::new(0, 0, 24, 12),
        ..Default::default()
    };
    let groups = [group("project", &theme), group("other", &theme)];
    let scene = Scene {
        panes: &[],
        layout: &layout,
        focused: 0,
        selected: 0,
        project_name: "project",
        hud: None,
        hud_pos: None,
        status_line: "",
        theme: &theme,
        anim: 0,
        leader_armed: false,
        sidebar_focused: false,
        version: "dmux-rs v0.22.27 (cdd6fbb-long-tail)",
        groups: &groups,
        pane_accents: &[],
        reorder: None,
        sidebar_project: None,
        hovered: None,
    };
    let mut buf = CellBuffer::new(24, 12);
    let mut clicks = ClickMap::new();

    draw_sidebar(&mut buf, &scene, &mut clicks);

    // #101: the title carries the build identity, not the tmux session name.
    let title: String = (0..24).map(|col| buf.get(col, 0).ch).collect();
    assert!(title.starts_with("  dmux-rs v0.22.27"));
    assert!(!title.contains("dmux-session"));
    assert!(title.ends_with('…'));
    assert!(!title
        .chars()
        .any(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch)));
    let (title_fg, title_bg) =
        title_bar_style(&theme, (theme.accent, theme.accent_soft), false, false);
    assert_eq!(buf.get(2, 0).fg, title_fg);
    assert!(!buf.get(2, 0).attrs.contains(AttrFlags::BOLD));
    for col in 0..24 {
        assert_eq!(buf.get(col, 0).bg, title_bg);
    }

    let project_row: String = (0..24).map(|col| buf.get(col, 2).ch).collect();
    assert!(project_row.starts_with("⣿ project "));
    assert_eq!(buf.get(0, 2).fg, groups[0].accent);
    assert_ne!(buf.get(0, 2).bg, title_bg);
}

#[test]
fn toolbar_uses_dimension_space_and_centers_dots() {
    // #98 round 2: each ● is centered in a THREE-cell click target — one
    // blank cell either side — so the glyph sits in the middle of its own
    // slot; the whole slot stays clickable for its action. #99 removes dimensions
    // and makes their former columns available to long pane titles.
    use crate::registry::adopt_panes;
    use crate::session::TmuxPaneInfo;
    use dmux_cc::{PaneId, WindowId};

    let info = TmuxPaneInfo {
        pane: PaneId(1),
        window: WindowId(1),
        title: "pane-title-abcdefghijklmnop".into(),
        width: 40,
        height: 10,
        alternate_on: false,
        current_command: "zsh".into(),
        window_name: "w".into(),
        pane_pid: 1,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: String::new(),
    };
    let mut pane = adopt_panes(None, &[info]).remove(0);
    pane.rect = Some(Rect::new(41, 1, 40, 8));
    let panes = [pane];

    let theme = Theme::named("violet");
    let layout = Layout {
        sidebar: Rect::new(0, 0, 40, 10),
        ..Default::default()
    };
    let scene = Scene {
        panes: &panes,
        layout: &layout,
        focused: 0,
        selected: 0,
        project_name: "p",
        hud: None,
        hud_pos: None,
        status_line: "",
        theme: &theme,
        anim: 0,
        leader_armed: false,
        sidebar_focused: false,
        sidebar_project: None,
        version: "v0.0.0",
        groups: &[],
        pane_accents: &[(theme.accent, theme.accent_soft)],
        reorder: None,
        hovered: None,
    };
    let mut buf = CellBuffer::new(90, 10);
    let mut clicks = ClickMap::new();
    compose(&mut buf, &scene, &mut clicks);

    let bar_text: String = (41..81).map(|x| buf.get(x, 0).ch).collect();
    assert!(bar_text.contains("pane-title-abcdefghijklmnop"));
    assert!(!bar_text.contains('×'));
    assert!(!bar_text.contains("40×10"));

    // Three dots on the title row (y = 0, the bar above the body at y 1).
    let bar_y = 0;
    let dot_cols: Vec<u16> = (0..90).filter(|x| buf.get(*x, bar_y).ch == '●').collect();
    assert_eq!(dot_cols.len(), 3, "three window dots: {dot_cols:?}");
    // #98 round 2: 3-column pitch, each dot centered in its slot with a
    // blank on both sides: ` ●  ●  ● `.
    assert_eq!(dot_cols[1] - dot_cols[0], 3);
    assert_eq!(dot_cols[2] - dot_cols[1], 3);
    assert_eq!(dot_cols[0], panes[0].rect.unwrap().x + 1);
    for dot_x in &dot_cols {
        assert_eq!(buf.get(dot_x - 1, bar_y).ch, ' ', "left gap of {dot_x}");
        assert_eq!(buf.get(dot_x + 1, bar_y).ch, ' ', "right gap of {dot_x}");
    }
    // Each dot's click target covers all THREE cells of its slot.
    use crate::views::ClickTarget;
    for (dot_x, want) in dot_cols.iter().zip([
        ClickTarget::TitleClose(0),
        ClickTarget::TitleHide(0),
        ClickTarget::TitleRename(0),
    ]) {
        assert_eq!(clicks.hit(*dot_x, bar_y), Some(&want), "glyph cell");
        assert_eq!(clicks.hit(dot_x - 1, bar_y), Some(&want), "left cell");
        assert_eq!(clicks.hit(dot_x + 1, bar_y), Some(&want), "right cell");
    }
}

#[test]
fn narrow_title_bars_show_only_complete_left_control_slots() {
    use crate::registry::adopt_panes;
    use crate::session::TmuxPaneInfo;
    use dmux_cc::{PaneId, WindowId};

    let info = TmuxPaneInfo {
        pane: PaneId(1),
        window: WindowId(1),
        title: "narrow".into(),
        width: 5,
        height: 3,
        alternate_on: false,
        current_command: "zsh".into(),
        window_name: "w".into(),
        pane_pid: 1,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: String::new(),
    };
    let mut pane = adopt_panes(None, &[info]).remove(0);
    pane.rect = Some(Rect::new(10, 1, 5, 3));
    let panes = [pane];
    let theme = Theme::named("violet");
    let layout = Layout {
        sidebar: Rect::new(0, 0, 9, 5),
        ..Default::default()
    };
    let scene = Scene {
        panes: &panes,
        layout: &layout,
        focused: 0,
        selected: 0,
        project_name: "p",
        hud: None,
        hud_pos: None,
        status_line: "",
        theme: &theme,
        anim: 0,
        leader_armed: false,
        sidebar_focused: false,
        sidebar_project: None,
        version: "v0.0.0",
        groups: &[],
        pane_accents: &[(theme.accent, theme.accent_soft)],
        reorder: None,
        hovered: None,
    };
    let mut buf = CellBuffer::new(20, 5);
    let mut clicks = ClickMap::new();
    compose(&mut buf, &scene, &mut clicks);

    assert_eq!(buf.get(11, 0).ch, '●');
    assert_eq!(clicks.hit(10, 0), Some(&ClickTarget::TitleClose(0)));
    assert_eq!(clicks.hit(12, 0), Some(&ClickTarget::TitleClose(0)));
    assert_ne!(clicks.hit(13, 0), Some(&ClickTarget::TitleHide(0)));
}
