use dmux_compositor::{AttrFlags, Cell, CellBuffer, Rect};
use dmux_host::{KeyCode, KeyEvent};
use dmux_ui::{
    centered, draw_button, draw_counter, draw_hint_bar, draw_panel, draw_select_value,
    frame_height, panel_frame, ButtonStyle, ClickMap, PanelStyle, TextInput,
};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};
use crate::agents::{AgentDef, AGENTS};
use dmux_core::i18n::t;

const TAG_LAUNCH: u64 = 1;
const TAG_PROMPT: u64 = 2;
const TAG_PERMISSION: u64 = 3;
const TAG_ROW: u64 = 100;
const TAG_MINUS: u64 = 200;
const TAG_PLUS: u64 = 300;
const MAX_PROMPT_ROWS: u16 = 6;

const PERMISSION_MODES: [(&str, &str); 4] = [
    ("", "Agent default (ask)"),
    ("plan", "Plan mode"),
    ("acceptEdits", "Accept edits"),
    ("bypassPermissions", "Bypass permissions"),
];

struct AgentRow {
    def: &'static AgentDef,
    installed: bool,
    count: u8,
}

/// The new-pane flow: one prompt, allocated across N panes of any mix of
/// agents ("run this in 2× Claude Code and 1× Codex"). Something the tmux
/// popup era never offered.
pub struct AgentSelectView {
    prompt: TextInput,
    rows: Vec<AgentRow>,
    /// 0 = prompt, 1..=rows = agent rows, rows+1 = permission, rows+2 = launch.
    focus: usize,
    permission_idx: usize,
    project_root: Option<String>,
    close_parent_on_launch: bool,
}

impl AgentSelectView {
    pub fn new(
        installed: &std::collections::HashSet<&'static str>,
        enabled: &[String],
        default_agent: Option<&str>,
        default_mode: &str,
        project_root: Option<String>,
    ) -> Self {
        let mut rows: Vec<AgentRow> = AGENTS
            .iter()
            .filter(|def| enabled.iter().any(|id| id == def.id))
            .map(|def| AgentRow {
                def,
                installed: installed.contains(def.id),
                count: 0,
            })
            .collect();
        if rows.is_empty() {
            rows = AGENTS
                .iter()
                .filter(|d| d.default_enabled)
                .map(|def| AgentRow {
                    def,
                    installed: installed.contains(def.id),
                    count: 0,
                })
                .collect();
        }
        // Installed first, then default-enabled, stable within groups.
        rows.sort_by_key(|r| (!r.installed, !r.def.default_enabled));
        // Preselect: one pane on the configured default agent (if installed),
        // else on the first installed agent.
        let pre = rows
            .iter()
            .position(|r| r.installed && default_agent == Some(r.def.id))
            .or_else(|| rows.iter().position(|r| r.installed));
        if let Some(i) = pre {
            rows[i].count = 1;
        }
        let permission_idx = PERMISSION_MODES
            .iter()
            .position(|(v, _)| *v == default_mode)
            .unwrap_or(0);
        Self {
            prompt: TextInput::default().placeholder("What should the agents do?"),
            rows,
            focus: 0,
            permission_idx,
            project_root,
            close_parent_on_launch: false,
        }
    }

    pub fn with_issue_prompt(mut self, prompt: String) -> Self {
        let placeholder = std::mem::take(&mut self.prompt.placeholder);
        self.prompt = TextInput::with_value(prompt).placeholder(placeholder);
        self.close_parent_on_launch = true;
        self
    }

    fn total(&self) -> u32 {
        self.rows.iter().map(|r| r.count as u32).sum()
    }

    fn adjust_row(&mut self, idx: usize, delta: i16) {
        if let Some(row) = self.rows.get_mut(idx) {
            if !row.installed {
                return;
            }
            row.count = (row.count as i16 + delta).clamp(0, 9) as u8;
        }
    }

    fn launch(&self) -> ViewResult {
        if self.total() == 0 {
            return ViewResult::Stay;
        }
        let allocations: Vec<(String, u8)> = self
            .rows
            .iter()
            .filter(|r| r.count > 0)
            .map(|r| (r.def.id.to_string(), r.count))
            .collect();
        let command = AppCmd::LaunchAgents {
            prompt: self.prompt.value.trim().to_string(),
            allocations,
            mode: PERMISSION_MODES[self.permission_idx].0.to_string(),
            project_root: self.project_root.clone(),
        };
        if self.close_parent_on_launch {
            ViewResult::CloseTwoAnd(command)
        } else {
            ViewResult::CloseAnd(command)
        }
    }

    fn zones(&self) -> usize {
        self.rows.len() + 3
    }

    fn prompt_rows(&self, panel_width: u16, max_height: u16) -> u16 {
        let capacity = max_height
            .saturating_sub(self.rows.len() as u16 + 12)
            .max(1);
        self.prompt
            .wrapped_line_count(panel_width.saturating_sub(4))
            .min(MAX_PROMPT_ROWS)
            .min(capacity)
    }
}

impl View for AgentSelectView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let panel_width = area.w.min(58);
        let max_h = area.h.saturating_sub(2);
        let prompt_rows = self.prompt_rows(panel_width, max_h);
        // Body: prompt label + prompt, blank, allocation label + rows,
        // blank, permission row, blank, launch button.
        let h = frame_height(self.rows.len() as u16 + prompt_rows + 7).min(max_h);
        let rect = centered(area, panel_width, h);
        let inner = draw_panel(buf, rect, t("agent.title"), ctx.theme, PanelStyle::Modal);
        let frame = panel_frame(inner);
        let content = frame.content;
        let bg = ctx.theme.bg_panel;

        buf.draw_text(
            content.x + 1,
            content.y,
            t("agent.prompt_label"),
            ctx.theme.text_dim,
            bg,
            AttrFlags::empty(),
            inner,
        );
        let prompt_rect = Rect::new(content.x, content.y + 1, content.w, prompt_rows);
        let cursor = self
            .prompt
            .draw_wrapped(buf, prompt_rect, ctx.theme, self.focus == 0);
        clicks.add(prompt_rect, ClickTarget::Overlay(TAG_PROMPT));

        buf.draw_text(
            content.x + 1,
            content.y + prompt_rows + 2,
            t("agent.allocate"),
            ctx.theme.text_dim,
            bg,
            AttrFlags::empty(),
            inner,
        );
        let rows_y = content.y + prompt_rows + 3;
        for (i, row) in self.rows.iter().enumerate() {
            let y = rows_y + i as u16;
            if y >= content.bottom().saturating_sub(4) {
                break;
            }
            let selected = self.focus == i + 1;
            let row_rect = Rect::new(content.x, y, content.w, 1);
            let row_bg = if selected { ctx.theme.bg_selected } else { bg };
            buf.fill(
                row_rect,
                &Cell {
                    bg: row_bg,
                    ..Cell::default()
                },
            );
            let caret = if selected { "▸ " } else { "  " };
            buf.draw_text(
                content.x,
                y,
                caret,
                ctx.theme.accent,
                row_bg,
                AttrFlags::BOLD,
                row_rect,
            );
            let name_fg = if !row.installed {
                ctx.theme.text_faint
            } else if row.count > 0 {
                ctx.theme.text
            } else {
                ctx.theme.text_dim
            };
            let x = buf.draw_text(
                content.x + 2,
                y,
                row.def.name,
                name_fg,
                row_bg,
                if row.count > 0 {
                    AttrFlags::BOLD
                } else {
                    AttrFlags::empty()
                },
                row_rect,
            );
            buf.draw_text(
                x + 1,
                y,
                &format!("[{}]", row.def.short),
                ctx.theme.text_faint,
                row_bg,
                AttrFlags::empty(),
                row_rect,
            );
            if row.installed {
                let (minus, plus) = draw_counter(
                    buf,
                    content.right().saturating_sub(9),
                    y,
                    row.count,
                    ctx.theme,
                    selected,
                    row_rect,
                );
                clicks.add(minus, ClickTarget::Overlay(TAG_MINUS + i as u64));
                clicks.add(plus, ClickTarget::Overlay(TAG_PLUS + i as u64));
            } else {
                let label = "not installed";
                buf.draw_text(
                    content.right().saturating_sub(label.len() as u16 + 1),
                    y,
                    label,
                    ctx.theme.text_faint,
                    row_bg,
                    AttrFlags::ITALIC,
                    row_rect,
                );
            }
            clicks.add(
                Rect::new(row_rect.x, y, row_rect.w.saturating_sub(10), 1),
                ClickTarget::Overlay(TAG_ROW + i as u64),
            );
        }

        // Permission mode row.
        let perm_y = content.bottom().saturating_sub(3);
        let perm_selected = self.focus == self.rows.len() + 1;
        let perm_rect = Rect::new(content.x, perm_y, content.w, 1);
        let perm_bg = if perm_selected {
            ctx.theme.bg_selected
        } else {
            bg
        };
        buf.fill(
            perm_rect,
            &Cell {
                bg: perm_bg,
                ..Cell::default()
            },
        );
        buf.draw_text(
            content.x + 1,
            perm_y,
            t("agent.permissions"),
            ctx.theme.text_dim,
            perm_bg,
            AttrFlags::empty(),
            perm_rect,
        );
        let value = draw_select_value(PERMISSION_MODES[self.permission_idx].1);
        buf.draw_text(
            content
                .right()
                .saturating_sub(value.chars().count() as u16 + 1),
            perm_y,
            &value,
            ctx.theme.accent,
            perm_bg,
            AttrFlags::empty(),
            perm_rect,
        );
        clicks.add(perm_rect, ClickTarget::Overlay(TAG_PERMISSION));

        // Launch button.
        let launch_y = content.bottom().saturating_sub(1);
        let total = self.total();
        let label = match total {
            0 => "Launch".to_string(),
            1 => "Launch 1 pane".to_string(),
            n => format!("Launch {n} panes"),
        };
        let launch_focused = self.focus == self.rows.len() + 2;
        let btn = draw_button(
            buf,
            content.x + (content.w.saturating_sub(label.len() as u16 + 2)) / 2,
            launch_y,
            &label,
            ctx.theme,
            ButtonStyle::Primary,
            launch_focused || total > 0,
            inner,
        );
        clicks.add(btn, ClickTarget::Overlay(TAG_LAUNCH));

        draw_hint_bar(
            buf,
            frame.footer,
            &[
                ("↑↓", "field"),
                ("←→", "count"),
                ("⏎", "launch"),
                ("esc", "close"),
            ],
            ctx.theme,
        );
        if self.focus == 0 {
            cursor
        } else {
            None
        }
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        let zones = self.zones();
        if matches!(key.key, KeyCode::UpArrow)
            || (vkeys::is_tab(key) && key.modifiers.contains(dmux_host::Modifiers::SHIFT))
        {
            self.focus = (self.focus + zones - 1) % zones;
            return ViewResult::Stay;
        }
        if matches!(key.key, KeyCode::DownArrow) || vkeys::is_tab(key) {
            self.focus = (self.focus + 1) % zones;
            return ViewResult::Stay;
        }
        if vkeys::is_enter(key) {
            return self.launch();
        }

        if self.focus == 0 {
            if let Some(ik) = vkeys::as_input_key(key) {
                self.prompt.handle(ik);
            }
            return ViewResult::Stay;
        }
        if self.focus <= self.rows.len() {
            let idx = self.focus - 1;
            match key.key {
                KeyCode::LeftArrow | KeyCode::Char('-') => self.adjust_row(idx, -1),
                KeyCode::RightArrow | KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.adjust_row(idx, 1)
                }
                KeyCode::Char(c @ '0'..='9') => {
                    if let Some(row) = self.rows.get_mut(idx) {
                        if row.installed {
                            row.count = c as u8 - b'0';
                        }
                    }
                }
                _ => {}
            }
            return ViewResult::Stay;
        }
        if self.focus == self.rows.len() + 1 {
            let n = PERMISSION_MODES.len();
            match key.key {
                KeyCode::LeftArrow => self.permission_idx = (self.permission_idx + n - 1) % n,
                KeyCode::RightArrow | KeyCode::Char(' ') => {
                    self.permission_idx = (self.permission_idx + 1) % n
                }
                _ => {}
            }
        }
        ViewResult::Stay
    }

    fn on_click(&mut self, tag: u64) -> ViewResult {
        match tag {
            TAG_LAUNCH => return self.launch(),
            TAG_PROMPT => self.focus = 0,
            TAG_PERMISSION => {
                self.focus = self.rows.len() + 1;
                self.permission_idx = (self.permission_idx + 1) % PERMISSION_MODES.len();
            }
            t if t >= TAG_PLUS => {
                let idx = (t - TAG_PLUS) as usize;
                self.focus = idx + 1;
                self.adjust_row(idx, 1);
            }
            t if t >= TAG_MINUS => {
                let idx = (t - TAG_MINUS) as usize;
                self.focus = idx + 1;
                self.adjust_row(idx, -1);
            }
            t if t >= TAG_ROW => {
                let idx = (t - TAG_ROW) as usize;
                self.focus = idx + 1;
                self.adjust_row(idx, 1);
            }
            _ => {}
        }
        ViewResult::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::Modifiers;

    #[test]
    fn launch_retains_the_sidebar_project_root() {
        let agent = &AGENTS[0];
        let installed = std::collections::HashSet::from([agent.id]);
        let mut view = AgentSelectView::new(
            &installed,
            &[agent.id.to_string()],
            Some(agent.id),
            "",
            Some("/projects/empty".into()),
        );
        view.prompt.value = "Fix the issue".into();

        let ViewResult::CloseAnd(AppCmd::LaunchAgents { project_root, .. }) = view.launch() else {
            panic!("expected an agent launch command");
        };
        assert_eq!(project_root.as_deref(), Some("/projects/empty"));
    }

    #[test]
    fn issue_launch_closes_the_chooser_and_issue_browser() {
        let agent = &AGENTS[0];
        let installed = std::collections::HashSet::from([agent.id]);
        let view = AgentSelectView::new(
            &installed,
            &[agent.id.to_string()],
            Some(agent.id),
            "",
            Some("/projects/coordinator".into()),
        )
        .with_issue_prompt("Work on owner/repo#1".into());

        assert!(matches!(
            view.launch(),
            ViewResult::CloseTwoAnd(AppCmd::LaunchAgents { .. })
        ));
    }

    #[test]
    fn issue_prompt_allocates_multiple_visual_rows() {
        let agent = &AGENTS[0];
        let installed = std::collections::HashSet::from([agent.id]);
        let view = AgentSelectView::new(
            &installed,
            &[agent.id.to_string()],
            Some(agent.id),
            "",
            None,
        )
        .with_issue_prompt(
            "Work on these assigned issues:\n\n- owner/repo#123: A long issue title that wraps\n  https://github.com/owner/repo/issues/123".into(),
        );
        assert!(view.prompt_rows(58, 28) > 1);
        assert_eq!(view.prompt.cursor, view.prompt.value.len());
    }

    #[test]
    fn cancelling_issue_launch_returns_to_the_issue_browser() {
        let agent = &AGENTS[0];
        let installed = std::collections::HashSet::from([agent.id]);
        let mut view = AgentSelectView::new(
            &installed,
            &[agent.id.to_string()],
            Some(agent.id),
            "",
            None,
        )
        .with_issue_prompt("Work on owner/repo#1".into());
        let escape = KeyEvent {
            key: KeyCode::Escape,
            modifiers: Modifiers::NONE,
        };

        assert!(matches!(view.on_key(&escape), ViewResult::Close));
    }
}
