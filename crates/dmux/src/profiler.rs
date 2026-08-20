//! Profiler pointer behavior (#103): the title row drags the card (grab
//! offset preserved, position clamped so the overlay is always
//! recoverable), the ✕ dismisses it, and every visibility control writes the
//! same global setting.

use crate::input::MouseKind;
use crate::render;
use crate::settings::SettingKey;
use crate::views::ClickTarget;
use crate::App;

pub(crate) fn configured_visible(settings: &dmux_core::SettingsStore) -> bool {
    settings.get_global_bool(SettingKey::PerformanceProfiler.as_str(), false)
}

fn save_visibility(settings: &mut dmux_core::SettingsStore, visible: bool) -> std::io::Result<()> {
    settings.set(
        SettingKey::PerformanceProfiler.as_str(),
        serde_json::Value::Bool(visible),
        dmux_core::SettingsScope::Global,
    );
    settings.save(dmux_core::SettingsScope::Global)
}

impl App {
    pub(crate) fn apply_profiler_visibility(&mut self, visible: bool) {
        self.profiler = visible;
        self.force_full = true;
        self.dirty = true;
    }

    pub(crate) fn set_profiler_visibility(&mut self, visible: bool) {
        let save = {
            let mut settings = self.settings.lock().unwrap();
            save_visibility(&mut settings, visible)
        };
        if let Err(err) = save {
            tracing::warn!(%err, "profiler visibility save failed");
        }
        self.apply_profiler_visibility(visible);
    }

    /// Handle a press on a profiler control. `ProfilerClose` dismisses; `ProfilerTitle`
    /// starts a drag anchored at the pointer's offset within the card.
    pub(crate) fn profiler_press(
        &mut self,
        target: Option<ClickTarget>,
        col: u16,
        row: u16,
    ) -> bool {
        match target {
            Some(ClickTarget::ProfilerClose) => {
                self.set_profiler_visibility(false);
            }
            Some(ClickTarget::ProfilerTitle) => {
                let rect = render::profiler_layout(
                    self.back.area(),
                    &self.metrics,
                    self.profiler_pos,
                    self.layout.sidebar.right(),
                );
                self.profiler_drag = Some((col.saturating_sub(rect.x), row.saturating_sub(rect.y)));
            }
            _ => {}
        }
        true
    }

    /// While a profiler drag is active, follow the pointer (minus the grab
    /// offset, clamped on screen) and swallow the mouse until release.
    /// Returns None when no drag is active.
    pub(crate) fn profiler_drag_motion(
        &mut self,
        kind: MouseKind,
        is_press: bool,
        col: u16,
        row: u16,
    ) -> Option<bool> {
        let (gx, gy) = self.profiler_drag?;
        match kind {
            MouseKind::LeftHeld if !is_press => {
                let area = self.back.area();
                let sidebar_right = self.layout.sidebar.right();
                let rect =
                    render::profiler_layout(area, &self.metrics, self.profiler_pos, sidebar_right);
                let workspace = render::profiler_workspace(area, sidebar_right);
                self.profiler_pos = Some(render::profiler_clamp(
                    (col.saturating_sub(gx), row.saturating_sub(gy)),
                    (rect.w, rect.h),
                    workspace,
                ));
                self.dirty = true;
                Some(true)
            }
            MouseKind::Release => {
                self.profiler_drag = None;
                Some(true)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_key_survives_the_rename() {
        // Persisted installations keep their visibility (#115): the key is
        // the pre-rename spelling and must never change.
        assert_eq!(VISIBLE_SETTING, "showPerformanceProfiler");
    }

    #[test]
    fn visibility_round_trips_through_the_global_settings_file() {
        let dir = std::env::temp_dir().join(format!(
            "dmux-profiler-setting-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut settings = dmux_core::SettingsStore::load(&dir, Some(&dir.join("project")));

        save_visibility(&mut settings, true).unwrap();
        let reloaded = dmux_core::SettingsStore::load(&dir, Some(&dir.join("project")));
        assert!(configured_visible(&reloaded));
        assert!(!reloaded
            .project
            .contains_key(SettingKey::PerformanceProfiler.as_str()));

        let _ = std::fs::remove_dir_all(dir);
    }
}
