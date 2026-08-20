//! Hermetic visual fixture for `scripts/issues-preview.sh` (#82): renders
//! the Issues pane deterministically — no tmux, no GitHub — so layout
//! changes can be reviewed as an artifact instead of a live screenshot.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use dmux_compositor::{CellBuffer, Emitter, Rect};
use dmux_ui::{ClickMap, Theme};

use crate::github::{GitHubIssue, IssueLoadState};

use super::issues::IssueBrowserView;
use super::{View, ViewCtx};

#[derive(Clone, Copy)]
struct PreviewCase {
    name: &'static str,
    cols: u16,
    rows: u16,
    /// Focused row index and checkbox-checked issue numbers.
    focus: usize,
    checked: &'static [u64],
}

const CASES: &[PreviewCase] = &[
    PreviewCase {
        name: "wide / groups + metadata columns",
        cols: 100,
        rows: 26,
        focus: 1,
        checked: &[41, 55],
    },
    PreviewCase {
        name: "wide / nothing selected",
        cols: 100,
        rows: 26,
        focus: 0,
        checked: &[],
    },
    PreviewCase {
        name: "narrow / identity only",
        cols: 44,
        rows: 24,
        focus: 3,
        checked: &[55],
    },
];

fn issue(
    repository: &str,
    number: u64,
    title: &str,
    labels: &[&str],
    assignees: &[&str],
    updated: &str,
) -> GitHubIssue {
    GitHubIssue {
        repository: repository.into(),
        number,
        title: title.into(),
        url: format!("https://github.com/{repository}/issues/{number}"),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        assignees: assignees.iter().map(|a| a.to_string()).collect(),
        updated_at: updated.into(),
    }
}

/// Every layout ingredient the acceptance list names: assignment groups
/// (Yours / @other / Unassigned), two repository headings, a checked and a
/// focused row, a title long enough to clip, labels, and update dates.
fn fixture_state() -> IssueLoadState {
    IssueLoadState::Loaded {
        repository: "standardagents/dmux-rs".into(),
        viewer_login: "justin".into(),
        issues: vec![
            issue(
                "standardagents/dmux-rs",
                41,
                "Sidebar reorder drops focus",
                &["bug"],
                &["justin"],
                "2026-08-14T09:00:00Z",
            ),
            issue(
                "standardagents/dmux-rs",
                55,
                "A title long enough to reach every reserved metadata column in the pane",
                &["render-incident", "p1"],
                &["justin"],
                "2026-08-16T12:00:00Z",
            ),
            issue(
                "standardagents/agentbuilder",
                7,
                "Coordinator retries forever",
                &["bug"],
                &["justin"],
                "2026-08-11T08:00:00Z",
            ),
            issue(
                "standardagents/dmux-rs",
                62,
                "Palette drift under OSC 4",
                &["needs-info"],
                &["andrew"],
                "2026-08-17T10:00:00Z",
            ),
            issue(
                "standardagents/dmux-rs",
                70,
                "Welcome screen flashes on resize",
                &[],
                &[],
                "2026-08-18T22:00:00Z",
            ),
            issue(
                "standardagents/agentbuilder",
                9,
                "Document the queue contract",
                &["docs"],
                &[],
                "2026-08-10T07:00:00Z",
            ),
        ],
    }
}

fn render_case(case: PreviewCase) -> CellBuffer {
    let theme = Theme::named("violet");
    let state = Arc::new(Mutex::new(fixture_state()));
    let mut view = IssueBrowserView::new("/work/dmux-rs".into(), state);
    let checked: Vec<(String, u64)> = fixture_issues_checked(case.checked);
    view.preview_select(case.focus, checked);
    let ctx = ViewCtx {
        theme: &theme,
        anim: 0,
        hovered: None,
        sidebar_right: 0,
        anchor: dmux_ui::Anchor::SidebarTop,
    };
    let mut buf = CellBuffer::new(case.cols, case.rows);
    let mut clicks = ClickMap::new();
    view.render(
        &mut buf,
        Rect::new(0, 0, case.cols, case.rows),
        &ctx,
        &mut clicks,
    );
    buf
}

fn fixture_issues_checked(numbers: &[u64]) -> Vec<(String, u64)> {
    let IssueLoadState::Loaded { issues, .. } = fixture_state() else {
        unreachable!()
    };
    issues
        .into_iter()
        .filter(|i| numbers.contains(&i.number))
        .map(|i| (i.repository, i.number))
        .collect()
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
fn plain_preview_covers_widths_groups_and_selection() {
    let output = plain_preview();
    // Assignment groups and repository headings.
    assert!(output.contains("Yours"), "{output}");
    assert!(output.contains("@andrew"), "{output}");
    assert!(output.contains("Unassigned"), "{output}");
    assert!(output.contains("standardagents/agentbuilder"), "{output}");
    // Metadata columns on the wide render; identity-only on the narrow one.
    assert!(output.contains("LABELS"), "{output}");
    assert!(output.contains("UPDATED"), "{output}");
    assert!(output.contains("render-incide…"), "{output}");
    assert!(output.contains("2026-08-16"), "{output}");
    // Long title clipped before the metadata columns.
    assert!(output.contains('…'), "{output}");
    // Checkbox selection state renders.
    assert!(output.contains('◼'), "{output}");
    assert!(output.contains('◻'), "{output}");
    // The narrow case dropped the metadata headers.
    let narrow = output.split("narrow / identity only").nth(1).unwrap();
    assert!(!narrow.contains("LABELS"), "{narrow}");
    assert!(!narrow.contains("UPDATED"), "{narrow}");
    assert!(narrow.contains("#55"), "{narrow}");
}

#[test]
#[ignore = "run through scripts/issues-preview.sh"]
fn write_issues_preview_artifact() {
    let path = std::env::var_os("DMUX_ISSUES_PREVIEW_OUT")
        .expect("DMUX_ISSUES_PREVIEW_OUT is set by scripts/issues-preview.sh");
    let bytes = if std::env::var_os("NO_COLOR").is_some() {
        plain_preview().into_bytes()
    } else {
        ansi_preview()
    };
    std::fs::write(path, bytes).expect("write issues preview");
}
