//! Pure style resolution for the composed frame: which colors, labels, and
//! ownership rules each sidebar and pane element gets. Everything here is a
//! pure function so the visual contracts stay unit-testable without a
//! buffer (#13, #15, #21, #23, #28, #29, #38, #39).

use dmux_compositor::{Color, Rect};
use dmux_ui::Theme;

use crate::layout::TITLE_ROWS;
use crate::render::SidebarGroup;

/// Whether a pane header renders with the full active treatment: actual
/// focus always does; while the sidebar owns the keyboard, the selected
/// pane previews it too (#21) — Enter then makes the preview real. With the
/// sidebar unfocused, selection falls back to the milder #13 state.
pub(crate) fn header_shows_active(focused: bool, selected: bool, sidebar_focused: bool) -> bool {
    focused || (selected && sidebar_focused)
}

/// Title-bar colors: focus (activation) and sidebar selection are distinct
/// states (#13). Focused wins with the solid soft-accent band; a pane
/// selected in the sidebar gets the sidebar's neutral selection surface
/// with accent text; everything else sits on the raised surface.
pub(crate) fn title_bar_style(
    theme: &Theme,
    (accent, accent_soft): (Color, Color),
    focused: bool,
    selected: bool,
) -> (Color, Color) {
    if focused {
        (Color::Indexed(255), accent_soft)
    } else if selected {
        (accent, theme.bg_selected)
    } else {
        (accent, theme.bg_raised)
    }
}

/// Rows of the sidebar border column that double as the focused pane's
/// left edge (#39): Some(top, bottom) — title row included — when the
/// focused pane sits directly right of the sidebar border column.
pub(crate) fn sidebar_edge_highlight(
    border_x: u16,
    focused_rect: Option<Rect>,
) -> Option<(u16, u16)> {
    let fr = focused_rect?;
    if fr.x != border_x + 1 {
        return None;
    }
    Some((fr.y.saturating_sub(TITLE_ROWS), fr.bottom()))
}

/// Does the focused pane touch the right-edge border drawn by the pane at
/// `rect` (#38)? True when the focused pane sits directly right of that
/// border column and their vertical extents (title row included) overlap —
/// the focused pane then owns the segment's color.
pub(crate) fn focused_claims_edge(
    rect: Rect,
    is_focused: bool,
    focused_rect: Option<Rect>,
) -> bool {
    if is_focused {
        return true;
    }
    let Some(fr) = focused_rect else { return false };
    let border_x = rect.right();
    if fr.x != border_x + 1 {
        return false;
    }
    let a_top = rect.y.saturating_sub(TITLE_ROWS);
    let f_top = fr.y.saturating_sub(TITLE_ROWS);
    a_top < fr.bottom() && f_top < rect.bottom()
}

/// Sidebar row annotation: an in-flight close (#29) outranks hidden.
pub(crate) fn row_tag(closing: bool, hidden: bool) -> &'static str {
    if closing {
        " (closing…)"
    } else if hidden {
        " (hidden)"
    } else {
        ""
    }
}

/// Right-side braille separator runs beside project names: the project's
/// LIGHT accent, matching the name they trail (#28) — the soft variant is a
/// dark shade that vanished against dark terminal backgrounds.
pub(crate) fn group_fill_color(group: &SidebarGroup) -> Color {
    group.accent
}

/// Agent-kind labels ([cc], [cx], …) on sidebar rows: the theme's light
/// dim-text foreground (#28), never a dark accent variant.
pub(crate) fn agent_tag_color(theme: &Theme) -> Color {
    theme.text_dim
}

/// The sidebar's base surface is ALWAYS the terminal's own background —
/// transparent, no tint in any focus state (#23, superseding #15's surface
/// lift; #6). Focus is signaled by accent cues instead: the bracketed
/// action labels and the selection bar.
pub(crate) fn sidebar_surface(theme: &Theme, _focused: bool) -> Color {
    theme.bg
}

/// Project action labels: bracketed hotkeys only while the sidebar has the
/// keyboard (#15) — hotkeys aren't live otherwise.
pub(crate) fn action_labels(group_active: bool, sidebar_focused: bool) -> (String, String) {
    if group_active && sidebar_focused {
        ("[n]ew agent".to_string(), "[t]erminal".to_string())
    } else {
        ("new agent".to_string(), "terminal".to_string())
    }
}

pub(crate) fn issue_action_label(label: &str, group_active: bool, sidebar_focused: bool) -> String {
    if !group_active || !sidebar_focused || label.is_empty() {
        return label.to_owned();
    }
    if label == "loading…" {
        return "[i]ssues loading…".to_owned();
    }
    match label.find("issue") {
        Some(index) => format!("{}[i]{}", &label[..index], &label[index + 1..]),
        None => label.to_owned(),
    }
}

/// Footer tip rotation: one step per 15 seconds of wall clock (`now_ms` is
/// milliseconds — #5 shipped a /15 that rotated every 15ms).
pub(crate) fn tip_index(now_ms: u64, len: usize) -> usize {
    (now_ms / 15_000) as usize % len.max(1)
}

/// Width-aware tip pick: rotate (15s steps) through only the tips that fit
/// the footer, so narrow sidebars show complete messages instead of clipped
/// fragments (#8). Falls back to the shortest tip when nothing fits.
pub(crate) fn pick_tip<'a>(tips: &[&'a str], now_ms: u64, width: usize) -> &'a str {
    let fitting: Vec<&'a str> = tips
        .iter()
        .copied()
        .filter(|t| t.chars().count() <= width)
        .collect();
    if fitting.is_empty() {
        return tips
            .iter()
            .copied()
            .min_by_key(|t| t.chars().count())
            .unwrap_or("");
    }
    fitting[tip_index(now_ms, fitting.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tips_are_picked_to_fit_the_sidebar() {
        let tips = &[
            "short tip",
            "a much longer tip that only fits wide sidebars",
        ];
        // Narrow sidebar: only the fitting tip is ever shown, at any time.
        assert_eq!(pick_tip(tips, 0, 20), "short tip");
        assert_eq!(pick_tip(tips, 16_000, 20), "short tip");
        // Wide sidebar: rotation covers both.
        assert_eq!(pick_tip(tips, 0, 80), tips[0]);
        assert_eq!(pick_tip(tips, 15_000, 80), tips[1]);
        // Nothing fits: shortest tip, never an empty footer.
        assert_eq!(pick_tip(tips, 0, 3), "short tip");
    }

    #[test]
    fn footer_tips_hold_for_fifteen_seconds() {
        // Stable within a 15s window…
        assert_eq!(tip_index(0, 7), tip_index(14_999, 7));
        // …advances by exactly one across the boundary…
        assert_eq!(tip_index(15_000, 7), 1);
        // …and wraps around the tip list.
        assert_eq!(tip_index(7 * 15_000, 7), 0);
    }

    #[test]
    fn sidebar_edge_highlights_alongside_adjacent_focused_pane() {
        // #39: a focused pane starting at the sidebar claims the border rows
        // spanning its title row through its body; a pane in a farther
        // column claims nothing, leaving the neutral border everywhere.
        let border_x = 40;
        let adjacent = Rect::new(41, 1, 78, 19);
        assert_eq!(
            sidebar_edge_highlight(border_x, Some(adjacent)),
            Some((0, 20))
        );
        let far_column = Rect::new(120, 1, 78, 19);
        assert_eq!(sidebar_edge_highlight(border_x, Some(far_column)), None);
        assert_eq!(sidebar_edge_highlight(border_x, None), None);
    }

    #[test]
    fn focused_pane_owns_every_touching_border() {
        // Two side-by-side panes: left pane's right edge is the focused
        // right pane's LEFT edge — the focused pane claims it (#38).
        let left = Rect::new(41, 1, 48, 38);
        let right = Rect::new(90, 1, 48, 38);
        // Focused pane itself always claims its own edge.
        assert!(focused_claims_edge(left, true, Some(left)));
        // Right neighbor focused: it claims the shared border column.
        assert!(focused_claims_edge(left, false, Some(right)));
        // Not adjacent (gap of more than the gutter): no claim.
        let far = Rect::new(95, 1, 40, 38);
        assert!(!focused_claims_edge(left, false, Some(far)));
        // Adjacent horizontally but no vertical overlap: no claim.
        let below = Rect::new(90, 60, 48, 20);
        assert!(!focused_claims_edge(left, false, Some(below)));
        // Stacked layouts: a pane BELOW does not touch the right border of
        // one above it, so unrelated borders keep their own colors.
        let stacked_top = Rect::new(41, 1, 98, 18);
        let stacked_bottom = Rect::new(41, 21, 98, 18);
        assert!(!focused_claims_edge(
            stacked_top,
            false,
            Some(stacked_bottom)
        ));
    }

    #[test]
    fn closing_state_outranks_hidden_in_row_tags() {
        // #29: a confirmed close shows immediately and wins over (hidden).
        assert_eq!(row_tag(true, false), " (closing…)");
        assert_eq!(row_tag(true, true), " (closing…)");
        assert_eq!(row_tag(false, true), " (hidden)");
        assert_eq!(row_tag(false, false), "");
    }

    #[test]
    fn right_side_metadata_uses_light_foregrounds() {
        // #28: braille separators match the project's light accent; agent
        // labels use the theme's light dim text — never the dark soft
        // accent variants.
        let theme = Theme::named("violet");
        let group = SidebarGroup {
            name: "app".into(),
            root: "/app".into(),
            accent: Color::Indexed(214),
            accent_soft: Color::Indexed(130),
            pane_indices: vec![],
            issue_label: "0 issues".into(),
            active: true,
        };
        assert_eq!(group_fill_color(&group), group.accent);
        assert_ne!(group_fill_color(&group), group.accent_soft);
        assert_eq!(agent_tag_color(&theme), theme.text_dim);
        assert_ne!(agent_tag_color(&theme), theme.accent_soft);
    }

    #[test]
    fn sidebar_focus_states_render_distinctly() {
        // #23: NO tint in either state — the terminal background shows
        // through; focus is carried by the action labels (below) and the
        // selection bar, not a surface color.
        let theme = Theme::named("violet");
        assert_eq!(sidebar_surface(&theme, true), Color::Default);
        assert_eq!(sidebar_surface(&theme, false), Color::Default);
        assert_eq!(
            theme.canvas,
            Color::Default,
            "content area is transparent too"
        );
        assert_eq!(
            action_labels(true, true),
            ("[n]ew agent".to_string(), "[t]erminal".to_string())
        );
        // Unfocused (or inactive group): plain labels — hotkeys aren't live.
        assert_eq!(
            action_labels(true, false),
            ("new agent".to_string(), "terminal".to_string())
        );
        assert_eq!(
            action_labels(false, true),
            ("new agent".to_string(), "terminal".to_string())
        );
        assert_eq!(issue_action_label("2 issues", true, true), "2 [i]ssues");
        assert_eq!(issue_action_label("0 issues", true, false), "0 issues");
        assert_eq!(issue_action_label("", true, true), "");
    }

    #[test]
    fn sidebar_selection_previews_the_active_header() {
        // #21: full treatment follows focus — or selection while the
        // sidebar owns the keyboard; never a stale preview afterwards.
        assert!(
            header_shows_active(true, false, false),
            "focused is always active"
        );
        assert!(
            header_shows_active(false, true, true),
            "sidebar navigation previews"
        );
        assert!(
            !header_shows_active(false, true, false),
            "no preview once sidebar unfocused"
        );
        assert!(
            !header_shows_active(false, false, true),
            "unselected panes stay plain"
        );
    }

    #[test]
    fn selection_and_focus_are_distinct_states() {
        // #13: sidebar selection must be visible on the body pane without
        // stealing activation's treatment.
        let theme = Theme::named("violet");
        let accents = (theme.accent, theme.accent_soft);
        let focused = title_bar_style(&theme, accents, true, false);
        let selected = title_bar_style(&theme, accents, false, true);
        let plain = title_bar_style(&theme, accents, false, false);
        assert_ne!(focused, selected, "selection must not look like focus");
        assert_ne!(selected, plain, "selection must be visible");
        assert_ne!(focused, plain);
        // Focus wins when a pane is both focused and selected.
        assert_eq!(title_bar_style(&theme, accents, true, true), focused);
        // Selection uses the sidebar's selection surface for pairing.
        assert_eq!(selected.1, theme.bg_selected);
    }
}
