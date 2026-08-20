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
fn session_title_uses_pane_bar_surface_without_project_braille() {
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
        session_name: "dmux-session-name-that-truncates",
        project_name: "project",
        hud: None,
        status_line: "",
        theme: &theme,
        anim: 0,
        leader_armed: false,
        sidebar_focused: false,
        version: "v0.0.0",
        issues: (0, 0),
        groups: &groups,
        pane_accents: &[],
        reorder: None,
        sidebar_project: None,
        hovered: None,
    };
    let mut buf = CellBuffer::new(24, 12);
    let mut clicks = ClickMap::new();

    draw_sidebar(&mut buf, &scene, &mut clicks);

    let title: String = (0..24).map(|col| buf.get(col, 0).ch).collect();
    assert!(title.starts_with("  dmux-session"));
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
