//! Hermetic visual fixture for `scripts/sidebar-preview.sh`.

use std::fmt::Write as _;

use dmux_compositor::{CellBuffer, Emitter, Rect};
use dmux_ui::{draw_panel, place, project_theme, ClickMap, PanelStyle, Theme, VerticalAlign};

use super::{compose, Scene, SidebarGroup};
use crate::layout::Layout;
use crate::metrics::Metrics;
use crate::sidebar::{ProjectAction, ProjectSelection};
use crate::view_stack::OverlayOrigin;
use crate::views::ClickTarget;

#[derive(Clone, Copy)]
struct PreviewCase {
    name: &'static str,
    cols: u16,
    rows: u16,
    sidebar_width: u16,
    status: &'static str,
    hovered: Option<ClickTarget>,
    sidebar_focused: bool,
    leader_armed: bool,
    diagnostics: bool,
}

const CASES: &[PreviewCase] = &[
    PreviewCase {
        name: "standard / default",
        cols: 40,
        rows: 20,
        sidebar_width: 40,
        status: "^b for commands · ^b ? help",
        hovered: None,
        sidebar_focused: false,
        leader_armed: false,
        diagnostics: false,
    },
    PreviewCase {
        name: "standard / hovered settings",
        cols: 40,
        rows: 20,
        sidebar_width: 40,
        status: "^b for commands · ^b ? help",
        hovered: Some(ClickTarget::SidebarSettings),
        sidebar_focused: false,
        leader_armed: false,
        diagnostics: false,
    },
    PreviewCase {
        name: "standard / active leader",
        cols: 40,
        rows: 20,
        sidebar_width: 40,
        status: "^b for commands · ^b ? help",
        hovered: None,
        sidebar_focused: true,
        leader_armed: true,
        diagnostics: false,
    },
    PreviewCase {
        name: "standard / diagnostics",
        cols: 96,
        rows: 20,
        sidebar_width: 40,
        status: "diagnostics enabled",
        hovered: None,
        sidebar_focused: false,
        leader_armed: false,
        diagnostics: true,
    },
    PreviewCase {
        name: "standard / long status",
        cols: 40,
        rows: 20,
        sidebar_width: 40,
        status: "worktree provisioning is waiting for the remote branch to become available",
        hovered: None,
        sidebar_focused: false,
        leader_armed: false,
        diagnostics: false,
    },
    PreviewCase {
        name: "compact / 35 columns",
        cols: 35,
        rows: 16,
        sidebar_width: 35,
        status: "compact status",
        hovered: None,
        sidebar_focused: false,
        leader_armed: false,
        diagnostics: false,
    },
    PreviewCase {
        name: "tiny / 24 columns",
        cols: 24,
        rows: 8,
        sidebar_width: 24,
        status: "tiny status",
        hovered: None,
        sidebar_focused: false,
        leader_armed: false,
        diagnostics: false,
    },
];

fn groups() -> [SidebarGroup; 2] {
    let (primary, primary_soft) = project_theme("orange");
    let (secondary, secondary_soft) = project_theme("cyan");
    [
        SidebarGroup {
            name: "dmux-rs".into(),
            root: "/work/dmux-rs".into(),
            accent: primary,
            accent_soft: primary_soft,
            pane_indices: vec![],
            issue_label: "3 issues".into(),
            active: true,
        },
        SidebarGroup {
            name: "agentbuilder-coordinator".into(),
            root: "/work/agentbuilder-coordinator".into(),
            accent: secondary,
            accent_soft: secondary_soft,
            pane_indices: vec![],
            issue_label: "1 issue".into(),
            active: false,
        },
    ]
}

fn render_case(case: PreviewCase) -> CellBuffer {
    let theme = Theme::named("violet");
    let groups = groups();
    let layout = Layout {
        sidebar: Rect::new(0, 0, case.sidebar_width, case.rows),
        ..Default::default()
    };
    let selection = ProjectSelection {
        root: groups[0].root.clone(),
        action: ProjectAction::NewAgent,
    };
    let metrics = Metrics::new();
    let scene = Scene {
        panes: &[],
        layout: &layout,
        focused: 0,
        selected: 0,
        project_name: "dmux-rs",
        profiler: case.diagnostics.then_some(&metrics),
        profiler_pos: None,
        status_line: case.status,
        theme: &theme,
        anim: 0,
        leader_armed: case.leader_armed,
        sidebar_focused: case.sidebar_focused,
        sidebar_project: case.sidebar_focused.then_some(&selection),
        version: "dmux-rs v0.20.6",
        groups: &groups,
        pane_accents: &[],
        reorder: None,
        hovered: case.hovered,
    };
    let mut buffer = CellBuffer::new(case.cols, case.rows);
    let mut clicks = ClickMap::new();
    compose(&mut buffer, &scene, &mut clicks);
    buffer
}

fn render_project_overlay(group_index: usize) -> (CellBuffer, Rect, dmux_compositor::Color) {
    let theme = Theme::named("violet");
    let groups = groups();
    let cols = 90;
    let rows = 20;
    let layout = Layout {
        sidebar: Rect::new(0, 0, 36, rows),
        ..Default::default()
    };
    let scene = Scene {
        panes: &[],
        layout: &layout,
        focused: 0,
        selected: 0,
        project_name: "dmux-rs",
        profiler: None,
        profiler_pos: None,
        status_line: "^b for commands · ^b ? help",
        theme: &theme,
        anim: 0,
        leader_armed: false,
        sidebar_focused: false,
        sidebar_project: None,
        version: "dmux-rs v0.23.0",
        groups: &groups,
        pane_accents: &[],
        reorder: None,
        hovered: None,
    };
    let mut buffer = CellBuffer::new(cols, rows);
    let mut clicks = ClickMap::new();
    compose(&mut buffer, &scene, &mut clicks);

    let origin = OverlayOrigin::project(
        groups[group_index].root.clone(),
        ProjectAction::NewAgent,
        VerticalAlign::Top,
    );
    let source = origin.source(&clicks, &groups);
    dmux_ui::draw_scrim_except(&mut buffer, Rect::new(0, 0, cols, rows), source);
    let panel_theme = origin.theme(theme, &groups);
    let panel = place(
        Rect::new(0, 0, cols, rows),
        layout.sidebar.right(),
        origin.resolve(&clicks, &groups),
        48,
        10,
    );
    draw_panel(
        &mut buffer,
        panel,
        "New Agents",
        &panel_theme,
        PanelStyle::Modal,
    );
    (buffer, panel, groups[group_index].accent)
}

fn plain_preview() -> String {
    let mut output = String::new();
    for (index, case) in CASES.iter().copied().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        writeln!(
            output,
            "=== {} ({}x{}) ===",
            case.name, case.cols, case.rows
        )
        .unwrap();
        let buffer = render_case(case);
        for row in 0..buffer.rows() {
            for cell in buffer.row(row) {
                output.push(cell.ch);
            }
            output.push('\n');
        }
    }
    for (group_index, name) in ["dmux-rs", "agentbuilder-coordinator"]
        .into_iter()
        .enumerate()
    {
        output.push('\n');
        writeln!(output, "=== project overlay / {name} (90x20) ===").unwrap();
        let (buffer, _, _) = render_project_overlay(group_index);
        for row in 0..buffer.rows() {
            for cell in buffer.row(row) {
                output.push(cell.ch);
            }
            output.push('\n');
        }
    }
    output
}

fn ansi_preview() -> Vec<u8> {
    let mut output = Vec::new();
    for (index, case) in CASES.iter().copied().enumerate() {
        if index > 0 {
            output.push(b'\n');
        }
        output.extend_from_slice(
            format!("=== {} ({}x{}) ===\n", case.name, case.cols, case.rows).as_bytes(),
        );
        let buffer = render_case(case);
        let mut emitter = Emitter::new();
        for row in 0..buffer.rows() {
            for cell in buffer.row(row) {
                if !cell.wide_spacer() {
                    emitter.put_cell(cell);
                }
            }
            emitter.reset_style();
            output.extend_from_slice(&emitter.take());
            output.push(b'\n');
        }
    }
    for (group_index, name) in ["dmux-rs", "agentbuilder-coordinator"]
        .into_iter()
        .enumerate()
    {
        output.push(b'\n');
        output.extend_from_slice(format!("=== project overlay / {name} (90x20) ===\n").as_bytes());
        let (buffer, _, _) = render_project_overlay(group_index);
        let mut emitter = Emitter::new();
        for row in 0..buffer.rows() {
            for cell in buffer.row(row) {
                if !cell.wide_spacer() {
                    emitter.put_cell(cell);
                }
            }
            emitter.reset_style();
            output.extend_from_slice(&emitter.take());
            output.push(b'\n');
        }
    }
    output
}

#[test]
fn plain_preview_covers_widths_and_interaction_states() {
    let output = plain_preview();
    for heading in [
        "standard / default",
        "standard / hovered settings",
        "standard / active leader",
        "standard / diagnostics",
        "standard / long status",
        "compact / 35 columns",
        "tiny / 24 columns",
        "project overlay / dmux-rs",
        "project overlay / agentbuilder-coordinator",
    ] {
        assert!(output.contains(heading), "missing {heading}");
    }
    assert!(output.contains("? shortcuts"));
    assert!(output.contains("? help"));
    assert!(output.contains("profiler"));
    assert!(output.contains("worktree provisioning is waiting for …"));
    assert!(output.contains("tiny status"));
}

#[test]
fn project_overlay_preview_uses_each_source_accent() {
    let (first, first_panel, first_accent) = render_project_overlay(0);
    let (second, second_panel, second_accent) = render_project_overlay(1);

    assert_ne!(first_accent, second_accent);
    assert_eq!(first.get(first_panel.x, first_panel.y).fg, first_accent);
    assert_eq!(second.get(second_panel.x, second_panel.y).fg, second_accent);
}

#[test]
#[ignore = "run through scripts/sidebar-preview.sh"]
fn write_sidebar_preview_artifact() {
    let path = std::env::var_os("DMUX_SIDEBAR_PREVIEW_OUT")
        .expect("DMUX_SIDEBAR_PREVIEW_OUT is set by scripts/sidebar-preview.sh");
    let bytes = if std::env::var_os("NO_COLOR").is_some() {
        plain_preview().into_bytes()
    } else {
        ansi_preview()
    };
    std::fs::write(path, bytes).expect("write sidebar preview");
}
