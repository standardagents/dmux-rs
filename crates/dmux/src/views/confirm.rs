use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::{KeyCode, KeyEvent};
use dmux_ui::{draw_button, draw_panel, ButtonStyle, ClickMap, PanelStyle};

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
    pub fn new(
        title: impl Into<String>,
        message: impl Into<String>,
        yes_label: impl Into<String>,
        danger: bool,
        cmd: AppCmd,
    ) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            yes_label: yes_label.into(),
            danger,
            cmd,
            yes_focused: !danger,
        }
    }

    /// Start with the confirm button focused even for a danger action —
    /// for flows the user just explicitly requested (pane close, #11):
    /// the dialog is the confirmation, so Enter should be the fast path.
    /// Esc / `n` / Tab-to-Cancel all keep working.
    pub fn focus_confirm(mut self) -> Self {
        self.yes_focused = true;
        self
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
        let rect = ctx.overlay(area, w, 7);
        let inner = draw_panel(buf, rect, &self.title, ctx.theme, PanelStyle::Modal);
        buf.draw_text(
            inner.x + 1,
            inner.y + 1,
            &self.message,
            ctx.theme.text,
            ctx.theme.bg_panel,
            AttrFlags::empty(),
            inner,
        );

        let y = inner.bottom().saturating_sub(1);
        let style = if self.danger {
            ButtonStyle::Danger
        } else {
            ButtonStyle::Primary
        };
        let yes = draw_button(
            buf,
            inner.x + 2,
            y,
            &self.yes_label,
            ctx.theme,
            style,
            ctx.active_overlay(TAG_YES, self.yes_focused),
            inner,
        );
        clicks.add(yes, ClickTarget::Overlay(TAG_YES));
        let no = draw_button(
            buf,
            yes.right() + 3,
            y,
            "Cancel",
            ctx.theme,
            ButtonStyle::Quiet,
            ctx.active_overlay(TAG_NO, !self.yes_focused),
            inner,
        );
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

    fn on_hover(&mut self, tag: u64) -> u64 {
        match tag {
            TAG_YES => self.yes_focused = true,
            TAG_NO => self.yes_focused = false,
            _ => {}
        }
        tag
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            key: code,
            modifiers: Modifiers::NONE,
        }
    }

    fn close_dialog() -> ConfirmView {
        ConfirmView::new("Close pane", "Close it?", "Close", true, AppCmd::Quit).focus_confirm()
    }

    #[test]
    fn close_dialog_confirms_on_enter() {
        // #11: the user already asked to close — Enter is the fast path.
        let mut v = close_dialog();
        assert!(matches!(
            v.on_key(&key(KeyCode::Enter)),
            ViewResult::CloseAnd(_)
        ));
    }

    #[test]
    fn cancel_paths_stay_intact() {
        // Esc and `n` cancel regardless of focus.
        let mut v = close_dialog();
        assert!(matches!(v.on_key(&key(KeyCode::Escape)), ViewResult::Close));
        let mut v = close_dialog();
        assert!(matches!(
            v.on_key(&key(KeyCode::Char('n'))),
            ViewResult::Close
        ));
        // Tab moves focus to Cancel; Enter then cancels.
        let mut v = close_dialog();
        assert!(matches!(v.on_key(&key(KeyCode::Tab)), ViewResult::Stay));
        assert!(matches!(v.on_key(&key(KeyCode::Enter)), ViewResult::Close));
    }

    #[test]
    fn hover_moves_focus_before_the_next_keyboard_action() {
        let mut v = close_dialog();
        assert_eq!(v.on_hover(TAG_NO), TAG_NO);
        assert!(matches!(v.on_key(&key(KeyCode::Enter)), ViewResult::Close));
    }

    #[test]
    fn plain_danger_dialogs_still_default_to_cancel() {
        let mut v = ConfirmView::new("t", "m", "Do it", true, AppCmd::Quit);
        assert!(matches!(v.on_key(&key(KeyCode::Enter)), ViewResult::Close));
    }

    #[test]
    fn hovered_button_owns_the_visible_active_treatment() {
        let mut view = close_dialog();
        view.on_hover(TAG_NO);
        let theme = dmux_ui::Theme::named("violet");
        let ctx = ViewCtx {
            theme: &theme,
            anim: 0,
            hovered: None,
            sidebar_right: 0,
            anchor: dmux_ui::Anchor::SidebarTop,
        };
        let mut buf = CellBuffer::new(60, 12);
        let area = buf.area();
        let mut clicks = ClickMap::new();
        view.render(&mut buf, area, &ctx, &mut clicks);
        let point = |target| {
            (0..area.h)
                .flat_map(|row| (0..area.w).map(move |col| (col, row)))
                .find(|(col, row)| clicks.hit(*col, *row) == Some(&target))
                .unwrap()
        };
        let yes = point(ClickTarget::Overlay(TAG_YES));
        let no = point(ClickTarget::Overlay(TAG_NO));
        assert!(!buf.get(yes.0, yes.1).attrs.contains(AttrFlags::BOLD));
        assert!(buf.get(no.0, no.1).attrs.contains(AttrFlags::BOLD));
    }
}
