//! Live application of persisted settings. The Settings view emits typed
//! updates; this module owns storage scope and immediate runtime effects.

use dmux_core::SettingsScope;
use dmux_ui::Theme;

use crate::App;

/// Stable identifiers for persisted settings used by application views.
///
/// `as_str` is the compatibility boundary with the existing JSON files. Keep
/// those serialized names stable when Rust symbols or UI labels change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingKey {
    PermissionMode,
    DefaultAgent,
    GoalModeByDefault,
    Notifications,
    FooterTips,
    PerformanceProfiler,
    ColorTheme,
    BaseBranch,
    BranchPrefix,
    PromptForGitOptionsOnCreate,
    MinPaneWidth,
    MaxPaneWidth,
    Language,
    EnabledAgents,
    EnabledNotificationSounds,
}

impl SettingKey {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PermissionMode => "permissionMode",
            Self::DefaultAgent => "defaultAgent",
            Self::GoalModeByDefault => "enableGoalModeByDefault",
            Self::Notifications => "enableNotifications",
            Self::FooterTips => "showFooterTips",
            Self::PerformanceProfiler => "showPerformanceProfiler",
            Self::ColorTheme => "colorTheme",
            Self::BaseBranch => "baseBranch",
            Self::BranchPrefix => "branchPrefix",
            Self::PromptForGitOptionsOnCreate => "promptForGitOptionsOnCreate",
            Self::MinPaneWidth => "minPaneWidth",
            Self::MaxPaneWidth => "maxPaneWidth",
            Self::Language => "language",
            Self::EnabledAgents => "enabledAgents",
            Self::EnabledNotificationSounds => "enabledNotificationSounds",
        }
    }

    pub(crate) const fn write_scope(self, requested: SettingsScope) -> SettingsScope {
        match self {
            Self::PerformanceProfiler => SettingsScope::Global,
            _ => requested,
        }
    }

    pub(crate) const fn is_global(self) -> bool {
        matches!(self, Self::PerformanceProfiler)
    }
}

impl App {
    pub(crate) fn set_setting(
        &mut self,
        key: SettingKey,
        value: serde_json::Value,
        requested_scope: SettingsScope,
    ) {
        let Some(_owner_guard) = self.renderer.confirmed_guard() else {
            return;
        };
        let scope = key.write_scope(requested_scope);
        let serialized_key = key.as_str();
        {
            let mut settings = self.settings.lock().unwrap();
            let unset = value
                .as_str()
                .map(|value| value.is_empty())
                .unwrap_or(false)
                && key == SettingKey::BaseBranch;
            if unset {
                settings.unset(serialized_key, scope);
            } else {
                settings.set(serialized_key, value, scope);
            }
            if let Err(err) = settings.save(scope) {
                tracing::warn!(%err, key = serialized_key, "settings save failed");
            }
        }
        match key {
            SettingKey::ColorTheme => {
                let name = {
                    let settings = self.settings.lock().unwrap();
                    settings
                        .get_str(key.as_str())
                        .unwrap_or("violet")
                        .to_string()
                };
                self.theme = Theme::named(&name);
                self.force_full = true;
            }
            SettingKey::MinPaneWidth | SettingKey::MaxPaneWidth => self.relayout(),
            SettingKey::Language => {
                let language = {
                    let settings = self.settings.lock().unwrap();
                    settings.get_str(key.as_str()).unwrap_or("en").to_string()
                };
                dmux_core::i18n::set_locale(&language);
                self.force_full = true;
            }
            SettingKey::PerformanceProfiler => {
                let visible = {
                    let settings = self.settings.lock().unwrap();
                    crate::profiler::configured_visible(&settings)
                };
                self.apply_profiler_visibility(visible);
            }
            _ => {}
        }
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_keys_preserve_existing_configuration_names() {
        let expected = [
            (SettingKey::PermissionMode, "permissionMode"),
            (SettingKey::DefaultAgent, "defaultAgent"),
            (SettingKey::GoalModeByDefault, "enableGoalModeByDefault"),
            (SettingKey::Notifications, "enableNotifications"),
            (SettingKey::FooterTips, "showFooterTips"),
            (SettingKey::PerformanceProfiler, "showPerformanceProfiler"),
            (SettingKey::ColorTheme, "colorTheme"),
            (SettingKey::BaseBranch, "baseBranch"),
            (SettingKey::BranchPrefix, "branchPrefix"),
            (
                SettingKey::PromptForGitOptionsOnCreate,
                "promptForGitOptionsOnCreate",
            ),
            (SettingKey::MinPaneWidth, "minPaneWidth"),
            (SettingKey::MaxPaneWidth, "maxPaneWidth"),
            (SettingKey::Language, "language"),
            (SettingKey::EnabledAgents, "enabledAgents"),
            (
                SettingKey::EnabledNotificationSounds,
                "enabledNotificationSounds",
            ),
        ];
        for (key, serialized) in expected {
            assert_eq!(key.as_str(), serialized);
        }
    }

    #[test]
    fn profiler_scope_is_global_and_other_settings_keep_the_requested_scope() {
        assert_eq!(
            SettingKey::PerformanceProfiler.write_scope(SettingsScope::Project),
            SettingsScope::Global
        );
        assert_eq!(
            SettingKey::ColorTheme.write_scope(SettingsScope::Project),
            SettingsScope::Project
        );
    }
}
