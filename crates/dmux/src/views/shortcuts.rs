use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_hint_bar, draw_panel, ClickMap, PanelStyle};

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
        // Two columns: leader table left, direct chords right.
        let rows = LEADER_ROWS.len().max(self.direct.len() + 2) as u16 + 4;
        let rect = centered(area, area.w.min(96), rows.min(area.h));
        let inner = draw_panel(
            buf,
            rect,
            "Keyboard Shortcuts",
            ctx.theme,
            PanelStyle::Modal,
        );
        let bg = ctx.theme.bg_raised;
        let col2 = inner.x + inner.w / 2 + 2;

        buf.draw_text(
            inner.x + 1,
            inner.y,
            "Leader",
            ctx.theme.text_dim,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        for (y, (key, desc)) in (inner.y + 1..).zip(LEADER_ROWS) {
            if y >= inner.bottom().saturating_sub(1) {
                break;
            }
            buf.draw_text(
                inner.x + 1,
                y,
                key,
                ctx.theme.accent,
                bg,
                AttrFlags::BOLD,
                inner,
            );
            buf.draw_text(
                inner.x + 12,
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
            inner.y,
            title,
            ctx.theme.text_dim,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        for (y, (key, desc)) in (inner.y + 1..).zip(&self.direct) {
            if y >= inner.bottom().saturating_sub(2) {
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
            inner.bottom().saturating_sub(2),
            "remap: \"keybindings\" in settings.json",
            ctx.theme.text_faint,
            bg,
            AttrFlags::ITALIC,
            inner,
        );

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
