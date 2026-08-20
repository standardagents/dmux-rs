//! Live application of persisted settings. The Settings view emits typed
//! updates; this module owns storage scope and immediate runtime effects.

use dmux_core::SettingsScope;
use dmux_ui::Theme;

use crate::App;

impl App {
    pub(crate) fn set_setting(
        &mut self,
        key: &str,
        value: serde_json::Value,
        requested_scope: SettingsScope,
    ) {
        let scope = if key == crate::hud::VISIBLE_SETTING {
            SettingsScope::Global
        } else {
            requested_scope
        };
        {
            let mut settings = self.settings.lock().unwrap();
            let unset = value
                .as_str()
                .map(|value| value.is_empty())
                .unwrap_or(false)
                && key == "baseBranch";
            if unset {
                settings.unset(key, scope);
            } else {
                settings.set(key, value, scope);
            }
            if let Err(err) = settings.save(scope) {
                tracing::warn!(%err, key, "settings save failed");
            }
        }
        match key {
            "colorTheme" => {
                let name = {
                    let settings = self.settings.lock().unwrap();
                    settings
                        .get_str("colorTheme")
                        .unwrap_or("violet")
                        .to_string()
                };
                self.theme = Theme::named(&name);
                self.force_full = true;
            }
            "minPaneWidth" | "maxPaneWidth" => self.relayout(),
            "language" => {
                let language = {
                    let settings = self.settings.lock().unwrap();
                    settings.get_str("language").unwrap_or("en").to_string()
                };
                dmux_core::i18n::set_locale(&language);
                self.force_full = true;
            }
            crate::hud::VISIBLE_SETTING => {
                let visible = {
                    let settings = self.settings.lock().unwrap();
                    crate::hud::configured_visible(&settings)
                };
                self.apply_hud_visibility(visible);
            }
            _ => {}
        }
        self.dirty = true;
    }
}
