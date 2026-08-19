//! Notification-sound catalog — port of `notificationSounds.ts`. Settings
//! store the ids (`enabledNotificationSounds`); the helper payload wants the
//! bundled `.caf` resource name, with `None` meaning the system default.

pub struct SoundDef {
    pub id: &'static str,
    pub label: &'static str,
    pub resource: Option<&'static str>,
    pub default_enabled: bool,
}

pub const SOUNDS: &[SoundDef] = &[
    SoundDef {
        id: "default-system-sound",
        label: "Default System Sound",
        resource: None,
        default_enabled: true,
    },
    SoundDef {
        id: "braam",
        label: "Braam",
        resource: Some("dmux-braam.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "brass",
        label: "Brass",
        resource: Some("dmux-brass.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "ding-bell",
        label: "Ding Bell",
        resource: Some("dmux-ding-bell.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "future",
        label: "Future",
        resource: Some("dmux-future.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "harp",
        label: "Harp",
        resource: Some("dmux-harp.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "quiet-bells",
        label: "Quiet Bells",
        resource: Some("dmux-quiet-bells.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "sonar",
        label: "Sonar",
        resource: Some("dmux-sonar.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "success",
        label: "Success",
        resource: Some("dmux-success.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "triumphant-trumpet",
        label: "Triumphant Trumpet",
        resource: Some("dmux-triumphant-trumpet.caf"),
        default_enabled: false,
    },
    SoundDef {
        id: "war-horn",
        label: "War Horn",
        resource: Some("dmux-war-horn.caf"),
        default_enabled: false,
    },
];

/// Enabled sounds in catalog order; a missing/empty/all-invalid setting falls
/// back to the defaults (same resolution the TS side uses), so the result is
/// never empty.
pub fn resolve_selection(setting: Option<&serde_json::Value>) -> Vec<&'static SoundDef> {
    let configured: Vec<&str> = setting
        .and_then(|v| v.as_array())
        .map(|l| l.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let picked: Vec<&SoundDef> = SOUNDS
        .iter()
        .filter(|s| configured.contains(&s.id))
        .collect();
    if !picked.is_empty() {
        return picked;
    }
    SOUNDS.iter().filter(|s| s.default_enabled).collect()
}

/// Resource file for one alert (TS randomizes; a seed pick keeps us
/// rng-free). `None` = play the system default sound.
pub fn pick_resource(setting: Option<&serde_json::Value>, seed: u64) -> Option<String> {
    let selection = resolve_selection(setting);
    selection[(seed as usize) % selection.len()]
        .resource
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn falls_back_to_defaults() {
        assert_eq!(resolve_selection(None).len(), 1);
        assert_eq!(resolve_selection(None)[0].id, "default-system-sound");
        assert_eq!(
            resolve_selection(Some(&json!(["nope"])))[0].id,
            "default-system-sound"
        );
        assert_eq!(pick_resource(None, 7), None);
    }

    #[test]
    fn catalog_order_and_resources() {
        let sel = resolve_selection(Some(&json!(["war-horn", "braam"])));
        let ids: Vec<_> = sel.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["braam", "war-horn"]);
        assert_eq!(
            pick_resource(Some(&json!(["braam"])), 3).as_deref(),
            Some("dmux-braam.caf")
        );
    }
}
