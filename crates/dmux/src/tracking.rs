//! Agent process tracking (port of `paneAgentTracking.ts`): walk the process
//! tree under each pane's shell to find a running agent, then inspect its
//! open files to capture the exact resumable session id (Claude Code's
//! `~/.claude/projects/**/<uuid>.jsonl`, Codex's
//! `~/.codex/sessions/**/rollout-*-<uuid>.jsonl`). Everything here is
//! blocking (subprocess-based) — call from spawn_blocking.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct AgentObservation {
    pub agent_id: &'static str,
    pub agent_pid: u32,
    pub session_id: Option<String>,
}

/// Process table snapshot: pid → (ppid, command line).
fn process_table() -> HashMap<u32, (u32, String)> {
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output();
    let mut table = HashMap::new();
    if let Ok(out) = out {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            // ps right-aligns columns: consecutive spaces between fields.
            let mut rest = line.trim_start();
            let mut take = || {
                let t = rest.trim_start();
                let end = t.find(char::is_whitespace).unwrap_or(t.len());
                let (tok, r) = t.split_at(end);
                rest = r;
                tok
            };
            let (Ok(pid), Ok(ppid)) = (take().parse::<u32>(), take().parse::<u32>()) else {
                continue;
            };
            let cmd = rest.trim_start().to_string();
            table.insert(pid, (ppid, cmd));
        }
    }
    table
}

/// Does this command line run a known agent? Returns the agent id.
fn match_agent(cmd: &str) -> Option<&'static str> {
    // Agents run bare, via node shims, or via interpreter wrappers
    // ("node /path/claude", "bash /path/claude") — check the basename of the
    // first few tokens.
    for token in cmd.split_whitespace().take(3) {
        let base = token.rsplit('/').next().unwrap_or(token);
        for def in crate::agents::AGENTS {
            let bin = def.command.split(' ').next().unwrap_or(def.command);
            if base == bin {
                return Some(def.id);
            }
        }
    }
    None
}

/// Find an agent process in the tree rooted at `pane_pid` (BFS).
fn find_agent(table: &HashMap<u32, (u32, String)>, pane_pid: u32) -> Option<(&'static str, u32)> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, &(ppid, _)) in table {
        children.entry(ppid).or_default().push(pid);
    }
    let mut queue = vec![pane_pid];
    let mut depth = 0;
    while !queue.is_empty() && depth < 6 {
        let mut next = Vec::new();
        for pid in queue {
            if let Some((_, cmd)) = table.get(&pid) {
                if let Some(agent) = match_agent(cmd) {
                    return Some((agent, pid));
                }
            }
            if let Some(kids) = children.get(&pid) {
                next.extend(kids.iter().copied());
            }
        }
        queue = next;
        depth += 1;
    }
    None
}

/// Extract a session id from an open file path, per agent conventions.
fn session_from_path(agent: &str, path: &str) -> Option<String> {
    match agent {
        "claude" => {
            if !path.contains("/.claude/projects/") || !path.ends_with(".jsonl") {
                return None;
            }
            let stem = path.rsplit('/').next()?.strip_suffix(".jsonl")?;
            is_uuid(stem).then(|| stem.to_string())
        }
        "codex" => {
            if !path.contains("/.codex/sessions/") || !path.ends_with(".jsonl") {
                return None;
            }
            let name = path.rsplit('/').next()?;
            let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
            // rollout-<timestamp>-<uuid>.jsonl — the uuid is the last 36 chars.
            let uuid = stem.get(stem.len().checked_sub(36)?..)?;
            is_uuid(uuid).then(|| uuid.to_string())
        }
        _ => None,
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Open-file paths of a process: /proc on Linux, lsof elsewhere.
fn open_files(pid: u32) -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let dir = format!("/proc/{pid}/fd");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            return entries
                .flatten()
                .filter_map(|e| std::fs::read_link(e.path()).ok())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
        }
        Vec::new()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("lsof")
            .args(["-Fn", "-p", &pid.to_string()])
            .output();
        match out {
            Ok(out) => String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.strip_prefix('n'))
                .map(String::from)
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Observe agents in the given panes: (slug, pane_pid) → observation.
pub fn observe(panes: &[(String, u32)]) -> Vec<(String, AgentObservation)> {
    let table = process_table();
    tracing::debug!(
        processes = table.len(),
        panes = panes.len(),
        "tracking observe"
    );
    let mut out = Vec::new();
    for (slug, pane_pid) in panes {
        match find_agent(&table, *pane_pid) {
            Some((agent_id, agent_pid)) => {
                let files = open_files(agent_pid);
                let session_id = files.iter().find_map(|p| session_from_path(agent_id, p));
                tracing::debug!(%slug, agent_id, agent_pid, files = files.len(), session = ?session_id, "agent observed");
                out.push((
                    slug.clone(),
                    AgentObservation {
                        agent_id,
                        agent_pid,
                        session_id,
                    },
                ));
            }
            None => {
                let root = table.get(pane_pid);
                tracing::debug!(%slug, pane_pid, root_cmd = ?root.map(|r| &r.1), "no agent found in tree");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_path_extraction() {
        assert_eq!(
            session_from_path(
                "claude",
                "/Users/x/.claude/projects/-Users-x-proj/0195cb1e-1111-7222-8333-444455556666.jsonl"
            ),
            Some("0195cb1e-1111-7222-8333-444455556666".to_string())
        );
        assert_eq!(
            session_from_path(
                "codex",
                "/Users/x/.codex/sessions/2026/08/rollout-2026-08-18T10-00-00-abcdefab-1234-5678-9abc-def012345678.jsonl"
            ),
            Some("abcdefab-1234-5678-9abc-def012345678".to_string())
        );
        assert_eq!(session_from_path("claude", "/tmp/other.jsonl"), None);
        assert_eq!(
            session_from_path("claude", "/Users/x/.claude/projects/x/notauuid.jsonl"),
            None
        );
    }

    #[test]
    fn agent_command_matching() {
        assert_eq!(match_agent("claude --continue"), Some("claude"));
        assert_eq!(match_agent("/opt/homebrew/bin/codex resume"), Some("codex"));
        assert_eq!(
            match_agent("node /usr/local/bin/claude --flag"),
            Some("claude")
        );
        assert_eq!(match_agent("vim ."), None);
    }

    #[test]
    fn finds_agent_in_own_tree() {
        // Smoke: our own process table parses without panicking.
        let table = process_table();
        assert!(!table.is_empty());
    }
}
