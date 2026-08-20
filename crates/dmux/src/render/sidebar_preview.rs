//! Hermetic visual fixture for `scripts/sidebar-preview.sh`.

use std::fmt::Write as _;

use dmux_compositor::{CellBuffer, Emitter, Rect};
use dmux_ui::{project_theme, ClickMap, Theme};

use super::{compose, Scene, SidebarGroup};
use crate::layout::Layout;
use crate::metrics::Metrics;
use crate::sidebar::{ProjectAction, ProjectSelection};
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
        hud: case.diagnostics.then_some(&metrics),
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
    ] {
        assert!(output.contains(heading), "missing {heading}");
    }
    assert!(output.contains("? shortcuts"));
    assert!(output.contains("? help"));
    assert!(output.contains("── perf ──"));
    assert!(output.contains("worktree provisioning is waiting for …"));
    assert!(output.contains("tiny status"));
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
