use std::sync::{Arc, Mutex};

use dmux_compositor::{CellBuffer, Rect};
use dmux_core::{SettingsScope, SettingsStore};
use dmux_host::{KeyEvent, Modifiers};
use dmux_ui::{
    draw_hint_bar, draw_kv_row, draw_panel, draw_select_value, ClickMap, ListState, PanelStyle,
};
use serde_json::Value;

use super::{vkeys, AppCmd, ClickTarget, InputPurpose, InputView, View, ViewCtx, ViewResult};
use crate::settings::SettingKey;

enum Kind {
    Bool,
    Select(Vec<(String, String)>),
    Number {
        min: i64,
        max: i64,
    },
    Text,
    /// Opens a dedicated sub-view (checklists, status pages).
    Sub(SubView),
}

#[derive(Clone, Copy)]
enum SubView {
    EnabledAgents,
    NotificationSounds,
    InferenceProviders,
    Hooks,
}

struct Def {
    key: Option<SettingKey>,
    label: &'static str,
    kind: Kind,
}

impl Def {
    fn value<'a>(&self, store: &'a SettingsStore) -> Option<&'a Value> {
        let key = self.key?;
        if key.is_global() {
            store.get_global(key.as_str())
        } else {
            store.get(key.as_str())
        }
    }

    fn write_scope(&self, selected: SettingsScope) -> SettingsScope {
        self.key
            .expect("editable setting must have a key")
            .write_scope(selected)
    }

    fn value_scope(&self, store: &SettingsStore) -> Option<SettingsScope> {
        let key = self.key?;
        if key.is_global() {
            Some(SettingsScope::Global)
        } else {
            store.effective_scope(key.as_str())
        }
    }
}

fn definitions() -> Vec<Def> {
    let themes = [
        "violet", "cyan", "green", "amber", "rose", "blue", "slate", "ember",
    ]
    .iter()
    .map(|t| {
        (t.to_string(), {
            let mut s = t.to_string();
            s[..1].make_ascii_uppercase();
            s
        })
    })
    .collect();
    let mut agent_options = vec![(String::new(), "Ask each time".to_string())];
    agent_options.extend(
        crate::agents::AGENTS
            .iter()
            .map(|a| (a.id.to_string(), a.name.to_string())),
    );

    vec![
        Def {
            key: Some(SettingKey::PermissionMode),
            label: "Agent Permission Mode",
            kind: Kind::Select(vec![
                (String::new(), "Agent default (ask)".into()),
                ("plan".into(), "Plan mode (Claude only)".into()),
                ("acceptEdits".into(), "Accept edits".into()),
                ("bypassPermissions".into(), "Bypass permissions".into()),
            ]),
        },
        Def {
            key: Some(SettingKey::DefaultAgent),
            label: "Default Agent",
            kind: Kind::Select(agent_options),
        },
        Def {
            key: Some(SettingKey::GoalModeByDefault),
            label: "Goal Mode by Default",
            kind: Kind::Bool,
        },
        Def {
            key: Some(SettingKey::Notifications),
            label: "Notifications",
            kind: Kind::Bool,
        },
        Def {
            key: Some(SettingKey::FooterTips),
            label: "Footer Tips",
            kind: Kind::Bool,
        },
        Def {
            key: Some(SettingKey::PerformanceProfiler),
            label: "Performance Profiler",
            kind: Kind::Bool,
        },
        Def {
            key: Some(SettingKey::ColorTheme),
            label: "Color Theme",
            kind: Kind::Select(themes),
        },
        Def {
            key: Some(SettingKey::BaseBranch),
            label: "Base Branch",
            kind: Kind::Text,
        },
        Def {
            key: Some(SettingKey::BranchPrefix),
            label: "Branch Name Prefix",
            kind: Kind::Select(vec![
                (String::new(), "No prefix".into()),
                ("feat/".into(), "feat/".into()),
                ("fix/".into(), "fix/".into()),
                ("chore/".into(), "chore/".into()),
            ]),
        },
        Def {
            key: Some(SettingKey::PromptForGitOptionsOnCreate),
            label: "Ask Git Options on Create",
            kind: Kind::Bool,
        },
        Def {
            key: Some(SettingKey::MinPaneWidth),
            label: "Min Pane Width",
            kind: Kind::Number { min: 40, max: 120 },
        },
        Def {
            key: Some(SettingKey::MaxPaneWidth),
            label: "Max Pane Width",
            kind: Kind::Number { min: 60, max: 240 },
        },
        Def {
            key: Some(SettingKey::Language),
            label: "Language",
            kind: Kind::Select(vec![
                ("en".into(), "English".into()),
                ("ja".into(), "日本語".into()),
            ]),
        },
        Def {
            key: Some(SettingKey::EnabledAgents),
            label: "Enabled Agents…",
            kind: Kind::Sub(SubView::EnabledAgents),
        },
        Def {
            key: Some(SettingKey::EnabledNotificationSounds),
            label: "Notification Sounds…",
            kind: Kind::Sub(SubView::NotificationSounds),
        },
        Def {
            key: None,
            label: "Inference Providers…",
            kind: Kind::Sub(SubView::InferenceProviders),
        },
        Def {
            key: None,
            label: "Project Hooks…",
            kind: Kind::Sub(SubView::Hooks),
        },
    ]
}

/// The settings menu: the TS declarative registry rendered through the shared
/// component system, editing the same JSON files with scope awareness.
pub struct SettingsView {
    settings: Arc<Mutex<SettingsStore>>,
    defs: Vec<Def>,
    list: ListState,
    scope: SettingsScope,
    has_project: bool,
    project_root: std::path::PathBuf,
}

impl SettingsView {
    pub fn new(
        settings: Arc<Mutex<SettingsStore>>,
        has_project: bool,
        project_root: std::path::PathBuf,
    ) -> Self {
        Self {
            settings,
            defs: definitions(),
            list: ListState::default(),
            scope: if has_project {
                SettingsScope::Project
            } else {
                SettingsScope::Global
            },
            has_project,
            project_root,
        }
    }

    fn current_value(&self, def: &Def) -> Value {
        let store = self.settings.lock().unwrap();
        def.value(&store).cloned().unwrap_or(Value::Null)
    }

    fn set(&self, def: &Def, value: Value) -> ViewResult {
        ViewResult::Cmd(AppCmd::SetSetting {
            key: def.key.expect("editable setting must have a key"),
            value,
            scope: def.write_scope(self.scope),
        })
    }

    /// Cycle a select/bool/number by `dir`, or open the text editor.
    fn adjust(&mut self, dir: i64, big: bool) -> ViewResult {
        let def = &self.defs[self.list.selected];
        match &def.kind {
            Kind::Bool => {
                let cur = self.current_value(def).as_bool().unwrap_or(false);
                self.set(def, Value::Bool(!cur))
            }
            Kind::Select(options) => {
                let cur = self.current_value(def);
                let cur = cur.as_str().unwrap_or("");
                let idx = options.iter().position(|(v, _)| v == cur).unwrap_or(0) as i64;
                let next = (idx + dir).rem_euclid(options.len() as i64) as usize;
                self.set(def, Value::String(options[next].0.clone()))
            }
            Kind::Number { min, max } => {
                let cur = self
                    .current_value(def)
                    .as_i64()
                    .unwrap_or((*min + *max) / 2);
                let step = if big { 5 } else { 1 };
                let next = (cur + dir * step).clamp(*min, *max);
                self.set(def, Value::from(next))
            }
            Kind::Text => {
                let cur = self.current_value(def);
                ViewResult::Push(Box::new(InputView::new(
                    def.label,
                    cur.as_str().unwrap_or(""),
                    "empty to unset",
                    InputPurpose::SetTextSetting {
                        key: def.key.expect("text setting must have a key"),
                        scope: self.scope,
                    },
                )))
            }
            Kind::Sub(subview) => match subview {
                SubView::EnabledAgents => ViewResult::Push(Box::new(
                    super::EnabledAgentsView::new(self.settings.clone(), self.has_project),
                )),
                SubView::NotificationSounds => ViewResult::Push(Box::new(super::SoundsView::new(
                    self.settings.clone(),
                    self.has_project,
                ))),
                SubView::InferenceProviders => ViewResult::Push(Box::new(
                    super::InferProvidersView::new(self.settings.clone()),
                )),
                SubView::Hooks => {
                    ViewResult::Push(Box::new(super::HooksView::new(self.project_root.clone())))
                }
            },
        }
    }
}

impl View for SettingsView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let h = (self.defs.len() as u16 + 6).min(area.h.saturating_sub(2));
        let rect = ctx.overlay(area, area.w.min(64), h);
        let scope_label = match self.scope {
            SettingsScope::Project => "project scope",
            SettingsScope::Global => "global scope",
        };
        let inner = draw_panel(
            buf,
            rect,
            &format!("Settings — {scope_label}"),
            ctx.theme,
            PanelStyle::Modal,
        );

        let visible = inner.h.saturating_sub(2) as usize;
        self.list.clamp(self.defs.len());
        self.list.ensure_visible(visible);

        let (values, scopes): (Vec<Value>, Vec<Option<SettingsScope>>) = {
            let store = self.settings.lock().unwrap();
            self.defs
                .iter()
                .map(|d| {
                    (
                        d.value(&store).cloned().unwrap_or(Value::Null),
                        d.value_scope(&store),
                    )
                })
                .unzip()
        };

        for (row, (i, def)) in self
            .defs
            .iter()
            .enumerate()
            .skip(self.list.scroll)
            .take(visible)
            .enumerate()
        {
            let y = inner.y + row as u16;
            let selected = ctx.active_overlay(i as u64, i == self.list.selected);
            let value_text = match &def.kind {
                Kind::Bool => {
                    let on = values[i].as_bool().unwrap_or(false);
                    if on {
                        "◼ on".to_string()
                    } else {
                        "◻ off".to_string()
                    }
                }
                Kind::Select(options) => {
                    let cur = values[i].as_str().unwrap_or("");
                    let label = options
                        .iter()
                        .find(|(v, _)| v == cur)
                        .map(|(_, l)| l.as_str())
                        .unwrap_or(options.first().map(|(_, l)| l.as_str()).unwrap_or(""));
                    draw_select_value(label)
                }
                Kind::Number { min, max } => {
                    let cur = values[i].as_i64().unwrap_or((*min + *max) / 2);
                    draw_select_value(&cur.to_string())
                }
                Kind::Text => {
                    let cur = values[i].as_str().unwrap_or("");
                    if cur.is_empty() {
                        "(unset) ✎".to_string()
                    } else {
                        format!("{cur} ✎")
                    }
                }
                Kind::Sub(_) => "›".to_string(),
            };
            let scope_mark = match scopes[i] {
                Some(SettingsScope::Project) => "ᵖ ",
                Some(SettingsScope::Global) => "ᵍ ",
                None => "  ",
            };
            let label = format!("{scope_mark}{}", def.label);
            let row_rect = Rect::new(inner.x, y, inner.w, 1);
            draw_kv_row(
                buf,
                row_rect,
                &label,
                &value_text,
                ctx.theme,
                selected,
                true,
            );
            clicks.add(row_rect, ClickTarget::Overlay(i as u64));
        }

        let mut hints: Vec<(&str, &str)> =
            vec![("↑↓", "row"), ("←→/⏎", "change"), ("esc", "close")];
        if self.has_project {
            hints.insert(2, ("tab", "scope"));
        }
        draw_hint_bar(
            buf,
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1),
            &hints,
            ctx.theme,
        );
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            return ViewResult::Close;
        }
        if vkeys::is_tab(key) && self.has_project {
            self.scope = match self.scope {
                SettingsScope::Project => SettingsScope::Global,
                SettingsScope::Global => SettingsScope::Project,
            };
            return ViewResult::Stay;
        }
        if vkeys::is_up(key) {
            self.list.step(-1, self.defs.len());
            return ViewResult::Stay;
        }
        if vkeys::is_down(key) {
            self.list.step(1, self.defs.len());
            return ViewResult::Stay;
        }
        let big = key.modifiers.contains(Modifiers::SHIFT);
        if vkeys::is_left(key) {
            return self.adjust(-1, big);
        }
        if vkeys::is_right(key) || vkeys::is_enter(key) || vkeys::is_space(key) {
            return self.adjust(1, big);
        }
        ViewResult::Stay
    }

    fn on_click(&mut self, tag: u64) -> ViewResult {
        if (tag as usize) < self.defs.len() {
            self.list.selected = tag as usize;
            return self.adjust(1, false);
        }
        ViewResult::Stay
    }

    fn on_hover(&mut self, tag: u64) -> u64 {
        if (tag as usize) < self.defs.len() {
            self.list.selected = tag as usize;
        }
        tag
    }

    fn on_wheel(&mut self, delta: i32) -> ViewResult {
        self.list.step(delta, self.defs.len());
        ViewResult::Stay
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
        let mut view = SettingsView::new(settings, false, std::path::PathBuf::from("."));
        assert_eq!(view.on_hover(1), 1);
        assert_eq!(view.list.selected, 1);
        let key = KeyEvent {
            key: KeyCode::DownArrow,
            modifiers: Modifiers::NONE,
        };
        view.on_key(&key);
        assert_eq!(view.list.selected, 2);
    }

    #[test]
    fn profiler_visibility_is_a_global_setting_in_project_scope() {
        let settings = Arc::new(Mutex::new(SettingsStore::load(
            std::path::Path::new("/definitely/missing"),
            Some(std::path::Path::new("/definitely/missing/project")),
        )));
        let mut view = SettingsView::new(settings, true, std::path::PathBuf::from("."));
        view.scope = SettingsScope::Project;
        view.list.selected = view
            .defs
            .iter()
            .position(|def| def.key == Some(SettingKey::PerformanceProfiler))
            .unwrap();

        let ViewResult::Cmd(AppCmd::SetSetting { key, value, scope }) = view.adjust(1, false)
        else {
            panic!("profiler row should emit a setting command");
        };
        assert_eq!(key, SettingKey::PerformanceProfiler);
        assert_eq!(value, Value::Bool(true));
        assert_eq!(scope, SettingsScope::Global);
    }

    #[test]
    fn settings_panel_renders_the_global_profiler_toggle() {
        let settings = Arc::new(Mutex::new(SettingsStore::load(
            std::path::Path::new("/definitely/missing"),
            Some(std::path::Path::new("/definitely/missing/project")),
        )));
        let mut view = SettingsView::new(settings, true, std::path::PathBuf::from("."));
        let theme = dmux_ui::Theme::named("violet");
        let ctx = ViewCtx {
            theme: &theme,
            anim: 0,
            hovered: None,
            sidebar_right: 40,
            anchor: dmux_ui::Anchor::SidebarTop,
        };
        let mut buf = CellBuffer::new(110, 30);
        let mut clicks = ClickMap::new();

        let area = buf.area();
        view.render(&mut buf, area, &ctx, &mut clicks);
        let mut rendered = String::new();
        for row in 0..buf.rows() {
            for col in 0..buf.cols() {
                rendered.push(buf.get(col, row).ch);
            }
        }
        assert!(rendered.contains("ᵍ Performance Profiler"));
        assert!(rendered.contains("◻ off"));
    }
}
