use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{
    centered, draw_hint_bar, draw_panel, frame_height, panel_frame, ClickMap, PanelStyle,
};

use super::{vkeys, ClickTarget, View, ViewCtx, ViewResult};

/// Cheat sheet: the leader table (fixed) plus the live direct-chord keymap
/// (user-configurable via the `keybindings` object in settings.json).
pub struct ShortcutsView {
    kitty: bool,
    direct: Vec<(String, &'static str)>,
}

impl ShortcutsView {
    pub fn new(kitty: bool, direct: Vec<(String, &'static str)>) -> Self {
        Self { kitty, direct }
    }
}

const LEADER_ROWS: &[(&str, &str)] = &[
    ("^b n", "new agents (allocate panes)"),
    ("^b t", "new terminal"),
    ("^b p", "add project (open a path)"),
    ("^b s", "settings"),
    ("^b m / ⏎", "pane menu"),
    ("^b r", "rename pane"),
    ("^b h", "hide / show pane"),
    ("^b x", "close pane"),
    ("^b 1..9", "focus pane N"),
    ("^b ← →", "cycle focus"),
    ("^b l", "logs"),
    ("^b /", "search scrollback"),
    ("^b ?", "this help"),
    ("^b d", "detach (quit dmux, keep session)"),
    ("^b ^b", "send a literal Ctrl+b to the pane"),
];

impl View for ShortcutsView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        // Two columns: leader table left, direct chords right. Body: column
        // headers, the table, one blank row, then the remap note.
        let table = LEADER_ROWS.len().max(self.direct.len()) as u16;
        let rect = centered(area, area.w.min(96), frame_height(table + 3).min(area.h));
        let inner = draw_panel(
            buf,
            rect,
            "Keyboard Shortcuts",
            ctx.theme,
            PanelStyle::Modal,
        );
        let frame = panel_frame(inner);
        let content = frame.content;
        let bg = ctx.theme.bg_panel;
        let col2 = content.x + content.w / 2 + 2;

        buf.draw_text(
            content.x + 1,
            content.y,
            "Leader",
            ctx.theme.text_dim,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        for (y, (key, desc)) in (content.y + 1..).zip(LEADER_ROWS) {
            if y >= content.bottom().saturating_sub(2) {
                break;
            }
            buf.draw_text(
                content.x + 1,
                y,
                key,
                ctx.theme.accent,
                bg,
                AttrFlags::BOLD,
                inner,
            );
            buf.draw_text(
                content.x + 12,
                y,
                desc,
                ctx.theme.text_dim,
                bg,
                AttrFlags::empty(),
                inner,
            );
        }

        let title = if self.kitty {
            "Direct (kitty host: ⌘ works)"
        } else {
            "Direct"
        };
        buf.draw_text(
            col2,
            content.y,
            title,
            ctx.theme.text_dim,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        for (y, (key, desc)) in (content.y + 1..).zip(&self.direct) {
            if y >= content.bottom().saturating_sub(2) {
                break;
            }
            buf.draw_text(col2, y, key, ctx.theme.ok, bg, AttrFlags::BOLD, inner);
            buf.draw_text(
                col2 + 10,
                y,
                desc,
                ctx.theme.text_dim,
                bg,
                AttrFlags::empty(),
                inner,
            );
        }
        buf.draw_text(
            col2,
            content.bottom().saturating_sub(1),
            "remap: \"keybindings\" in settings.json",
            ctx.theme.text_faint,
            bg,
            AttrFlags::ITALIC,
            inner,
        );

        draw_hint_bar(buf, frame.footer, &[("esc", "close")], ctx.theme);
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

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_ui::Theme;

    #[test]
    fn overlay_separates_the_remap_note_from_the_hint_bar() {
        let mut view = ShortcutsView::new(false, vec![("⌘k".into(), "pane menu")]);
        let mut buf = CellBuffer::new(100, 30);
        let area = buf.area();
        let theme = Theme::default();
        let ctx = ViewCtx {
            theme: &theme,
            anim: 0,
            hovered: None,
        };
        let mut clicks = ClickMap::new();
        view.render(&mut buf, area, &ctx, &mut clicks);

        let row_text = |y: u16| -> String { (0..100).map(|x| buf.get(x, y).ch).collect() };
        let remap_y = (0..30)
            .find(|y| row_text(*y).contains("remap"))
            .expect("remap note rendered");
        let hint_y = (0..30)
            .find(|y| row_text(*y).contains("esc close"))
            .expect("hint bar rendered");
        // #42 spacing contract: remap note, one blank row, then the hint bar.
        assert_eq!(hint_y, remap_y + 2);
        let gap = row_text(remap_y + 1);
        assert!(
            gap.chars().skip(3).take(94).all(|c| c == ' '),
            "gap row: {gap:?}"
        );
        // The last leader row is separated from the remap note by one blank
        // row as well (the table ends above the note).
        let last_leader = (0..30)
            .rev()
            .find(|y| row_text(*y).contains("literal Ctrl+b"))
            .expect("leader table rendered");
        assert!(last_leader + 2 <= remap_y);
    }
}
