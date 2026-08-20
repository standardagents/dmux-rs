//! Native worktree-bootstrap experience: while a new worktree pane is being
//! prepared (worktree add, project setup hook, agent launch), the pane body
//! shows a composed loader card — step checklist, live output line, progress
//! bar — instead of raw shell scroll. The work itself runs on a blocking
//! task; the pane's shell only ever receives the final agent launch line.

use std::io::BufRead;
use std::time::Instant;

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Rect};
use dmux_ui::{centered, draw_panel, spinner_frame, PanelStyle, Theme};

/// What the runner reports back to the app loop.
#[derive(Debug)]
pub enum Ev {
    Step(usize),
    Detail(String),
    Failed(String),
    Done,
}

/// Inputs for the blocking runner.
#[derive(Debug)]
pub struct Plan {
    pub root: String,
    pub wt: String,
    pub branch: String,
    pub base_branch: String,
    pub slug: String,
    pub has_hook: bool,
}

/// The agent start deferred until the bootstrap finishes.
#[derive(Debug)]
pub struct Launch {
    pub agent_cmd: String,
    pub wt: String,
    pub root: String,
    pub injection: Option<(String, u64)>,
}

/// Per-pane loader state, keyed by slug in the app.
pub struct Ui {
    pub pane: dmux_cc::PaneId,
    pub title: String,
    pub agent_label: String,
    pub branch: String,
    pub steps: Vec<String>,
    pub current: usize,
    pub detail: String,
    pub started: Instant,
    pub done_at: Option<Instant>,
    pub failed: Option<String>,
    pub launch: Option<Launch>,
}

impl Ui {
    pub fn step_labels(agent_label: &str, has_hook: bool) -> Vec<String> {
        let mut steps = vec!["Creating worktree".to_string()];
        if has_hook {
            steps.push("Project setup · worktree_created".to_string());
        }
        steps.push(format!("Launching {agent_label}"));
        steps
    }
}

/// Run one shell step with merged output, streaming every line as a Detail
/// event. Returns the exit success.
fn stream_step(
    shell_cmd: &str,
    cwd: Option<&str>,
    envs: &[(&str, &str)],
    emit: &mut dyn FnMut(Ev),
) -> bool {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("{{ {shell_cmd} ; }} 2>&1"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            emit(Ev::Detail(format!("spawn failed: {err}")));
            return false;
        }
    };
    if let Some(out) = child.stdout.take() {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let line = line.trim_end().to_string();
            if !line.trim().is_empty() {
                emit(Ev::Detail(line));
            }
        }
    }
    child.wait().map(|st| st.success()).unwrap_or(false)
}

/// The bootstrap itself: worktree add, then the project's `worktree_created`
/// hook (streamed; its failure is reported but non-fatal, matching the
/// fire-and-forget hook contract). Blocking — run on `spawn_blocking`.
pub fn run_blocking(plan: &Plan, emit: &mut dyn FnMut(Ev)) {
    let q = crate::shq;
    emit(Ev::Step(0));
    let base = if plan.base_branch.is_empty() {
        String::new()
    } else {
        format!(" {}", q(&plan.base_branch))
    };
    let add = format!(
        "git -C {root} worktree add -b {branch} {wt}{base} || git -C {root} worktree add {wt} {branch}",
        root = q(&plan.root),
        branch = q(&plan.branch),
        wt = q(&plan.wt),
    );
    if !stream_step(&add, None, &[], emit) {
        emit(Ev::Failed("git worktree add failed".into()));
        return;
    }

    let mut step = 1;
    if plan.has_hook {
        emit(Ev::Step(step));
        step += 1;
        let hook = format!("{}/.dmux-hooks/worktree_created", plan.root);
        let ok = stream_step(
            &format!("exec {}", q(&hook)),
            Some(&plan.wt),
            &[
                ("DMUX_ROOT", plan.root.as_str()),
                ("DMUX_SLUG", plan.slug.as_str()),
                ("DMUX_WORKTREE_PATH", plan.wt.as_str()),
                ("DMUX_BRANCH", plan.branch.as_str()),
            ],
            emit,
        );
        if !ok {
            emit(Ev::Detail(
                "worktree_created hook exited nonzero (continuing)".into(),
            ));
        }
    }

    emit(Ev::Step(step));
    emit(Ev::Done);
}

/// Paint the loader over a pane's body rect.
/// Draw every bootstrapping pane's loader card in the owning project's
/// accent (#107). `accents` is parallel to `panes` and recomputed on every
/// sidebar rebuild, so reorders, theme changes, and resizes cannot
/// cross-color a card; success/warning/failure keep their status colors.
pub fn draw_all(
    buf: &mut CellBuffer,
    panes: &[crate::session::LogicalPane],
    accents: &[(dmux_compositor::Color, dmux_compositor::Color)],
    theme: &Theme,
    bootstraps: &std::collections::HashMap<String, Ui>,
    anim: u64,
) {
    if bootstraps.is_empty() {
        return;
    }
    for (i, p) in panes.iter().enumerate() {
        if let (Some(rect), Some(ui)) = (p.rect, bootstraps.get(&p.slug)) {
            let themed = match accents.get(i) {
                Some((accent, accent_soft)) => Theme {
                    accent: *accent,
                    accent_soft: *accent_soft,
                    ..*theme
                },
                None => *theme,
            };
            draw(buf, rect, &themed, ui, anim);
        }
    }
}

pub fn draw(buf: &mut CellBuffer, rect: Rect, theme: &Theme, ui: &Ui, anim: u64) {
    // The pane's shell may already have painted a prompt — cover it all.
    buf.fill(
        rect,
        &Cell {
            bg: theme.bg,
            ..Cell::default()
        },
    );

    let total = ui.steps.len();
    let h = (total as u16 + 8).min(rect.h);
    let w = rect.w.saturating_sub(4).clamp(24, 56);
    let card = centered(rect, w, h);
    let inner = draw_panel(buf, card, "Preparing worktree", theme, PanelStyle::Modal);
    let bg = theme.bg_panel;

    // Identity line: what's being set up, for whom.
    let head = format!("{} · {}", ui.title, ui.agent_label);
    buf.draw_text(
        inner.x + 1,
        inner.y,
        &head,
        theme.text,
        bg,
        AttrFlags::BOLD,
        inner,
    );
    let branch = format!("⎇ {}", ui.branch);
    buf.draw_text(
        inner.x + 1,
        inner.y + 1,
        &branch,
        theme.text_faint,
        bg,
        AttrFlags::empty(),
        inner,
    );

    // Step checklist.
    let steps_y = inner.y + 3;
    for (i, label) in ui.steps.iter().enumerate() {
        let y = steps_y + i as u16;
        if y >= inner.bottom().saturating_sub(3) {
            break;
        }
        let done_all = ui.done_at.is_some() && ui.failed.is_none();
        let (glyph, color, label_color) = if ui.failed.is_some() && i == ui.current {
            ("✗".to_string(), theme.danger, theme.danger)
        } else if i < ui.current || done_all {
            ("✓".to_string(), theme.ok, theme.text_dim)
        } else if i == ui.current {
            (spinner_frame(anim).to_string(), theme.accent, theme.text)
        } else {
            ("○".to_string(), theme.text_faint, theme.text_faint)
        };
        buf.draw_text(inner.x + 2, y, &glyph, color, bg, AttrFlags::BOLD, inner);
        buf.draw_text(
            inner.x + 4,
            y,
            label,
            label_color,
            bg,
            AttrFlags::empty(),
            inner,
        );
    }

    // Progress bar + counters.
    let bar_y = inner.bottom().saturating_sub(2);
    let elapsed = ui.started.elapsed().as_secs();
    let counter = format!(" {}/{} · {}s", (ui.current + 1).min(total), total, elapsed);
    let bar_w = (inner.w as usize).saturating_sub(2 + counter.len());
    if bar_w >= 4 {
        let frac = if ui.done_at.is_some() && ui.failed.is_none() {
            1.0
        } else {
            ui.current as f32 / total as f32
        };
        let filled = (frac * bar_w as f32).round() as usize;
        for i in 0..bar_w {
            let x = inner.x + 1 + i as u16;
            let (ch, fg) = if i < filled {
                (
                    '▰',
                    if ui.failed.is_some() {
                        theme.danger
                    } else {
                        theme.accent
                    },
                )
            } else if i == filled && ui.done_at.is_none() && ui.failed.is_none() {
                // A breathing leading edge keeps the bar alive between steps.
                if anim % 6 < 3 {
                    ('▰', theme.accent_soft)
                } else {
                    ('▱', theme.border)
                }
            } else {
                ('▱', theme.border)
            };
            buf.set(
                x,
                bar_y,
                Cell {
                    ch,
                    fg,
                    bg,
                    ..Cell::default()
                },
            );
        }
        buf.draw_text(
            inner.x + 1 + bar_w as u16,
            bar_y,
            &counter,
            theme.text_faint,
            bg,
            AttrFlags::empty(),
            inner,
        );
    }

    // Live output line (hook/npm/git chatter) or the failure reason.
    let detail_y = inner.bottom().saturating_sub(1);
    let (line, color) = match &ui.failed {
        // The last streamed line (git's actual complaint) beats the generic
        // failure label when we have one.
        Some(err) if ui.detail.is_empty() => (err.clone(), theme.danger),
        Some(_) => (ui.detail.clone(), theme.danger),
        None => (ui.detail.clone(), theme.text_faint),
    };
    let clipped: String = line
        .chars()
        .take(inner.w.saturating_sub(2) as usize)
        .collect();
    buf.draw_text(
        inner.x + 1,
        detail_y,
        &clipped,
        color,
        bg,
        AttrFlags::ITALIC,
        inner,
    );
}

#[cfg(test)]
mod accent_tests {
    use super::*;
    use dmux_compositor::Color;

    fn card_ui() -> Ui {
        Ui {
            pane: dmux_cc::PaneId(1),
            title: "issue-worktree".into(),
            agent_label: "claude".into(),
            branch: "issue-1".into(),
            steps: Ui::step_labels("claude", false),
            current: 1,
            detail: String::new(),
            started: Instant::now(),
            done_at: None,
            failed: None,
            launch: None,
        }
    }

    fn accent_cells(buf: &CellBuffer, accent: Color) -> usize {
        let area = buf.area();
        (0..area.h)
            .flat_map(|y| (0..area.w).map(move |x| (x, y)))
            .filter(|(x, y)| buf.get(*x, *y).fg == accent)
            .count()
    }

    #[test]
    fn cards_use_their_own_project_accent_and_keep_status_colors() {
        // Two projects with distinct accents (#107): each card's border,
        // heading, and progress glyphs carry ITS project's accent; the
        // other project's accent never appears, and error state keeps the
        // status color under both.
        let base = Theme::default();
        let orange = dmux_ui::project_theme("orange");
        let cyan = dmux_ui::project_theme("cyan");
        let themes = [
            Theme {
                accent: orange.0,
                accent_soft: orange.1,
                ..base
            },
            Theme {
                accent: cyan.0,
                accent_soft: cyan.1,
                ..base
            },
        ];
        let ui = card_ui();
        for (theme, other) in [(themes[0], themes[1]), (themes[1], themes[0])] {
            let mut buf = CellBuffer::new(60, 16);
            draw(&mut buf, Rect::new(0, 0, 60, 16), &theme, &ui, 0);
            assert!(
                accent_cells(&buf, theme.accent) > 0,
                "card carries its own accent"
            );
            assert_eq!(
                accent_cells(&buf, other.accent),
                0,
                "the other project's accent never appears"
            );
        }
        // Failure semantics stay on the status color for any accent.
        let mut failed = card_ui();
        failed.failed = Some("clone failed".into());
        let mut buf = CellBuffer::new(60, 16);
        draw(&mut buf, Rect::new(0, 0, 60, 16), &themes[0], &failed, 0);
        assert!(
            accent_cells(&buf, base.danger) > 0,
            "error state renders in the status color"
        );
    }
}
