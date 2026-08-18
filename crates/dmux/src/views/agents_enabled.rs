use std::sync::{Arc, Mutex};

use dmux_compositor::{CellBuffer, Rect};
use dmux_core::{SettingsScope, SettingsStore};
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_checkbox, draw_hint_bar, draw_kv_row, draw_panel, ClickMap, ListState, PanelStyle};

use super::{vkeys, AppCmd, ClickTarget, View, ViewCtx, ViewResult};
use crate::agents::AGENTS;

/// Checklist controlling which agents appear in the allocator
/// (settings `enabledAgents`, TS-compatible: array of agent ids).
pub struct EnabledAgentsView {
    settings: Arc<Mutex<SettingsStore>>,
    list: ListState,
    scope: SettingsScope,
}

impl EnabledAgentsView {
    pub fn new(settings: Arc<Mutex<SettingsStore>>, has_project: bool) -> Self {
        Self {
            settings,
            list: ListState::default(),
            scope: if has_project { SettingsScope::Project } else { SettingsScope::Global },
        }
    }

    fn enabled_set(&self) -> Vec<String> {
        let store = self.settings.lock().unwrap();
        match store.get("enabledAgents").and_then(|v| v.as_array().cloned()) {
            Some(list) => list.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            None => AGENTS.iter().filter(|a| a.default_enabled).map(|a| a.id.to_string()).collect(),
        }
    }

    fn toggle(&mut self, idx: usize) -> ViewResult {
        let Some(def) = AGENTS.get(idx) else { return ViewResult::Stay };
        let mut enabled = self.enabled_set();
        if let Some(pos) = enabled.iter().position(|id| id == def.id) {
            enabled.remove(pos);
        } else {
            enabled.push(def.id.to_string());
        }
        ViewResult::Cmd(AppCmd::SetSetting {
            key: "enabledAgents".into(),
            value: serde_json::Value::Array(enabled.into_iter().map(serde_json::Value::String).collect()),
            scope: self.scope,
        })
    }
}

impl View for EnabledAgentsView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let rect = centered(area, area.w.min(48), (AGENTS.len() as u16 + 4).min(area.h));
        let inner = draw_panel(buf, rect, "Enabled Agents", ctx.theme, PanelStyle::Modal);
        let enabled = self.enabled_set();
        self.list.clamp(AGENTS.len());
        for (row, (i, def)) in AGENTS.iter().enumerate().take(inner.h.saturating_sub(1) as usize).enumerate() {
            let y = inner.y + row as u16;
            let on = enabled.iter().any(|id| id == def.id);
            let label = format!("{} {}  [{}]", draw_checkbox(on), def.name, def.short);
            let row_rect = Rect::new(inner.x, y, inner.w, 1);
            draw_kv_row(buf, row_rect, &label, "", ctx.theme, i == self.list.selected, true);
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
            self.list.step(-1, AGENTS.len());
            return ViewResult::Stay;
        }
        if vkeys::is_down(key) {
            self.list.step(1, AGENTS.len());
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
}
