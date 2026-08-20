use std::path::PathBuf;

use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{draw_hint_bar, draw_panel, ClickMap, PanelStyle};

use super::{vkeys, ClickTarget, View, ViewCtx, ViewResult};

/// Diff peek for a worktree pane: uncommitted changes first, else what the
/// branch adds over its merge base. Read-only pager with +/- coloring.
pub struct DiffView {
    title: String,
    lines: Vec<String>,
    scroll: usize,
}

fn git_out(dir: &PathBuf, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl DiffView {
    pub fn new(title: String, worktree: PathBuf) -> Self {
        // Uncommitted work is what you usually want to peek at; a clean tree
        // falls back to the branch's committed delta against HEAD's upstream
        // merge base (best effort — plain `HEAD` diff when there is none).
        let mut text =
            git_out(&worktree, &["diff", "HEAD", "--stat", "--patch"]).unwrap_or_default();
        // `diff HEAD` misses brand-new files; surface them explicitly.
        let untracked = git_out(&worktree, &["ls-files", "--others", "--exclude-standard"])
            .unwrap_or_default()
            .lines()
            .map(|f| format!("+ new file: {f} (untracked)"))
            .collect::<Vec<_>>();
        if !untracked.is_empty() {
            text = format!("{}\n{text}", untracked.join("\n"));
        }
        let mut heading = "uncommitted changes";
        if text.trim().is_empty() {
            heading = "committed changes vs merge base";
            let base = git_out(&worktree, &["merge-base", "HEAD", "@{-1}"])
                .or_else(|| git_out(&worktree, &["merge-base", "HEAD", "main"]))
                .or_else(|| git_out(&worktree, &["merge-base", "HEAD", "master"]))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let Some(base) = base {
                text =
                    git_out(&worktree, &["diff", &base, "--stat", "--patch"]).unwrap_or_default();
            }
        }
        let mut lines: Vec<String> = vec![format!("· {heading}"), String::new()];
        if text.trim().is_empty() {
            lines.push("(no changes)".to_string());
        } else {
            lines.extend(text.lines().take(5000).map(String::from));
        }
        Self {
            title,
            lines,
            scroll: 0,
        }
    }
}

impl View for DiffView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let rect = ctx.global(
            area,
            area.w.saturating_sub(8).min(140),
            area.h.saturating_sub(4),
        );
        let inner = draw_panel(buf, rect, &self.title, ctx.theme, PanelStyle::Modal);
        let visible = inner.h.saturating_sub(1) as usize;
        let max_scroll = self.lines.len().saturating_sub(visible);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        let bg = ctx.theme.bg_panel;
        for (row, line) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(visible)
            .enumerate()
        {
            let fg = if line.starts_with('+') && !line.starts_with("+++") {
                ctx.theme.ok
            } else if line.starts_with('-') && !line.starts_with("---") {
                ctx.theme.danger
            } else if line.starts_with("@@") {
                ctx.theme.accent
            } else if line.starts_with("diff ") || line.starts_with("index ") {
                ctx.theme.text_faint
            } else {
                ctx.theme.text_dim
            };
            let clipped: String = line.chars().take(inner.w as usize).collect();
            buf.draw_text(
                inner.x,
                inner.y + row as u16,
                &clipped,
                fg,
                bg,
                AttrFlags::empty(),
                inner,
            );
        }
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &[("↑↓/wheel", "scroll"), ("esc", "close")],
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_up(key) {
            self.scroll = self.scroll.saturating_sub(1);
        } else if vkeys::is_down(key) {
            self.scroll = self.scroll.saturating_add(1);
        } else if matches!(key.key, dmux_host::KeyCode::PageUp) {
            self.scroll = self.scroll.saturating_sub(20);
        } else if matches!(key.key, dmux_host::KeyCode::PageDown) {
            self.scroll = self.scroll.saturating_add(20);
        }
        ViewResult::Stay
    }

    fn on_wheel(&mut self, delta: i32) -> ViewResult {
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(3);
        } else {
            self.scroll = self.scroll.saturating_add(3);
        }
        ViewResult::Stay
    }
}
