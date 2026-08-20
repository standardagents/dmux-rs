use dmux_compositor::{CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{draw_hint_bar, draw_panel, ClickMap, PanelStyle, TextInput};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};

/// Click-target base for cursor positioning; the offset is the column.
const TAG_FIELD: u64 = 500;

/// What a submitted input means; keeps views free of closures.
#[derive(Debug, Clone)]
pub enum InputPurpose {
    RenamePane(usize),
    SetTextSetting {
        key: String,
        scope: dmux_core::SettingsScope,
    },
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
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let rect = ctx.overlay(area, area.w.min(56), 6);
        let inner = draw_panel(buf, rect, &self.title, ctx.theme, PanelStyle::Modal);
        let field = Rect::new(inner.x, inner.y + 1, inner.w, 1);
        let cursor = self.input.draw(buf, field, ctx.theme, true);
        // Click-to-position (#96): one target per cell carries the column.
        for col in 0..field.w {
            clicks.add(
                Rect::new(field.x + col, field.y, 1, 1),
                ClickTarget::Overlay(TAG_FIELD + col as u64),
            );
        }
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

    fn on_click(&mut self, tag: u64) -> ViewResult {
        if tag >= TAG_FIELD {
            self.input.click_col((tag - TAG_FIELD) as u16);
        }
        ViewResult::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::{KeyCode, Modifiers};

    fn key(code: KeyCode, mods: Modifiers) -> KeyEvent {
        KeyEvent {
            key: code,
            modifiers: mods,
        }
    }

    #[test]
    fn rename_dialog_renders_at_its_assigned_source_anchor() {
        use dmux_compositor::CellBuffer;
        use dmux_ui::{ClickMap, Theme};
        // A pane far from the sidebar (#105): the stack assigns a pointer
        // anchor and the dialog renders there, not beside the sidebar.
        let mut v = InputView::new("Rename", "name", "", InputPurpose::RenamePane(0));
        let theme = Theme::default();
        let ctx = super::super::ViewCtx {
            theme: &theme,
            anim: 0,
            hovered: None,
            sidebar_right: 40,
            anchor: dmux_ui::Anchor::Pointer { x: 100, y: 8 },
        };
        let mut buf = CellBuffer::new(160, 30);
        let mut clicks = ClickMap::new();
        v.render(
            &mut buf,
            dmux_compositor::Rect::new(0, 0, 160, 30),
            &ctx,
            &mut clicks,
        );
        assert_eq!(buf.get(100, 8).ch, '╭', "panel opens at the pointer");
        // Global anchor keeps the sidebar-top placement.
        let ctx_global = super::super::ViewCtx {
            anchor: dmux_ui::Anchor::SidebarTop,
            ..ctx
        };
        let mut buf2 = CellBuffer::new(160, 30);
        let mut clicks2 = ClickMap::new();
        v.render(
            &mut buf2,
            dmux_compositor::Rect::new(0, 0, 160, 30),
            &ctx_global,
            &mut clicks2,
        );
        assert_eq!(buf2.get(41, 0).ch, '╭', "global stays beside the sidebar");
    }

    #[test]
    fn clicks_position_the_cursor_in_the_field() {
        // Tag offset = clicked column within the drawn field; typing then
        // inserts at the clicked spot.
        let mut v = InputView::new("Rename", "alpha beta", "", InputPurpose::RenamePane(0));
        v.on_click(super::TAG_FIELD + 1); // first interior cell → col 0
        v.on_key(&key(KeyCode::Char('X'), Modifiers::NONE));
        assert_eq!(v.input.value, "Xalpha beta");
    }

    #[test]
    fn rename_pane_receives_shared_word_and_line_navigation() {
        // The Rename Pane field routes every key through the shared
        // translation (#96): Option+Left lands at a word start, so a typed
        // character inserts there; Command+Right returns to the end.
        let mut v = InputView::new("Rename", "alpha beta", "", InputPurpose::RenamePane(0));
        v.on_key(&key(KeyCode::LeftArrow, Modifiers::ALT));
        v.on_key(&key(KeyCode::Char('X'), Modifiers::NONE));
        assert_eq!(v.input.value, "alpha Xbeta");
        v.on_key(&key(KeyCode::RightArrow, Modifiers::SUPER));
        v.on_key(&key(KeyCode::Char('!'), Modifiers::NONE));
        assert_eq!(v.input.value, "alpha Xbeta!");
    }
}
