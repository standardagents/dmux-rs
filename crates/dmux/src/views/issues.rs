//! The per-project GitHub issue browser.
//!
//! Issue retrieval is owned by the application.  This view only owns the
//! transient selection and turns user actions into commands for the app loop.

use std::collections::BTreeSet;

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Rect};
use dmux_host::{KeyCode, KeyEvent};
use dmux_ui::{
    centered, draw_button, draw_hint_bar, draw_panel, frame_height, panel_frame, spinner_frame,
    ButtonStyle, ClickMap, ListState, PanelStyle,
};

use crate::github::{GitHubIssue, IssueLoadState, SharedIssueState};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};

const TAG_ROW: u64 = 100;
const TAG_REFRESH: u64 = 1;
const TAG_OPEN: u64 = 2;
const TAG_CONTINUE: u64 = 3;

/// The issue assignment surface for one sidebar project.
pub struct IssueBrowserView {
    project_root: String,
    state: SharedIssueState,
    list: ListState,
    selected: BTreeSet<(String, u64)>,
    last_rows: Vec<(String, u64, String, String)>,
}

impl IssueBrowserView {
    pub fn new(project_root: String, state: SharedIssueState) -> Self {
        let last_rows = rows_key(&state_snapshot(&state));
        Self {
            project_root,
            state,
            list: ListState::default(),
            selected: BTreeSet::new(),
            last_rows,
        }
    }

    #[cfg(test)]
    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    fn sync_rows(&mut self, state: &IssueLoadState) {
        let rows = rows_key(state);
        if rows != self.last_rows {
            self.selected.clear();
            self.list.selected = 0;
            self.list.scroll = 0;
            self.last_rows = rows;
        }
        let len = loaded_issues(state).map_or(0, |issues| issues.len());
        self.list.clamp(len);
    }

    fn repository(&self, state: &IssueLoadState) -> Option<String> {
        match state {
            IssueLoadState::Unavailable => None,
            IssueLoadState::Loading { repository } | IssueLoadState::Error { repository, .. } => {
                repository.clone()
            }
            IssueLoadState::Loaded { repository, .. } => Some(repository.clone()),
        }
    }

    fn refresh(&mut self, state: &IssueLoadState) -> ViewResult {
        let repository = self.repository(state);
        if let Ok(mut current) = self.state.lock() {
            *current = IssueLoadState::Loading { repository };
        }
        ViewResult::Cmd(AppCmd::RefreshIssues {
            project_root: self.project_root.clone(),
        })
    }

    fn current_issue<'a>(&self, state: &'a IssueLoadState) -> Option<&'a GitHubIssue> {
        loaded_issues(state)?.get(self.list.selected)
    }

    fn open_current(&self, state: &IssueLoadState) -> ViewResult {
        match self.current_issue(state) {
            Some(issue) => ViewResult::Cmd(AppCmd::OpenUrl(issue.url.clone())),
            None => ViewResult::Stay,
        }
    }

    fn toggle_current(&mut self, state: &IssueLoadState) {
        let Some(key) = self
            .current_issue(state)
            .map(|issue| (issue.repository.clone(), issue.number))
        else {
            return;
        };
        if !self.selected.remove(&key) {
            self.selected.insert(key);
        }
    }

    /// Build the prompt passed to the chooser and then to the new agent.
    pub fn generated_prompt(&self, state: &IssueLoadState) -> String {
        let IssueLoadState::Loaded { issues, .. } = state else {
            return String::new();
        };
        let mut prompt = String::from("Work on these assigned issues:\n\n");
        for issue in issues.iter().filter(|issue| {
            self.selected
                .contains(&(issue.repository.clone(), issue.number))
        }) {
            prompt.push_str(&format!(
                "- {}#{}: {}\n  {}\n",
                issue.repository, issue.number, issue.title, issue.url
            ));
        }
        prompt.trim_end_matches('\n').to_string()
    }

    fn continue_to_agent(&self, state: &IssueLoadState) -> ViewResult {
        if self.selected.is_empty() {
            return ViewResult::Stay;
        }
        let prompt = self.generated_prompt(state);
        if prompt.is_empty() {
            return ViewResult::Stay;
        }
        ViewResult::Cmd(AppCmd::ChooseAgentForIssues {
            project_root: self.project_root.clone(),
            prompt,
        })
    }
}

fn state_snapshot(state: &SharedIssueState) -> IssueLoadState {
    state
        .lock()
        .map(|state| state.clone())
        .unwrap_or_else(|_| IssueLoadState::Error {
            repository: None,
            message: "Issue state is unavailable".into(),
        })
}

fn loaded_issues(state: &IssueLoadState) -> Option<&Vec<GitHubIssue>> {
    match state {
        IssueLoadState::Loaded { issues, .. } => Some(issues),
        _ => None,
    }
}

fn rows_key(state: &IssueLoadState) -> Vec<(String, u64, String, String)> {
    loaded_issues(state)
        .map(|issues| {
            issues
                .iter()
                .map(|issue| {
                    (
                        issue.repository.clone(),
                        issue.number,
                        issue.title.clone(),
                        issue.url.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn grouped_row_count(issues: &[GitHubIssue], start: usize, end: usize) -> usize {
    if start > end || start >= issues.len() {
        return 0;
    }
    let end = end.min(issues.len() - 1);
    let mut rows = end - start + 2;
    for index in start + 1..=end {
        if issues[index - 1].repository != issues[index].repository {
            rows += 1;
        }
    }
    rows
}

fn ensure_grouped_visible(list: &mut ListState, issues: &[GitHubIssue], visible_rows: usize) {
    if visible_rows == 0 || issues.is_empty() {
        return;
    }
    if list.selected < list.scroll {
        list.scroll = list.selected;
    }
    while list.scroll < list.selected
        && grouped_row_count(issues, list.scroll, list.selected) > visible_rows
    {
        list.scroll += 1;
    }
}

fn issue_meta(issue: &GitHubIssue) -> String {
    let mut metadata = Vec::new();
    if !issue.labels.is_empty() {
        metadata.push(issue.labels.join(", "));
    }
    if !issue.assignees.is_empty() {
        metadata.push(format!("@{}", issue.assignees.join(", @")));
    }
    if !issue.updated_at.is_empty() {
        metadata.push(format!("updated {}", issue.updated_at));
    }
    metadata.join(" · ")
}

impl View for IssueBrowserView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let state = state_snapshot(&self.state);
        self.sync_rows(&state);
        let issue_count = loaded_issues(&state).map_or(0, Vec::len);
        let repository_count = loaded_issues(&state).map_or(0, |issues| {
            issues
                .iter()
                .map(|issue| issue.repository.as_str())
                .collect::<BTreeSet<_>>()
                .len()
        });
        let max_h = area.h.saturating_sub(2);
        // Body: the grouped list (issues plus one header per repository),
        // one blank row, then the action-button row.
        let list_rows = (issue_count + repository_count).max(2) as u16;
        let h = frame_height(list_rows + 2)
            .min(max_h)
            .max(max_h.min(frame_height(4)));
        let rect = centered(area, area.w.min(100), h);
        let title = match self.repository(&state) {
            Some(repository) => format!("Issues · {repository}"),
            None => "Issues".to_string(),
        };
        let inner = draw_panel(buf, rect, &title, ctx.theme, PanelStyle::Modal);
        if inner.is_empty() {
            return None;
        }

        let frame = panel_frame(inner);
        let content = frame.content;
        let bg = ctx.theme.bg_panel;
        let rows_bottom = content.bottom().saturating_sub(2);
        let visible = rows_bottom.saturating_sub(content.y) as usize;
        if let Some(issues) = loaded_issues(&state) {
            ensure_grouped_visible(&mut self.list, issues, visible);
        }

        match &state {
            IssueLoadState::Unavailable => {
                buf.draw_text(
                    content.x,
                    content.y,
                    "No GitHub repositories found",
                    ctx.theme.text_dim,
                    bg,
                    AttrFlags::empty(),
                    inner,
                );
            }
            IssueLoadState::Loading { repository } => {
                let repo = repository.as_deref().unwrap_or("the selected project");
                buf.draw_text(
                    content.x,
                    content.y,
                    &format!(
                        "{} Loading open issues for {repo}…",
                        spinner_frame(ctx.anim)
                    ),
                    ctx.theme.text_dim,
                    bg,
                    AttrFlags::empty(),
                    inner,
                );
            }
            IssueLoadState::Error {
                repository,
                message,
            } => {
                let repo = repository.as_deref().unwrap_or("the selected project");
                buf.draw_text(
                    content.x,
                    content.y,
                    &format!("Unable to load open issues for {repo}"),
                    ctx.theme.danger,
                    bg,
                    AttrFlags::BOLD,
                    inner,
                );
                buf.draw_text(
                    content.x,
                    content.y + 1,
                    message,
                    ctx.theme.text_dim,
                    bg,
                    AttrFlags::empty(),
                    inner,
                );
            }
            IssueLoadState::Loaded { repository, issues } if issues.is_empty() => {
                buf.draw_text(
                    content.x,
                    content.y,
                    &format!("No open issues in {repository}"),
                    ctx.theme.text_dim,
                    bg,
                    AttrFlags::empty(),
                    inner,
                );
            }
            IssueLoadState::Loaded { issues, .. } => {
                let mut y = content.y;
                let mut previous_repository = None;
                for (idx, issue) in issues.iter().enumerate().skip(self.list.scroll) {
                    if y >= rows_bottom {
                        break;
                    }
                    if previous_repository.as_deref() != Some(issue.repository.as_str()) {
                        buf.draw_text(
                            content.x,
                            y,
                            &issue.repository,
                            ctx.theme.accent,
                            bg,
                            AttrFlags::BOLD,
                            inner,
                        );
                        previous_repository = Some(issue.repository.clone());
                        y += 1;
                        if y >= rows_bottom {
                            break;
                        }
                    }
                    let row_rect = Rect::new(content.x, y, content.w, 1);
                    let focused =
                        ctx.active_overlay(TAG_ROW + idx as u64, idx == self.list.selected);
                    let selected = self
                        .selected
                        .contains(&(issue.repository.clone(), issue.number));
                    let row_bg = if focused { ctx.theme.bg_selected } else { bg };
                    buf.fill(
                        row_rect,
                        &Cell {
                            bg: row_bg,
                            ..Cell::default()
                        },
                    );
                    let checkbox = if selected { "◼" } else { "◻" };
                    buf.draw_text(
                        content.x + 2,
                        y,
                        checkbox,
                        if selected {
                            ctx.theme.accent
                        } else {
                            ctx.theme.text_dim
                        },
                        row_bg,
                        AttrFlags::empty(),
                        row_rect,
                    );
                    let prefix = format!(" #{:<5} ", issue.number);
                    let x = buf.draw_text(
                        content.x + 4,
                        y,
                        &prefix,
                        ctx.theme.accent,
                        row_bg,
                        AttrFlags::BOLD,
                        row_rect,
                    );
                    let title_width =
                        content.w.saturating_sub(x.saturating_sub(content.x) + 1) as usize;
                    let meta = issue_meta(issue);
                    let text = if meta.is_empty() {
                        issue.title.clone()
                    } else {
                        format!("{}  ·  {}", issue.title, meta)
                    };
                    let clipped: String = text.chars().take(title_width).collect();
                    buf.draw_text(
                        x,
                        y,
                        &clipped,
                        if focused {
                            ctx.theme.text
                        } else {
                            ctx.theme.text_dim
                        },
                        row_bg,
                        if focused {
                            AttrFlags::BOLD
                        } else {
                            AttrFlags::empty()
                        },
                        row_rect,
                    );
                    clicks.add(row_rect, ClickTarget::Overlay(TAG_ROW + idx as u64));
                    y += 1;
                }
            }
        }

        let button_y = content.bottom().saturating_sub(1);
        let refresh = draw_button(
            buf,
            content.x,
            button_y,
            "Refresh",
            ctx.theme,
            ButtonStyle::Quiet,
            ctx.active_overlay(TAG_REFRESH, false),
            inner,
        );
        clicks.add(refresh, ClickTarget::Overlay(TAG_REFRESH));
        let open = draw_button(
            buf,
            refresh.right() + 2,
            button_y,
            "Open",
            ctx.theme,
            ButtonStyle::Quiet,
            ctx.active_overlay(TAG_OPEN, false),
            inner,
        );
        clicks.add(open, ClickTarget::Overlay(TAG_OPEN));
        let continue_style = if self.selected.is_empty() {
            ButtonStyle::Quiet
        } else {
            ButtonStyle::Primary
        };
        let continue_button = draw_button(
            buf,
            open.right() + 2,
            button_y,
            "Continue",
            ctx.theme,
            continue_style,
            ctx.active_overlay(TAG_CONTINUE, !self.selected.is_empty()),
            inner,
        );
        clicks.add(continue_button, ClickTarget::Overlay(TAG_CONTINUE));
        draw_hint_bar(
            buf,
            frame.footer,
            &[
                ("↑↓", "select"),
                ("space", "toggle"),
                ("o", "open"),
                ("r", "refresh"),
                ("enter", "continue"),
                ("esc", "close"),
            ],
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        let state = state_snapshot(&self.state);
        self.sync_rows(&state);
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_up(key) {
            let len = loaded_issues(&state).map_or(0, Vec::len);
            self.list.step(-1, len);
        } else if vkeys::is_down(key) {
            let len = loaded_issues(&state).map_or(0, Vec::len);
            self.list.step(1, len);
        } else if vkeys::is_space(key) {
            self.toggle_current(&state);
        } else if matches!(key.key, KeyCode::Char('o')) && key.modifiers.is_empty() {
            return self.open_current(&state);
        } else if matches!(key.key, KeyCode::Char('r')) && key.modifiers.is_empty() {
            return self.refresh(&state);
        } else if vkeys::is_enter(key) {
            return self.continue_to_agent(&state);
        }
        ViewResult::Stay
    }

    fn on_click(&mut self, tag: u64) -> ViewResult {
        let state = state_snapshot(&self.state);
        self.sync_rows(&state);
        match tag {
            TAG_REFRESH => self.refresh(&state),
            TAG_OPEN => self.open_current(&state),
            TAG_CONTINUE => self.continue_to_agent(&state),
            t if t >= TAG_ROW => {
                let idx = (t - TAG_ROW) as usize;
                if loaded_issues(&state).is_some_and(|issues| idx < issues.len()) {
                    self.list.selected = idx;
                    self.toggle_current(&state);
                }
                ViewResult::Stay
            }
            _ => ViewResult::Stay,
        }
    }

    fn on_hover(&mut self, tag: u64) -> u64 {
        let state = state_snapshot(&self.state);
        self.sync_rows(&state);
        if let Some(issues) = loaded_issues(&state) {
            if tag >= TAG_ROW {
                let idx = (tag - TAG_ROW) as usize;
                if idx < issues.len() {
                    self.list.selected = idx;
                }
            }
        }
        tag
    }

    fn on_wheel(&mut self, delta: i32) -> ViewResult {
        let state = state_snapshot(&self.state);
        self.sync_rows(&state);
        let len = loaded_issues(&state).map_or(0, Vec::len);
        self.list.step(if delta < 0 { -1 } else { 1 }, len);
        ViewResult::Stay
    }

    fn animating(&self) -> bool {
        self.state
            .lock()
            .map(|state| matches!(*state, IssueLoadState::Loading { .. }))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use dmux_host::Modifiers;

    fn issue(number: u64, title: &str) -> GitHubIssue {
        issue_in("owner/repo", number, title)
    }

    fn issue_in(repository: &str, number: u64, title: &str) -> GitHubIssue {
        GitHubIssue {
            repository: repository.into(),
            number,
            title: title.into(),
            url: format!("https://github.com/{repository}/issues/{number}"),
            labels: vec!["bug".into()],
            assignees: vec!["andrew".into()],
            updated_at: "2026-08-19".into(),
        }
    }

    fn key(key: KeyCode) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::NONE,
        }
    }

    fn loaded() -> (SharedIssueState, IssueBrowserView) {
        let state = Arc::new(Mutex::new(IssueLoadState::Loaded {
            repository: "owner/repo".into(),
            issues: vec![issue(1, "First"), issue(2, "Second")],
        }));
        let view = IssueBrowserView::new("/projects/repo".into(), state.clone());
        (state, view)
    }

    #[test]
    fn multi_select_toggles_rows() {
        let (_, mut view) = loaded();
        assert!(matches!(
            view.on_key(&key(KeyCode::Char(' '))),
            ViewResult::Stay
        ));
        assert_eq!(view.selected_count(), 1);
        view.on_key(&key(KeyCode::DownArrow));
        view.on_key(&key(KeyCode::Char(' ')));
        assert_eq!(view.selected_count(), 2);
        view.on_key(&key(KeyCode::Char(' ')));
        assert_eq!(view.selected_count(), 1);
    }

    #[test]
    fn hover_moves_row_selection_before_keyboard_navigation() {
        let (_, mut view) = loaded();
        assert_eq!(view.on_hover(TAG_ROW + 1), TAG_ROW + 1);
        assert_eq!(view.list.selected, 1);
        let result = view.on_key(&key(KeyCode::DownArrow));
        assert!(matches!(result, ViewResult::Stay));
        assert_eq!(view.list.selected, 0);
    }

    #[test]
    fn generated_prompt_contains_repository_title_and_url() {
        let (_, mut view) = loaded();
        let state = state_snapshot(&view.state);
        view.on_key(&key(KeyCode::Char(' ')));
        view.on_key(&key(KeyCode::DownArrow));
        view.on_key(&key(KeyCode::Char(' ')));
        let prompt = view.generated_prompt(&state);
        assert!(prompt.contains("owner/repo#1: First"));
        assert!(prompt.contains("owner/repo#2: Second"));
        assert!(prompt.contains("https://github.com/owner/repo/issues/1"));
    }

    #[test]
    fn selection_distinguishes_matching_numbers_across_repositories() {
        let state = Arc::new(Mutex::new(IssueLoadState::Loaded {
            repository: "2 repositories".into(),
            issues: vec![
                issue_in("owner/first", 1, "First repository"),
                issue_in("owner/second", 1, "Second repository"),
            ],
        }));
        let mut view = IssueBrowserView::new("/projects/coordinator".into(), state.clone());
        view.on_key(&key(KeyCode::Char(' ')));
        view.on_key(&key(KeyCode::DownArrow));
        view.on_key(&key(KeyCode::Char(' ')));

        assert_eq!(view.selected_count(), 2);
        let snapshot = state_snapshot(&state);
        let prompt = view.generated_prompt(&snapshot);
        assert!(prompt.contains("owner/first#1: First repository"));
        assert!(prompt.contains("owner/second#1: Second repository"));
        assert_eq!(
            grouped_row_count(loaded_issues(&snapshot).unwrap(), 0, 1),
            4
        );
    }

    #[test]
    fn open_returns_url_command() {
        let (_, mut view) = loaded();
        let result = view.on_key(&key(KeyCode::Char('o')));
        assert!(matches!(result, ViewResult::Cmd(AppCmd::OpenUrl(url)) if url.ends_with("/1")));
    }

    #[test]
    fn continue_requires_selection_and_keeps_view_open() {
        let (_, mut view) = loaded();
        assert!(matches!(
            view.on_key(&key(KeyCode::Enter)),
            ViewResult::Stay
        ));
        view.on_key(&key(KeyCode::Char(' ')));
        assert!(matches!(
            view.on_key(&key(KeyCode::Enter)),
            ViewResult::Cmd(AppCmd::ChooseAgentForIssues { .. })
        ));
    }

    #[test]
    fn rows_changing_clears_selection() {
        let (state, mut view) = loaded();
        view.on_key(&key(KeyCode::Char(' ')));
        assert_eq!(view.selected_count(), 1);
        *state.lock().unwrap() = IssueLoadState::Loaded {
            repository: "owner/repo".into(),
            issues: vec![issue(3, "Changed")],
        };
        view.on_key(&key(KeyCode::DownArrow));
        assert_eq!(view.selected_count(), 0);
    }

    #[test]
    fn overlay_uses_the_shared_frame_spacing_and_terminal_background() {
        let (_, mut view) = loaded();
        let mut buf = CellBuffer::new(100, 24);
        let area = buf.area();
        buf.fill(
            area,
            &Cell {
                bg: dmux_compositor::Color::Indexed(17),
                ..Cell::default()
            },
        );
        let theme = dmux_ui::Theme::default();
        let ctx = ViewCtx {
            theme: &theme,
            anim: 0,
            hovered: None,
        };
        let mut clicks = ClickMap::new();
        view.render(&mut buf, area, &ctx, &mut clicks);

        let row_text = |y: u16| -> String { (0..100).map(|x| buf.get(x, y).ch).collect() };
        let hint_y = (0..24)
            .find(|y| row_text(*y).contains("toggle"))
            .expect("hint bar rendered");
        let button_y = (0..24)
            .find(|y| row_text(*y).contains("Refresh"))
            .expect("action row rendered");
        // #42 spacing contract: action row, one blank row, then the hint bar.
        assert_eq!(hint_y, button_y + 2);
        let blank_inside = |y: u16| {
            let row = row_text(y);
            row.chars().skip(2).take(96).all(|c| c == ' ')
        };
        assert!(blank_inside(button_y + 1), "gap row below the action row");
        // The list is separated from the action row by one blank row too.
        assert!(blank_inside(button_y - 1), "gap row above the action row");
        // Panel surface inherits the terminal background (#42); the second
        // row is unfocused, so it carries the panel surface, not selection.
        let list_y = (0..24)
            .find(|y| row_text(*y).contains("Second"))
            .expect("issue row rendered");
        assert_eq!(buf.get(50, list_y).bg, dmux_compositor::Color::Default);
    }

    #[test]
    fn loading_empty_and_error_states_are_safe() {
        for state in [
            IssueLoadState::Unavailable,
            IssueLoadState::Loading {
                repository: Some("owner/repo".into()),
            },
            IssueLoadState::Loaded {
                repository: "owner/repo".into(),
                issues: Vec::new(),
            },
            IssueLoadState::Error {
                repository: Some("owner/repo".into()),
                message: "network unavailable".into(),
            },
        ] {
            let shared = Arc::new(Mutex::new(state));
            let mut view = IssueBrowserView::new("/projects/repo".into(), shared);
            let mut buf = CellBuffer::new(100, 20);
            let theme = dmux_ui::Theme::default();
            let ctx = ViewCtx {
                theme: &theme,
                anim: 0,
                hovered: None,
            };
            let mut clicks = ClickMap::new();
            let area = buf.area();
            view.render(&mut buf, area, &ctx, &mut clicks);
        }
    }
}
