use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Mirror of the TS `DmuxPane` interface (`src/types.ts`), modeling only the
/// fields the renderer needs; everything else round-trips through `extra`.
/// Field names must stay camelCase-identical to the TS implementation — this
/// is the coexistence contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DmuxPane {
    pub id: String,
    pub slug: String,
    /// Required by the TS type; empty for terminals.
    #[serde(default)]
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The tmux pane id (`%N`) this logical pane was last bound to. Stale
    /// after a tmux restart; rebinding goes through the title contract.
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<PaneKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_attention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneKind {
    Worktree,
    Shell,
}

impl DmuxPane {
    pub fn kind(&self) -> PaneKind {
        self.kind.unwrap_or(PaneKind::Worktree)
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden.unwrap_or(false)
    }

    pub fn display_title(&self) -> &str {
        match &self.display_name {
            Some(name) if !name.trim().is_empty() => name,
            _ => &self.slug,
        }
    }
}

/// Mirror of the TS `DmuxConfig` interface. `settings` and `sidebarProjects`
/// pass through as raw JSON in Phase 0 (read-only renderer).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DmuxConfig {
    pub project_name: String,
    pub project_root: String,
    #[serde(default)]
    pub panes: Vec<DmuxPane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome_pane_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl DmuxPane {
    /// Minimal record for a pane dmux-rs created; all optional TS fields ride
    /// in `extra` when other tooling adds them.
    pub fn new_record(id: String, slug: String, pane_id: String, kind: PaneKind) -> Self {
        Self {
            id,
            slug,
            prompt: String::new(),
            display_name: None,
            pane_id,
            hidden: None,
            project_root: None,
            project_name: None,
            kind: Some(kind),
            worktree_path: None,
            shell_cwd: None,
            agent: None,
            agent_status: None,
            needs_attention: None,
            autopilot: None,
            extra: Map::new(),
        }
    }
}

impl DmuxConfig {
    /// Fresh config for a project dmux-rs is seeing for the first time.
    pub fn new(project_name: String, project_root: String) -> Self {
        let mut extra = Map::new();
        extra.insert("settings".into(), Value::Object(Map::new()));
        extra.insert("lastUpdated".into(), Value::String(String::new()));
        Self {
            project_name,
            project_root,
            panes: Vec::new(),
            control_pane_id: None,
            welcome_pane_id: None,
            extra,
        }
    }

    /// Atomic save (tmp + rename), preserving unknown fields.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path).map_err(|e| ConfigError::Io(path.display().to_string(), e))?;
        serde_json::from_slice(&bytes).map_err(|e| ConfigError::Parse(path.display().to_string(), e))
    }

    /// Default config path for a project root: `<root>/.dmux/dmux.config.json`.
    pub fn default_path(project_root: &std::path::Path) -> std::path::PathBuf {
        project_root.join(".dmux").join("dmux.config.json")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {0}: {1}")]
    Io(String, #[source] std::io::Error),
    #[error("cannot parse {0}: {1}")]
    Parse(String, #[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "projectName": "dmux",
        "projectRoot": "/Users/x/Projects/dmux",
        "panes": [
            {
                "id": "pane-1",
                "slug": "fix-auth",
                "displayName": "Fix authentication",
                "prompt": "fix the auth bug",
                "paneId": "%12",
                "type": "worktree",
                "worktreePath": "/Users/x/Projects/dmux/.dmux/worktrees/fix-auth",
                "agent": "claude",
                "agentStatus": "working",
                "autopilot": true
            },
            {
                "id": "pane-2",
                "slug": "terminal-1",
                "prompt": "",
                "paneId": "%13",
                "type": "shell",
                "shellCwd": "/Users/x",
                "hidden": true
            }
        ],
        "settings": {"defaultAgent": "claude"},
        "lastUpdated": "2026-08-18T00:00:00.000Z",
        "controlPaneId": "%0",
        "controlPaneSize": 40
    }"#;

    #[test]
    fn parses_ts_config() {
        let cfg: DmuxConfig = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.project_name, "dmux");
        assert_eq!(cfg.panes.len(), 2);
        assert_eq!(cfg.panes[0].kind(), PaneKind::Worktree);
        assert_eq!(cfg.panes[0].display_title(), "Fix authentication");
        assert_eq!(cfg.panes[1].kind(), PaneKind::Shell);
        assert!(cfg.panes[1].is_hidden());
        assert_eq!(cfg.control_pane_id.as_deref(), Some("%0"));
    }

    #[test]
    fn unknown_fields_round_trip() {
        let cfg: DmuxConfig = serde_json::from_str(SAMPLE).unwrap();
        // Fields we don't model must survive serialization.
        let out = serde_json::to_value(&cfg).unwrap();
        assert_eq!(out["panes"][0]["prompt"], "fix the auth bug");
        assert_eq!(out["panes"][0]["autopilot"], true);
        assert_eq!(out["settings"]["defaultAgent"], "claude");
        assert_eq!(out["controlPaneSize"], 40);
        assert_eq!(out["lastUpdated"], "2026-08-18T00:00:00.000Z");
    }
}
