//! Agent-pane launch planning: shared names, collision handling, worktree
//! bootstrap setup, and handoff to the tmux window creator.

use std::path::PathBuf;

use dmux_core::PaneKind;

use crate::views::AgentLaunchIdentity;
use crate::window_launch::{BootstrapSpec, NewWindowCtx};
use crate::{agents, bootstrap, git, hooks, registry, slugify, timestamp, App};

fn launch_names(prompt: &str, identity: Option<&AgentLaunchIdentity>) -> (String, String) {
    identity
        .map(|identity| (identity.slug.clone(), identity.display.clone()))
        .unwrap_or_else(|| {
            let slug = slugify(prompt);
            (slug.clone(), slug)
        })
}

fn unique_agent_slug(
    base: &str,
    agent_short: &str,
    ordinal: u8,
    total: u32,
    taken: impl Fn(&str) -> bool,
) -> String {
    let initial = if total == 1 {
        base.to_string()
    } else {
        format!("{base}-{agent_short}-{ordinal}")
    };
    if !taken(&initial) {
        return initial;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{agent_short}-{suffix}");
        if !taken(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn worktree_target(
    project_root: &std::path::Path,
    slug: &str,
    branch_prefix: &str,
) -> (String, String) {
    let branch = format!("{branch_prefix}{slug}");
    let path = project_root.join(".dmux").join("worktrees").join(slug);
    (branch, path.to_string_lossy().into_owned())
}

impl App {
    pub(super) fn launch_agents(
        &mut self,
        prompt: String,
        allocations: Vec<(String, u8)>,
        mode: String,
        project_root: Option<String>,
        identity: Option<AgentLaunchIdentity>,
    ) {
        let total: u32 = allocations.iter().map(|(_, count)| *count as u32).sum();
        if total == 0 {
            return;
        }
        let project_root = project_root
            .or_else(|| self.active_project_root())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());
        let project_context = registry::project_context(
            &self.project_root,
            Some(project_root.to_string_lossy().into_owned()),
        );
        let (base_slug, base_display) = launch_names(&prompt, identity.as_ref());
        let (base_branch, branch_prefix) = {
            let settings = self.settings.lock().unwrap();
            (
                settings.get_str("baseBranch").unwrap_or("").to_string(),
                settings.get_str("branchPrefix").unwrap_or("").to_string(),
            )
        };

        for (agent_id, count) in &allocations {
            let Some(definition) = agents::agent(agent_id) else {
                continue;
            };
            for ordinal in 1..=*count {
                let slug =
                    unique_agent_slug(&base_slug, definition.short, ordinal, total, |candidate| {
                        self.config.panes.iter().any(|pane| pane.slug == candidate)
                            || self.panes.iter().any(|pane| pane.slug == candidate)
                    });
                let prompt_file = (!prompt.is_empty()).then(|| {
                    let directory = project_root.join(".dmux").join("prompts");
                    let _ = std::fs::create_dir_all(&directory);
                    let path = directory.join(format!("{slug}-{}.txt", timestamp()));
                    let _ = std::fs::write(&path, &prompt);
                    path.to_string_lossy().into_owned()
                });
                let injection = match definition.transport {
                    agents::Transport::SendKeys { ready_delay_ms } if !prompt.is_empty() => {
                        Some((prompt.clone(), ready_delay_ms))
                    }
                    _ => None,
                };
                let agent_cmd = agents::compose_launch(definition, prompt_file.as_deref(), &mode);

                let (launch_cmd, injection, worktree_path, bootstrap) =
                    if git::git_main_worktree_root(&project_root).is_some() {
                        let (branch, worktree) =
                            worktree_target(&project_root, &slug, &branch_prefix);
                        let root = project_root.to_string_lossy().into_owned();
                        let spec = BootstrapSpec {
                            plan: bootstrap::Plan {
                                root: root.clone(),
                                wt: worktree.clone(),
                                branch,
                                base_branch: base_branch.clone(),
                                slug: slug.clone(),
                                has_hook: hooks::hook_path(&project_root, "worktree_created")
                                    .is_some(),
                            },
                            launch: bootstrap::Launch {
                                agent_cmd,
                                wt: worktree.clone(),
                                root,
                                injection,
                            },
                            agent_label: definition.name.to_string(),
                        };
                        (None, None, Some(worktree), Some(spec))
                    } else {
                        (Some(format!("clear; {agent_cmd}")), injection, None, None)
                    };

                self.create_window(NewWindowCtx {
                    bootstrap,
                    prompt: prompt.clone(),
                    display: if total == 1 {
                        base_display.clone()
                    } else {
                        format!("{base_display} ({}{ordinal})", definition.short)
                    },
                    slug,
                    kind: PaneKind::Worktree,
                    agent: Some(definition.id.to_string()),
                    launch_cmd,
                    injection,
                    worktree_path,
                    cwd: None,
                    project_root: project_context.clone(),
                });
            }
        }
        self.toast(format!(
            "Launching {total} pane{}…",
            if total == 1 { "" } else { "s" }
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn source_identity_overrides_prompt_derived_names() {
        let identity = AgentLaunchIdentity {
            slug: "issue-106-derived-name".into(),
            display: "#106 Derived name".into(),
        };
        assert_eq!(
            launch_names("Work on these assigned issues", Some(&identity)),
            (identity.slug, identity.display)
        );
    }

    #[test]
    fn collisions_keep_the_issue_identity_and_use_stable_suffixes() {
        let taken: BTreeSet<&str> = ["issue-106-derived-name", "issue-106-derived-name-cx-2"]
            .into_iter()
            .collect();
        let slug = unique_agent_slug("issue-106-derived-name", "cx", 1, 1, |candidate| {
            taken.contains(candidate)
        });
        assert_eq!(slug, "issue-106-derived-name-cx-3");
    }

    #[test]
    fn issue_identity_reaches_branch_and_worktree_names() {
        let identity = AgentLaunchIdentity {
            slug: "issue-106-derived-name".into(),
            display: "#106 Derived name".into(),
        };
        let (base, display) = launch_names("generic prompt", Some(&identity));
        let slug = unique_agent_slug(&base, "cx", 1, 1, |_| false);
        let (branch, worktree) =
            worktree_target(std::path::Path::new("/projects/dmux"), &slug, "feat/");

        assert_eq!(display, "#106 Derived name");
        assert_eq!(slug, "issue-106-derived-name");
        assert_eq!(branch, "feat/issue-106-derived-name");
        assert_eq!(
            worktree,
            "/projects/dmux/.dmux/worktrees/issue-106-derived-name"
        );
    }
}
