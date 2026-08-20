use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Two-scope settings store mirroring the TS files: global
/// `~/.dmux.global.json` and project `<root>/.dmux/settings.json`. Values are
/// kept as raw JSON maps so fields the Rust side doesn't model round-trip
/// untouched, and writes are atomic (tmp + rename) like `atomicWrite.ts`.
#[derive(Debug)]
pub struct SettingsStore {
    global_path: PathBuf,
    project_path: Option<PathBuf>,
    pub global: Map<String, Value>,
    pub project: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsScope {
    Global,
    Project,
}

impl SettingsStore {
    pub fn load(home: &Path, project_root: Option<&Path>) -> Self {
        let global_path = home.join(".dmux.global.json");
        let project_path = project_root.map(|r| r.join(".dmux").join("settings.json"));
        Self {
            global: read_map(&global_path),
            project: project_path.as_deref().map(read_map).unwrap_or_default(),
            global_path,
            project_path,
        }
    }

    /// Effective value: project overrides global.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.project.get(key).or_else(|| self.global.get(key))
    }

    /// Global value without project override, for application-level settings.
    pub fn get_global(&self, key: &str) -> Option<&Value> {
        self.global.get(key)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn get_global_bool(&self, key: &str, default: bool) -> bool {
        self.get_global(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|v| v.as_u64())
    }

    /// The scope currently providing `key`, if any.
    pub fn effective_scope(&self, key: &str) -> Option<SettingsScope> {
        if self.project.contains_key(key) {
            Some(SettingsScope::Project)
        } else if self.global.contains_key(key) {
            Some(SettingsScope::Global)
        } else {
            None
        }
    }

    pub fn set(&mut self, key: &str, value: Value, scope: SettingsScope) {
        match scope {
            SettingsScope::Global => {
                self.global.insert(key.to_string(), value);
            }
            SettingsScope::Project => {
                self.project.insert(key.to_string(), value);
            }
        }
    }

    pub fn unset(&mut self, key: &str, scope: SettingsScope) {
        match scope {
            SettingsScope::Global => self.global.remove(key),
            SettingsScope::Project => self.project.remove(key),
        };
    }

    pub fn has_project_scope(&self) -> bool {
        self.project_path.is_some()
    }

    pub fn save(&self, scope: SettingsScope) -> std::io::Result<()> {
        match scope {
            SettingsScope::Global => write_map_atomic(&self.global_path, &self.global),
            SettingsScope::Project => match &self.project_path {
                Some(path) => write_map_atomic(path, &self.project),
                None => Ok(()),
            },
        }
    }
}

fn read_map(path: &Path) -> Map<String, Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

fn write_map_atomic(path: &Path, map: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut json = serde_json::to_string_pretty(&Value::Object(map.clone()))?;
    json.push('\n');
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_precedence_and_roundtrip() {
        let dir = std::env::temp_dir().join(format!("dmux-core-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        std::fs::write(
            dir.join(".dmux.global.json"),
            r#"{"colorTheme":"violet","unknownField":{"keep":true}}"#,
        )
        .unwrap();

        let mut store = SettingsStore::load(&dir, Some(&dir.join("proj")));
        assert_eq!(store.get_str("colorTheme"), Some("violet"));
        store.set(
            "colorTheme",
            Value::String("cyan".into()),
            SettingsScope::Project,
        );
        assert_eq!(store.get_str("colorTheme"), Some("cyan"));
        assert_eq!(
            store.effective_scope("colorTheme"),
            Some(SettingsScope::Project)
        );

        store.save(SettingsScope::Project).unwrap();
        store.set("minPaneWidth", Value::from(70u64), SettingsScope::Global);
        store.set("appFlag", Value::Bool(true), SettingsScope::Global);
        store.set("appFlag", Value::Bool(false), SettingsScope::Project);
        store.save(SettingsScope::Project).unwrap();
        store.save(SettingsScope::Global).unwrap();

        let reloaded = SettingsStore::load(&dir, Some(&dir.join("proj")));
        assert_eq!(reloaded.get_str("colorTheme"), Some("cyan"));
        assert_eq!(reloaded.get_u64("minPaneWidth"), Some(70));
        // Unknown fields preserved through the global save.
        assert!(reloaded.global.get("unknownField").is_some());
        assert!(!reloaded.get_bool("appFlag", true));
        assert!(reloaded.get_global_bool("appFlag", false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
