use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_hint_bar, draw_panel, ClickMap, PanelStyle};

use super::{vkeys, ClickTarget, View, ViewCtx, ViewResult};

/// Static cheat sheet; also doubles as the leader-key reference.
pub struct ShortcutsView {
    kitty: bool,
}

impl ShortcutsView {
    pub fn new(kitty: bool) -> Self {
        Self { kitty }
    }
}

const ROWS: &[(&str, &str)] = &[
    ("^b n", "new agents (allocate panes)"),
    ("^b t", "new terminal"),
    ("^b s", "settings"),
    ("^b m / ⏎", "pane menu"),
    ("^b r", "rename pane"),
    ("^b h", "hide / show pane"),
    ("^b x", "close pane"),
    ("^b 1..9", "focus pane N"),
    ("^b ← →", "cycle focus"),
    ("^b ?", "this help"),
    ("^b d", "detach (quit dmux, keep session)"),
    ("^b ^b", "send a literal Ctrl+b to the pane"),
    ("^y", "perf HUD"),
    ("⌥← ⌥→ / ⌥1..9", "focus (no leader)"),
    ("⌥PgUp ⌥PgDn", "scrollback"),
    ("wheel", "scroll pane · click title buttons ✎ – ✕"),
];

const KITTY_ROWS: &[(&str, &str)] = &[
    ("⌘n ⌘t ⌘,", "new agents / terminal / settings"),
    ("⌘1..9 ⌘[ ⌘]", "focus panes"),
    ("⌘r ⌘h ⌘w", "rename / hide / close"),
];

impl View for ShortcutsView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let extra = if self.kitty { KITTY_ROWS.len() + 1 } else { 0 };
        let h = (ROWS.len() + extra + 4) as u16;
        let rect = centered(area, area.w.min(58), h.min(area.h));
        let inner = draw_panel(buf, rect, "Keyboard Shortcuts", ctx.theme, PanelStyle::Modal);
        let bg = ctx.theme.bg_raised;
        let mut y = inner.y;
        for (key, desc) in ROWS {
            if y >= inner.bottom().saturating_sub(1) {
                break;
            }
            buf.draw_text(inner.x + 1, y, key, ctx.theme.accent, bg, AttrFlags::BOLD, inner);
            buf.draw_text(inner.x + 16, y, desc, ctx.theme.text_dim, bg, AttrFlags::empty(), inner);
            y += 1;
        }
        if self.kitty {
            y += 1;
            for (key, desc) in KITTY_ROWS {
                if y >= inner.bottom().saturating_sub(1) {
                    break;
                }
                buf.draw_text(inner.x + 1, y, key, ctx.theme.ok, bg, AttrFlags::BOLD, inner);
                buf.draw_text(inner.x + 16, y, desc, ctx.theme.text_dim, bg, AttrFlags::empty(), inner);
                y += 1;
            }
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
