//! Sidebar composition (#61): project groups, rows, per-project action
//! rows, and their focused helpers — extracted from render.rs, which keeps
//! the top-level frame composition boundary (the footer sibling module
//! draws the utility block).

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::{spinner_frame, ClickMap, Theme};

use super::{status_glyph, truncate, Scene, SidebarGroup};
#[cfg(test)]
use crate::layout::Layout;
use crate::session::PaneStatus;
use crate::sidebar::ProjectAction;
#[cfg(test)]
use crate::sidebar::ProjectSelection;
use crate::style::{
    action_labels, agent_tag_color, group_fill_color, issue_action_label, row_tag,
    sidebar_edge_highlight, sidebar_surface, title_bar_style,
};
use crate::views::ClickTarget;

pub(crate) fn draw_sidebar(
    buf: &mut CellBuffer,
    scene: &Scene<'_>,
    clicks: &mut ClickMap<ClickTarget>,
) {
    let t = scene.theme;
    let area = scene.layout.sidebar;
    if area.is_empty() {
        return;
    }
    // Focused sidebar reads as the active input area: the whole surface
    // lifts to the raised background, not just the selected row (#15).
    let sb_bg = sidebar_surface(t, scene.sidebar_focused);
    buf.fill(
        area,
        &Cell {
            bg: sb_bg,
            ..Cell::default()
        },
    );

    // Use the inactive pane-title surface for application context (#49).
    // Project headers retain the brighter braille separators below it.
    // The title carries the build identity (#101): name, version, and
    // commit — the generated tmux session name is plumbing, not identity.
    let title_area = Rect::new(area.x, 0, area.w, 1);
    let (title_fg, title_bg) = title_bar_style(t, (t.accent, t.accent_soft), false, false);
    buf.fill(
        title_area,
        &Cell {
            bg: title_bg,
            ..Cell::default()
        },
    );
    let header = format!(
        "  {}",
        truncate(scene.version, area.w.saturating_sub(2) as usize)
    );
    buf.draw_text(
        area.x,
        0,
        &header,
        title_fg,
        title_bg,
        AttrFlags::empty(),
        title_area,
    );

    // Project groups (TS parity: main project first, per-group colors,
    // per-group creation actions, blank spacing between groups).
    let multi = scene.groups.len() > 1;
    let mut row = 2u16;
    let bottom_limit = area.bottom().saturating_sub(4);
    for (gi, group) in scene.groups.iter().enumerate() {
        if row >= bottom_limit {
            break;
        }
        let project_active = selected_project_is_active(scene, group);
        if multi {
            let text = format!(
                "⣿ {} ",
                truncate(&group.name, area.w.saturating_sub(8) as usize)
            );
            let end = buf.draw_text(
                area.x,
                row,
                &text,
                group.accent,
                sb_bg,
                AttrFlags::BOLD,
                area,
            );
            let mut fill = String::new();
            for _ in end..area.right() {
                fill.push('⣿');
            }
            buf.draw_text(
                end,
                row,
                &fill,
                group_fill_color(group),
                sb_bg,
                AttrFlags::empty(),
                area,
            );
            row += 1;
        }
        for &i in &group.pane_indices {
            if row >= bottom_limit {
                break;
            }
            let pane = &scene.panes[i];
            let selected = active_target(
                scene.hovered,
                ClickTarget::SidebarRow(i),
                pane_is_selected(i, scene.selected, scene.sidebar_project.is_some()),
            );
            let focused = i == scene.focused;
            let caret = if selected { "▸" } else { " " };
            let glyph = if pane.closing {
                spinner_frame(scene.anim).to_string()
            } else {
                status_glyph(pane, scene.anim)
            };
            let attn = if pane.needs_attention && !pane.closing {
                "!"
            } else {
                " "
            };
            let hidden_tag = row_tag(pane.closing, pane.hidden);
            let name = truncate(
                pane.display_title(),
                area.w.saturating_sub(14 + hidden_tag.len() as u16) as usize,
            );
            let line = format!("{caret}{attn}{glyph} {name}{hidden_tag}");
            // Reorder drag (#26): the row under the pointer is the insertion
            // target (selection surface + marker below); the dragged source
            // row dims so the movement reads.
            let reorder_target = scene.reorder.map(|(_, pr)| pr == row).unwrap_or(false);
            let reorder_src = scene.reorder.map(|(srci, _)| srci == i).unwrap_or(false);
            let (fg, attrs) = if pane.closing || pane.hidden {
                (t.text_faint, AttrFlags::empty())
            } else if focused {
                (group.accent, AttrFlags::BOLD)
            } else if pane.status == PaneStatus::Waiting || pane.needs_attention {
                (t.warn, AttrFlags::empty())
            } else if project_active {
                (t.text, AttrFlags::empty())
            } else {
                (t.text_dim, AttrFlags::empty())
            };
            let fg = if reorder_src && !reorder_target {
                t.text_faint
            } else {
                fg
            };
            let bg = if selected || reorder_target {
                t.bg_selected
            } else {
                sb_bg
            };
            let row_rect = Rect::new(area.x, row, area.w, 1);
            if selected || reorder_target {
                buf.fill(
                    row_rect,
                    &Cell {
                        bg,
                        ..Cell::default()
                    },
                );
            }
            let end = buf.draw_text(area.x, row, &line, fg, bg, attrs, area);
            if selected && scene.sidebar_focused {
                buf.set(
                    area.x,
                    row,
                    Cell {
                        ch: '▍',
                        fg: group.accent,
                        bg,
                        ..Cell::default()
                    },
                );
            }
            // Reorder drag (#26): accent marker on the insertion target row.
            if reorder_target {
                buf.set(
                    area.x,
                    row,
                    Cell {
                        ch: '➤',
                        fg: group.accent,
                        bg,
                        ..Cell::default()
                    },
                );
            }
            if let Some(agent) = &pane.agent {
                let short = crate::agents::agent(agent).map(|d| d.short).unwrap_or("??");
                let tag = format!("[{short}]");
                let tag_x = area.right().saturating_sub(tag.len() as u16 + 1);
                if tag_x > end {
                    buf.draw_text(
                        tag_x,
                        row,
                        &tag,
                        agent_tag_color(t),
                        bg,
                        AttrFlags::empty(),
                        area,
                    );
                }
            }
            clicks.add(row_rect, ClickTarget::SidebarRow(i));
            row += 1;
        }
        // Per-project creation actions, right-aligned like the TS sidebar.
        // Keyboard navigation shows the hotkeys. Pointer navigation keeps
        // the action words intact while hover supplies the active treatment.
        if row < bottom_limit {
            let keyboard_navigation = scene.sidebar_focused && scene.hovered.is_none();
            let (na, term) = action_labels(project_active, keyboard_navigation);
            let total = na.chars().count() as u16 + term.chars().count() as u16 + 2;
            let x0 = area.right().saturating_sub(total + 1);
            let color = if project_active {
                group.accent
            } else {
                t.text_faint
            };
            let keyboard_action = scene
                .sidebar_project
                .filter(|project| project.root == group.root)
                .map(|project| project.action);
            let issue_max = x0.saturating_sub(area.x + 2) as usize;
            let issue_action =
                issue_action_label(&group.issue_label, project_active, keyboard_navigation);
            let issue_label = truncate(&issue_action, issue_max);
            let ix = area.x + 1;
            let issue_target = ClickTarget::SidebarGroupIssues(gi);
            let issue_end = draw_sidebar_action(
                buf,
                ix,
                row,
                &issue_label,
                active_target(
                    scene.hovered,
                    issue_target,
                    keyboard_action == Some(ProjectAction::Issues),
                ),
                color,
                group.accent,
                sb_bg,
                t,
                area,
            );
            clicks.add(
                Rect::new(ix, row, issue_end.saturating_sub(ix), 1),
                issue_target,
            );
            let agent_target = ClickTarget::SidebarGroupNewAgent(gi);
            let ax = draw_sidebar_action(
                buf,
                x0,
                row,
                &na,
                active_target(
                    scene.hovered,
                    agent_target,
                    keyboard_action == Some(ProjectAction::NewAgent),
                ),
                color,
                group.accent,
                sb_bg,
                t,
                area,
            );
            clicks.add(Rect::new(x0, row, ax - x0, 1), agent_target);
            let terminal_target = ClickTarget::SidebarGroupNewTerminal(gi);
            let tx = draw_sidebar_action(
                buf,
                ax + 2,
                row,
                &term,
                active_target(
                    scene.hovered,
                    terminal_target,
                    keyboard_action == Some(ProjectAction::NewTerminal),
                ),
                color,
                group.accent,
                sb_bg,
                t,
                area,
            );
            clicks.add(Rect::new(ax + 2, row, tx - ax - 2, 1), terminal_target);
            row += 1;
        }
        // Breathing room between groups.
        if multi {
            row += 1;
        }
    }

    // Right-hand sidebar border. The rows alongside a focused pane that
    // starts at the sidebar are that pane's LEFT edge — they take its
    // project accent so the active outline is complete (#39); the rest of
    // the column keeps the neutral border.
    let border_x = area.right();
    if border_x < buf.cols() {
        let focused_rect = scene.panes.get(scene.focused).and_then(|p| p.rect);
        let highlight = sidebar_edge_highlight(border_x, focused_rect);
        let focused_accent = scene
            .pane_accents
            .get(scene.focused)
            .map(|(a, _)| *a)
            .unwrap_or(t.accent);
        for y in 0..area.bottom() {
            let fg = match highlight {
                Some((top, bottom)) if y >= top && y < bottom => focused_accent,
                _ => t.border,
            };
            buf.set(
                border_x,
                y,
                Cell {
                    ch: '│',
                    fg,
                    bg: sb_bg,
                    ..Cell::default()
                },
            );
        }
    }

    super::footer::draw(buf, scene, clicks, area, sb_bg);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_sidebar_action(
    buf: &mut CellBuffer,
    x: u16,
    row: u16,
    label: &str,
    selected: bool,
    color: Color,
    accent: Color,
    background: Color,
    theme: &Theme,
    area: Rect,
) -> u16 {
    let (fg, bg, attrs) = if selected {
        (accent, theme.bg_selected, AttrFlags::BOLD)
    } else {
        (color, background, AttrFlags::empty())
    };
    buf.draw_text(x, row, label, fg, bg, attrs, area)
}

pub(crate) fn pane_is_selected(index: usize, selected: usize, project_selected: bool) -> bool {
    !project_selected && index == selected
}

fn selected_project_is_active(scene: &Scene<'_>, group: &SidebarGroup) -> bool {
    if !scene.sidebar_focused {
        return group.active;
    }
    scene.sidebar_project.map_or_else(
        || group.pane_indices.contains(&scene.selected),
        |project| project.root == group.root,
    )
}

pub(crate) fn active_target(
    hovered: Option<ClickTarget>,
    target: ClickTarget,
    selected: bool,
) -> bool {
    match hovered {
        Some(hovered) if hovered.is_hoverable() => hovered == target,
        _ => selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_text(buf: &CellBuffer, w: u16, h: u16) -> String {
        let mut out = String::new();
        for row in 0..h {
            for col in 0..w {
                out.push(buf.get(col, row).ch);
            }
            out.push('\n');
        }
        out
    }

    fn footer_scene<'a>(
        layout: &'a Layout,
        groups: &'a [SidebarGroup],
        theme: &'a Theme,
    ) -> Scene<'a> {
        Scene {
            panes: &[],
            layout,
            focused: 0,
            selected: 0,
            project_name: "p",
            hud: None,
            hud_pos: None,
            status_line: "",
            theme,
            anim: 0,
            leader_armed: false,
            sidebar_focused: false,
            version: "v0.0.0",
            groups,
            pane_accents: &[],
            reorder: None,
            sidebar_project: None,
            hovered: None,
        }
    }

    #[test]
    fn hover_highlights_exact_sidebar_actions() {
        let theme = Theme::named("violet");
        let layout = Layout {
            sidebar: Rect::new(0, 0, 40, 20),
            ..Default::default()
        };
        let groups = [SidebarGroup {
            name: "repo".into(),
            root: "/repo".into(),
            accent: theme.accent,
            accent_soft: theme.accent_soft,
            pane_indices: vec![],
            issue_label: "2 issues".into(),
            active: false,
        }];
        let selection = ProjectSelection {
            root: "/repo".into(),
            action: ProjectAction::NewAgent,
        };
        let mut scene = footer_scene(&layout, &groups, &theme);
        scene.sidebar_focused = true;
        scene.sidebar_project = Some(&selection);
        scene.hovered = Some(ClickTarget::SidebarGroupNewAgent(0));
        let mut buf = CellBuffer::new(40, 20);
        let mut clicks = ClickMap::new();
        draw_sidebar(&mut buf, &scene, &mut clicks);

        let agent_x = (0..40)
            .find(|x| clicks.hit(*x, 2) == Some(&ClickTarget::SidebarGroupNewAgent(0)))
            .unwrap();
        let issue_x = (0..40)
            .find(|x| clicks.hit(*x, 2) == Some(&ClickTarget::SidebarGroupIssues(0)))
            .unwrap();
        assert_eq!(buf.get(agent_x, 2).bg, theme.bg_selected);
        assert_eq!(buf.get(agent_x, 2).fg, groups[0].accent);
        assert!(buf.get(agent_x, 2).attrs.contains(AttrFlags::BOLD));
        assert_ne!(buf.get(issue_x, 2).bg, theme.bg_selected);
        let pointer_text = buffer_text(&buf, 40, 20);
        assert!(pointer_text.contains("new agent"));
        assert!(pointer_text.contains("terminal"));
        assert!(pointer_text.contains("2 issues"));
        assert!(!pointer_text.contains("[n]ew agent"));
        assert!(!pointer_text.contains("[t]erminal"));
        assert!(!pointer_text.contains("[i]ssues"));

        scene.hovered = None;
        let mut keyboard = CellBuffer::new(40, 20);
        clicks.clear();
        draw_sidebar(&mut keyboard, &scene, &mut clicks);
        let keyboard_text = buffer_text(&keyboard, 40, 20);
        assert!(keyboard_text.contains("[n]ew agent"));
        assert!(keyboard_text.contains("[t]erminal"));
        assert!(keyboard_text.contains("2 [i]ssues"));

        scene.hovered = Some(ClickTarget::SidebarSettings);
        let mut footer = CellBuffer::new(40, 20);
        clicks.clear();
        draw_sidebar(&mut footer, &scene, &mut clicks);
        let settings_x = (0..40)
            .find(|x| clicks.hit(*x, 17) == Some(&ClickTarget::SidebarSettings))
            .unwrap();
        assert_eq!(footer.get(settings_x, 17).bg, theme.bg_selected);
    }
}
