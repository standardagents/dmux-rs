use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::{KeyCode, KeyEvent};
use dmux_ui::{centered, draw_button, draw_panel, ButtonStyle, ClickMap, PanelStyle};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};

const TAG_YES: u64 = 1;
const TAG_NO: u64 = 2;

/// Yes/no dialog. `y`/Enter confirms, `n`/Esc cancels; buttons clickable.
pub struct ConfirmView {
    title: String,
    message: String,
    yes_label: String,
    danger: bool,
    cmd: AppCmd,
    yes_focused: bool,
}

impl ConfirmView {
    pub fn new(title: impl Into<String>, message: impl Into<String>, yes_label: impl Into<String>, danger: bool, cmd: AppCmd) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            yes_label: yes_label.into(),
            danger,
            cmd,
            yes_focused: !danger,
        }
    }
}

impl View for ConfirmView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let w = (self.message.chars().count() as u16 + 6).clamp(34, area.w.min(70));
        let rect = centered(area, w, 7);
        let inner = draw_panel(buf, rect, &self.title, ctx.theme, PanelStyle::Modal);
        buf.draw_text(inner.x + 1, inner.y + 1, &self.message, ctx.theme.text, ctx.theme.bg_raised, AttrFlags::empty(), inner);

        let y = inner.bottom().saturating_sub(1);
        let style = if self.danger { ButtonStyle::Danger } else { ButtonStyle::Primary };
        let yes = draw_button(buf, inner.x + 2, y, &self.yes_label, ctx.theme, style, self.yes_focused, inner);
        clicks.add(yes, ClickTarget::Overlay(TAG_YES));
        let no = draw_button(buf, yes.right() + 3, y, "Cancel", ctx.theme, ButtonStyle::Quiet, !self.yes_focused, inner);
        clicks.add(no, ClickTarget::Overlay(TAG_NO));
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) || matches!(key.key, KeyCode::Char('n')) {
            return ViewResult::Close;
        }
        if matches!(key.key, KeyCode::Char('y')) {
            return ViewResult::CloseAnd(self.cmd.clone());
        }
        if vkeys::is_tab(key) || vkeys::is_left(key) || vkeys::is_right(key) {
            self.yes_focused = !self.yes_focused;
            return ViewResult::Stay;
        }
        if vkeys::is_enter(key) {
            return if self.yes_focused {
                ViewResult::CloseAnd(self.cmd.clone())
            } else {
                ViewResult::Close
            };
        }
        ViewResult::Stay
    }

    fn on_click(&mut self, tag: u64) -> ViewResult {
        match tag {
            TAG_YES => ViewResult::CloseAnd(self.cmd.clone()),
            _ => ViewResult::Close,
        }
    }
}
