use std::path::PathBuf;

use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_hint_bar, draw_panel, ClickMap, PanelStyle};

use super::{vkeys, ClickTarget, View, ViewCtx, ViewResult};

/// The eleven project hook names from the TS `hooks.ts` contract; dmux-rs
/// currently executes `worktree_created` and `pre_merge`.
const HOOK_NAMES: &[&str] = &[
    "before_pane_create",
    "pane_created",
    "worktree_created",
    "before_pane_close",
    "pane_closed",
    "before_worktree_remove",
    "worktree_removed",
    "pre_merge",
    "post_merge",
    "run_test",
    "run_dev",
];

enum HookState {
    Installed,
    NotExecutable,
    Missing,
}

/// Read-only inventory of `<project>/.dmux-hooks/` against the known hook
/// names, flagging present-but-not-executable scripts (the silent-failure
/// case worth surfacing).
pub struct HooksView {
    dir: PathBuf,
    rows: Vec<(&'static str, HookState)>,
}

impl HooksView {
    pub fn new(project_root: PathBuf) -> Self {
        let dir = project_root.join(".dmux-hooks");
        let rows = HOOK_NAMES
            .iter()
            .map(|name| {
                let path = dir.join(name);
                let state = match std::fs::metadata(&path) {
                    Ok(meta) if meta.is_file() => {
                        use std::os::unix::fs::PermissionsExt;
                        if meta.permissions().mode() & 0o111 != 0 {
                            HookState::Installed
                        } else {
                            HookState::NotExecutable
                        }
                    }
                    _ => HookState::Missing,
                };
                (*name, state)
            })
            .collect();
        Self { dir, rows }
    }
}

impl View for HooksView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let h = (self.rows.len() as u16 + 6).min(area.h.saturating_sub(2));
        let rect = centered(area, area.w.min(58), h);
        let inner = draw_panel(buf, rect, "Project Hooks", ctx.theme, PanelStyle::Modal);
        let bg = ctx.theme.bg_panel;

        let dir_line = self.dir.to_string_lossy();
        buf.draw_text(
            inner.x + 1,
            inner.y,
            &dir_line,
            ctx.theme.text_faint,
            bg,
            AttrFlags::ITALIC,
            inner,
        );

        for (y, (name, state)) in (inner.y + 1..).zip(&self.rows) {
            if y >= inner.bottom().saturating_sub(2) {
                break;
            }
            let (mark, color, note) = match state {
                HookState::Installed => ("✓", ctx.theme.ok, ""),
                HookState::NotExecutable => ("!", ctx.theme.warn, "not executable (chmod +x)"),
                HookState::Missing => ("–", ctx.theme.text_faint, ""),
            };
            buf.draw_text(inner.x + 1, y, mark, color, bg, AttrFlags::BOLD, inner);
            let name_color = if matches!(state, HookState::Missing) {
                ctx.theme.text_faint
            } else {
                ctx.theme.text
            };
            buf.draw_text(
                inner.x + 3,
                y,
                name,
                name_color,
                bg,
                AttrFlags::empty(),
                inner,
            );
            buf.draw_text(
                inner.x + 27,
                y,
                note,
                ctx.theme.warn,
                bg,
                AttrFlags::empty(),
                inner,
            );
        }
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &[("esc", "close")],
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) || vkeys::is_enter(key) {
            ViewResult::Close
        } else {
            ViewResult::Stay
        }
    }
}
