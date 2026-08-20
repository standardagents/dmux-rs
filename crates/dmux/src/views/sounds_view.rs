use std::sync::{Arc, Mutex};

use dmux_compositor::{CellBuffer, Rect};
use dmux_core::{SettingsScope, SettingsStore};
use dmux_host::KeyEvent;
use dmux_ui::{
    draw_checkbox, draw_hint_bar, draw_kv_row, draw_panel, ClickMap, ListState, PanelStyle,
};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};
use crate::sounds::SOUNDS;

/// Checklist for the helper's alert sounds (settings
/// `enabledNotificationSounds`, TS-compatible: array of sound ids).
pub struct SoundsView {
    settings: Arc<Mutex<SettingsStore>>,
    list: ListState,
    scope: SettingsScope,
}

impl SoundsView {
    pub fn new(settings: Arc<Mutex<SettingsStore>>, has_project: bool) -> Self {
        Self {
            settings,
            list: ListState::default(),
            scope: if has_project {
                SettingsScope::Project
            } else {
                SettingsScope::Global
            },
        }
    }

    fn enabled_ids(&self) -> Vec<String> {
        let store = self.settings.lock().unwrap();
        crate::sounds::resolve_selection(store.get("enabledNotificationSounds"))
            .iter()
            .map(|s| s.id.to_string())
            .collect()
    }

    fn toggle(&mut self, idx: usize) -> ViewResult {
        let Some(def) = SOUNDS.get(idx) else {
            return ViewResult::Stay;
        };
        let mut enabled = self.enabled_ids();
        if let Some(pos) = enabled.iter().position(|id| id == def.id) {
            enabled.remove(pos);
        } else {
            enabled.push(def.id.to_string());
        }
        ViewResult::Cmd(AppCmd::SetSetting {
            key: "enabledNotificationSounds".into(),
            value: serde_json::Value::Array(
                enabled.into_iter().map(serde_json::Value::String).collect(),
            ),
            scope: self.scope,
        })
    }
}

impl View for SoundsView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let rect = ctx.global(area, area.w.min(52), (SOUNDS.len() as u16 + 4).min(area.h));
        let inner = draw_panel(
            buf,
            rect,
            "Notification Sounds",
            ctx.theme,
            PanelStyle::Modal,
        );
        let enabled = self.enabled_ids();
        self.list.clamp(SOUNDS.len());
        for (row, (i, def)) in SOUNDS
            .iter()
            .enumerate()
            .take(inner.h.saturating_sub(1) as usize)
            .enumerate()
        {
            let y = inner.y + row as u16;
            let on = enabled.iter().any(|id| id == def.id);
            let label = format!("{} {}", draw_checkbox(on), def.label);
            let value = if def.resource.is_some() { "" } else { "system" };
            let row_rect = Rect::new(inner.x, y, inner.w, 1);
            let active = ctx.active_overlay(i as u64, i == self.list.selected);
            draw_kv_row(buf, row_rect, &label, value, ctx.theme, active, true);
            clicks.add(row_rect, ClickTarget::Overlay(i as u64));
        }
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &[("␣/⏎", "toggle"), ("esc", "close")],
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_up(key) {
            self.list.step(-1, SOUNDS.len());
            return ViewResult::Stay;
        }
        if vkeys::is_down(key) {
            self.list.step(1, SOUNDS.len());
            return ViewResult::Stay;
        }
        if vkeys::is_enter(key) || vkeys::is_space(key) {
            return self.toggle(self.list.selected);
        }
        ViewResult::Stay
    }

    fn on_click(&mut self, tag: u64) -> ViewResult {
        self.list.selected = tag as usize;
        self.toggle(tag as usize)
    }

    fn on_hover(&mut self, tag: u64) -> u64 {
        if (tag as usize) < SOUNDS.len() {
            self.list.selected = tag as usize;
        }
        tag
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::{KeyCode, Modifiers};

    #[test]
    fn hover_moves_selection_before_keyboard_navigation() {
        let settings = Arc::new(Mutex::new(SettingsStore::load(
            std::path::Path::new("/definitely/missing"),
            None,
        )));
        let mut view = SoundsView::new(settings, false);
        assert_eq!(view.on_hover(1), 1);
        assert_eq!(view.list.selected, 1);
        let key = KeyEvent {
            key: KeyCode::DownArrow,
            modifiers: Modifiers::NONE,
        };
        view.on_key(&key);
        assert_eq!(view.list.selected, 2);
    }
}
