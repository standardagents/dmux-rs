//! Agent registry, ported 1:1 from `src/utils/agentLaunch.ts` (data and
//! transport semantics). Command composition happens here; delivery is the
//! app's control-mode send-keys.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// `cmd "<prompt>" <flags>`
    Positional,
    /// `cmd <opt> "<prompt>" <flags>`
    Option(&'static str),
    /// `printf '%s\n' "<prompt>" | cmd <flags>`
    Stdin,
    /// Launch bare; type the prompt into the running TUI after a delay.
    SendKeys { ready_delay_ms: u64 },
}

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub id: &'static str,
    pub name: &'static str,
    pub short: &'static str,
    pub command: &'static str,
    /// Launch command when no prompt is given (defaults to `command`).
    pub bare_command: &'static str,
    pub transport: Transport,
    /// Flags per permission mode: (plan, accept_edits, bypass).
    pub flags_plan: Option<&'static str>,
    pub flags_accept: Option<&'static str>,
    pub flags_bypass: Option<&'static str>,
    pub default_enabled: bool,
    /// Resume-most-recent-session command template ({permissions} substituted).
    pub resume_template: Option<&'static str>,
    /// Exact-session resume template ({sessionId} + {permissions}).
    pub resume_session_template: Option<&'static str>,
}

pub const AGENTS: &[AgentDef] = &[
    AgentDef {
        id: "claude",
        name: "Claude Code",
        short: "cc",
        command: "claude",
        bare_command: "claude",
        transport: Transport::Positional,
        flags_plan: Some("--permission-mode plan"),
        flags_accept: Some("--permission-mode acceptEdits"),
        flags_bypass: Some("--dangerously-skip-permissions"),
        default_enabled: true,
        resume_template: Some("claude --continue{permissions}"),
        resume_session_template: Some("claude --resume {sessionId}{permissions}"),
    },
    AgentDef {
        id: "opencode",
        name: "OpenCode",
        short: "oc",
        command: "opencode",
        bare_command: "opencode",
        transport: Transport::Option("--prompt"),
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: true,
        resume_template: None,
        resume_session_template: None,
    },
    AgentDef {
        id: "codex",
        name: "Codex",
        short: "cx",
        command: "codex",
        bare_command: "codex",
        transport: Transport::Positional,
        flags_plan: None,
        flags_accept: Some("--full-auto"),
        flags_bypass: Some("--dangerously-bypass-approvals-and-sandbox"),
        default_enabled: true,
        resume_template: Some("codex resume --last{permissions}"),
        resume_session_template: Some("codex resume {sessionId}{permissions}"),
    },
    AgentDef {
        id: "grok",
        name: "Grok Build",
        short: "gb",
        command: "grok",
        bare_command: "grok",
        transport: Transport::SendKeys { ready_delay_ms: 2500 },
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: false,
        resume_template: Some("grok --continue{permissions}"),
        resume_session_template: None,
    },
    AgentDef {
        id: "cline",
        name: "Cline CLI",
        short: "cl",
        command: "cline",
        bare_command: "cline",
        transport: Transport::SendKeys { ready_delay_ms: 2500 },
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: false,
        resume_template: None,
        resume_session_template: None,
    },
    AgentDef {
        id: "gemini",
        name: "Gemini CLI",
        short: "gm",
        command: "gemini",
        bare_command: "gemini",
        transport: Transport::Option("--prompt-interactive"),
        flags_plan: None,
        flags_accept: None,
        flags_bypass: Some("--yolo"),
        default_enabled: false,
        resume_template: Some("gemini --resume latest{permissions}"),
        resume_session_template: None,
    },
    AgentDef {
        id: "qwen",
        name: "Qwen CLI",
        short: "qn",
        command: "qwen",
        bare_command: "qwen",
        transport: Transport::Option("-i"),
        flags_plan: None,
        flags_accept: None,
        flags_bypass: Some("--yolo"),
        default_enabled: false,
        resume_template: Some("qwen --continue{permissions}"),
        resume_session_template: None,
    },
    AgentDef {
        id: "amp",
        name: "Amp CLI",
        short: "ap",
        command: "amp",
        bare_command: "amp",
        transport: Transport::Stdin,
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: false,
        resume_template: None,
        resume_session_template: None,
    },
    AgentDef {
        id: "pi",
        name: "pi CLI",
        short: "pi",
        command: "pi",
        bare_command: "pi",
        transport: Transport::Positional,
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: false,
        resume_template: Some("pi --continue{permissions}"),
        resume_session_template: None,
    },
    AgentDef {
        id: "cursor",
        name: "Cursor CLI",
        short: "cr",
        command: "cursor-agent",
        bare_command: "cursor-agent",
        transport: Transport::Positional,
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: false,
        resume_template: None,
        resume_session_template: None,
    },
    AgentDef {
        id: "copilot",
        name: "Copilot CLI",
        short: "co",
        command: "copilot",
        bare_command: "copilot",
        transport: Transport::Option("-i"),
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: false,
        resume_template: Some("copilot --continue{permissions}"),
        resume_session_template: None,
    },
    AgentDef {
        id: "crush",
        name: "Crush CLI",
        short: "cs",
        command: "crush run",
        bare_command: "crush",
        transport: Transport::SendKeys { ready_delay_ms: 2500 },
        flags_plan: None,
        flags_accept: None,
        flags_bypass: None,
        default_enabled: false,
        resume_template: None,
        resume_session_template: None,
    },
];

pub fn agent(id: &str) -> Option<&'static AgentDef> {
    AGENTS.iter().find(|a| a.id == id)
}

/// Which agents are installed. Checked once at startup (a dozen `command -v`
/// probes through the user's login shell so PATH additions are honored).
pub fn detect_installed() -> std::collections::HashSet<&'static str> {
    let mut installed = std::collections::HashSet::new();
    for def in AGENTS {
        let bin = def.command.split(' ').next().unwrap_or(def.command);
        let found = std::process::Command::new("/bin/sh")
            .args(["-lc", &format!("command -v {bin} >/dev/null 2>&1")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if found {
            installed.insert(def.id);
        }
    }
    installed
}

/// Permission flags for a mode name ('', plan, acceptEdits, bypassPermissions).
pub fn permission_flags(def: &AgentDef, mode: &str) -> Option<&'static str> {
    match mode {
        "plan" => def.flags_plan,
        "acceptEdits" => def.flags_accept,
        "bypassPermissions" => def.flags_bypass,
        _ => None,
    }
}

/// Resume the most recent session for an agent (TS `resumeCommandTemplate`).
/// None when the agent has no resume support — callers fall back to a bare
/// launch.
pub fn compose_resume(def: &AgentDef, mode: &str) -> Option<String> {
    let template = def.resume_template?;
    let flags = permission_flags(def, mode).map(|f| format!(" {f}")).unwrap_or_default();
    Some(template.replace("{permissions}", &flags))
}

/// Resume an exact captured session (TS `resumeSessionCommandTemplate`),
/// falling back to resume-latest when unsupported.
pub fn compose_resume_session(def: &AgentDef, session_id: Option<&str>, mode: &str) -> Option<String> {
    if let (Some(template), Some(id)) = (def.resume_session_template, session_id) {
        let flags = permission_flags(def, mode).map(|f| format!(" {f}")).unwrap_or_default();
        return Some(template.replace("{sessionId}", id).replace("{permissions}", &flags));
    }
    compose_resume(def, mode)
}

/// Compose the in-pane shell command that reads the prompt file, deletes it,
/// and launches the agent. Mirrors `buildPromptReadAndDeleteSnippet` +
/// transport composition from the TS registry. For `SendKeys` transports the
/// returned command launches the bare TUI; the caller schedules the prompt
/// injection afterwards.
pub fn compose_launch(def: &AgentDef, prompt_file: Option<&str>, mode: &str) -> String {
    let flags = permission_flags(def, mode).map(|f| format!(" {f}")).unwrap_or_default();
    let Some(pf) = prompt_file else {
        return format!("{}{}", def.bare_command, flags);
    };
    let read = format!("DMUX_PROMPT=\"$(cat {pf})\"; rm -f {pf}; ");
    match def.transport {
        Transport::Positional => format!("{read}{} \"$DMUX_PROMPT\"{flags}", def.command),
        Transport::Option(opt) => format!("{read}{} {opt} \"$DMUX_PROMPT\"{flags}", def.command),
        Transport::Stdin => format!("{read}printf '%s\\n' \"$DMUX_PROMPT\" | {}{flags}", def.command),
        Transport::SendKeys { .. } => format!("{}{}", def.bare_command, flags),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_labels_unique_and_two_chars() {
        let mut seen = std::collections::HashSet::new();
        for a in AGENTS {
            assert_eq!(a.short.chars().count(), 2, "{} shortLabel must be 2 chars", a.id);
            assert!(seen.insert(a.short), "duplicate shortLabel {}", a.short);
        }
        assert_eq!(AGENTS.len(), 12);
    }

    #[test]
    fn launch_composition() {
        let claude = agent("claude").unwrap();
        let cmd = compose_launch(claude, Some("/tmp/p.txt"), "acceptEdits");
        assert_eq!(
            cmd,
            "DMUX_PROMPT=\"$(cat /tmp/p.txt)\"; rm -f /tmp/p.txt; claude \"$DMUX_PROMPT\" --permission-mode acceptEdits"
        );
        let amp = agent("amp").unwrap();
        let cmd = compose_launch(amp, Some("/tmp/p.txt"), "");
        assert!(cmd.contains("printf '%s\\n' \"$DMUX_PROMPT\" | amp"));
        let grok = agent("grok").unwrap();
        assert_eq!(compose_launch(grok, Some("/tmp/p.txt"), ""), "grok");
        assert_eq!(compose_launch(claude, None, "plan"), "claude --permission-mode plan");
    }
}
