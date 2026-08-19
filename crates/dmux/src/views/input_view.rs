use dmux_compositor::{CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_hint_bar, draw_panel, ClickMap, PanelStyle, TextInput};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};

/// What a submitted input means; keeps views free of closures.
#[derive(Debug, Clone)]
pub enum InputPurpose {
    RenamePane(usize),
    SetTextSetting {
        key: String,
        scope: dmux_core::SettingsScope,
    },
    AddProjectPath,
    /// Commit message before merging a dirty worktree.
    MergeCommitMessage {
        slug: String,
    },
    /// Scrollback search in the focused pane.
    SearchScrollback,
}

pub struct InputView {
    title: String,
    input: TextInput,
    purpose: InputPurpose,
}

impl InputView {
    pub fn new(
        title: impl Into<String>,
        initial: &str,
        placeholder: &str,
        purpose: InputPurpose,
    ) -> Self {
        Self {
            title: title.into(),
            input: TextInput::with_value(initial).placeholder(placeholder),
            purpose,
        }
    }

    fn submit(&self) -> ViewResult {
        let value = self.input.value.trim().to_string();
        let cmd = match &self.purpose {
            InputPurpose::RenamePane(idx) => {
                if value.is_empty() {
                    return ViewResult::Close;
                }
                AppCmd::RenamePane {
                    idx: *idx,
                    name: value,
                }
            }
            InputPurpose::SetTextSetting { key, scope } => AppCmd::SetSetting {
                key: key.clone(),
                value: serde_json::Value::String(value),
                scope: *scope,
            },
            InputPurpose::AddProjectPath => {
                if value.is_empty() {
                    return ViewResult::Close;
                }
                AppCmd::OpenProjectAt(value)
            }
            InputPurpose::MergeCommitMessage { slug } => AppCmd::MergeExec {
                slug: slug.clone(),
                message: Some(if value.is_empty() {
                    "dmux: worktree changes".into()
                } else {
                    value
                }),
            },
            InputPurpose::SearchScrollback => {
                if value.is_empty() {
                    return ViewResult::Close;
                }
                AppCmd::SearchScrollback(value)
            }
        };
        ViewResult::CloseAnd(cmd)
    }
}

impl View for InputView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let rect = centered(area, area.w.min(56), 6);
        let inner = draw_panel(buf, rect, &self.title, ctx.theme, PanelStyle::Modal);
        let cursor = self.input.draw(
            buf,
            Rect::new(inner.x, inner.y + 1, inner.w, 1),
            ctx.theme,
            true,
        );
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &[("⏎", "save"), ("esc", "cancel")],
            ctx.theme,
        );
        cursor
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_enter(key) {
            return self.submit();
        }
        if let Some(ik) = vkeys::as_input_key(key) {
            self.input.handle(ik);
        }
        ViewResult::Stay
    }
}
