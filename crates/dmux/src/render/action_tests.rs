use super::*;

#[test]
fn selected_project_action_uses_the_selection_surface() {
    let theme = Theme::named("violet");
    let mut buf = CellBuffer::new(40, 1);
    let area = buf.area();
    let end = draw_sidebar_action(
        &mut buf,
        2,
        0,
        "[n]ew agent",
        true,
        theme.text_dim,
        theme.accent,
        theme.bg,
        &theme,
        area,
    );
    for col in 2..end {
        let cell = buf.get(col, 0);
        assert_eq!(cell.bg, theme.bg_selected);
        assert_eq!(cell.fg, theme.accent);
        assert!(cell.attrs.contains(AttrFlags::BOLD));
    }
    assert!(pane_is_selected(2, 2, false));
    assert!(!pane_is_selected(2, 2, true));
}
