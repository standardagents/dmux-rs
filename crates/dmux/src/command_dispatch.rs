//! Command dispatch (#93): the single boundary that turns an `AppCmd` —
//! from menus, keyboard shortcuts, views, or click targets — into state
//! transitions and side effects on `App`. Extracted from main.rs as a
//! cohesive responsibility; behavior is unchanged.

use std::path::{Path, PathBuf};

use dmux_core::i18n::{t, tf};
use dmux_core::PaneKind;

use crate::session::LogicalPane;
use crate::view_stack::OverlayOrigin;
use crate::views::{
    AgentSelectView, AppCmd, ConfirmView, InputPurpose, InputView, IssueBrowserView, MenuItem,
    MenuView, PathPickerView, SettingsView, ShortcutsView,
};
use crate::window_launch::NewWindowCtx;
use crate::{
    agents, ai_merge, audit, dirs_home, git, github, hooks, pane_actions, session, shq, view_stack,
    views, App, AppMsg,
};

/// Expand a user-entered path: `~` and `~/…` resolve against the home
/// directory; anything else is taken literally.
pub(crate) fn expand_user_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        dirs_home().join(rest)
    } else if raw == "~" {
        dirs_home()
    } else {
        PathBuf::from(raw)
    }
}

/// Next slug in a numbered family (`terminal-`, `editor-`, `hook-`,
/// `pr-`): one past the count of live panes already in the family.
pub(crate) fn numbered_slug(panes: &[LogicalPane], prefix: &str) -> String {
    let n = 1 + panes.iter().filter(|p| p.slug.starts_with(prefix)).count();
    format!("{prefix}{n}")
}

/// The global entries appended to the keyboard pane menu, with their
/// leader-key hints. One list so menu wiring and shortcuts stay in sync.
pub(crate) fn global_menu_tail() -> Vec<MenuItem> {
    vec![
        MenuItem::new(t("menu.new_agents"), "^b n", AppCmd::OpenNewAgent),
        MenuItem::new(t("menu.new_terminal"), "^b t", AppCmd::NewTerminal),
        MenuItem::new(t("menu.add_project"), "^b p", AppCmd::PromptAddProject),
        MenuItem::new(t("menu.settings"), "^b s", AppCmd::OpenSettings),
        MenuItem::new(t("menu.logs"), "^b l", AppCmd::OpenLogs),
        MenuItem::new(t("menu.shortcuts"), "^b ?", AppCmd::OpenShortcuts),
        MenuItem::new(t("menu.detach"), "^b d", AppCmd::Quit),
    ]
}

impl App {
    pub(crate) fn open_project_issue_browser(&mut self, project_root: String) -> bool {
        self.open_project_issue_browser_at(project_root, OverlayOrigin::Global)
    }

    pub(crate) fn open_project_issue_browser_at(
        &mut self,
        project_root: String,
        origin: OverlayOrigin,
    ) -> bool {
        if let Some(state) = self.project_issues.get(&project_root).cloned() {
            if !github::issue_state_label(Some(&state)).is_empty() {
                self.views
                    .push_at(Box::new(IssueBrowserView::new(project_root, state)), origin);
                self.dirty = true;
            }
        }
        true
    }

    fn open_agent_select(
        &mut self,
        project_root: Option<String>,
        prompt: Option<String>,
        origin: OverlayOrigin,
    ) {
        let (default_agent, default_mode, enabled) = {
            let settings = self.settings.lock().unwrap();
            let enabled = settings
                .get("enabledAgents")
                .and_then(|value| value.as_array().cloned())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    agents::AGENTS
                        .iter()
                        .filter(|agent| agent.default_enabled)
                        .map(|agent| agent.id.to_string())
                        .collect()
                });
            (
                settings.get_str("defaultAgent").map(str::to_string),
                settings.get_str("permissionMode").unwrap_or("").to_string(),
                enabled,
            )
        };
        let mut view = AgentSelectView::new(
            &self.installed_agents,
            &enabled,
            default_agent.as_deref(),
            &default_mode,
            project_root,
        );
        if let Some(prompt) = prompt {
            view = view.with_issue_prompt(prompt);
        }
        self.views.push_at(Box::new(view), origin);
        self.dirty = true;
    }

    /// Returns false to quit.
    pub(crate) fn execute_cmd(&mut self, cmd: AppCmd) -> bool {
        self.execute_cmd_at(cmd, OverlayOrigin::Global)
    }

    /// Execute a command while preserving the control that invoked it for
    /// any overlay the command opens.
    pub(crate) fn execute_cmd_at(&mut self, cmd: AppCmd, origin: OverlayOrigin) -> bool {
        match cmd {
            AppCmd::Quit => return false,
            AppCmd::FocusPane(i) => {
                self.sidebar_focused = false;
                self.sidebar_project = None;
                if i < self.panes.len() && !self.panes[i].hidden {
                    self.focused = i;
                    self.selected = i;
                    self.panes[i].needs_attention = false;
                    let w = self.panes[i].tmux_window;
                    let _ = self.client.send(format!("select-window -t {w}"));
                    self.rebuild_sidebar_groups();
                    self.dirty = true;
                }
            }
            AppCmd::OpenPaneFlyout { idx, y } => return self.open_sidebar_pane_flyout(idx, y),
            AppCmd::OpenPaneMenu => {
                let idx = self.selected.min(self.panes.len().saturating_sub(1));
                let mut items = self.pane_menu_items(idx, view_stack::PaneMenuClose::Confirm);
                items.extend(global_menu_tail());
                let title = self
                    .panes
                    .get(idx)
                    .map(|p| p.display_title().to_string())
                    .unwrap_or_else(|| "dmux".into());
                self.views.push(Box::new(MenuView::new(title, items)));
                self.dirty = true;
            }
            AppCmd::OpenSettings => {
                let has_project = self.settings.lock().unwrap().has_project_scope();
                let root = self
                    .active_project_root()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.project_root.clone());
                self.views.push_at(
                    Box::new(SettingsView::new(self.settings.clone(), has_project, root)),
                    origin,
                );
                self.dirty = true;
            }
            AppCmd::OpenNewAgent => self.open_agent_select(None, None, origin),
            AppCmd::OpenNewAgentAt { project_root } => {
                self.open_agent_select(Some(project_root), None, origin)
            }
            AppCmd::ChooseAgentForIssues {
                project_root,
                prompt,
            } => self.open_agent_select(Some(project_root), Some(prompt), origin),
            AppCmd::RefreshIssues { project_root } => self.refresh_project_issues(project_root),
            AppCmd::OpenUrl(url) => {
                tokio::task::spawn_blocking(move || {
                    let _ = std::process::Command::new("open").arg(url).status();
                });
            }
            AppCmd::OpenShortcuts => {
                self.views.push_at(
                    Box::new(ShortcutsView::new(
                        self.host.caps().kitty_keyboard,
                        self.keymap.describe(),
                    )),
                    origin,
                );
                self.dirty = true;
            }
            AppCmd::OpenLogs => {
                self.views
                    .push(Box::new(views::LogsView::new(self.log_path.clone())));
                self.dirty = true;
            }
            AppCmd::PromptRename(idx) => {
                if let Some(p) = self.panes.get(idx) {
                    self.views.push_at(
                        Box::new(InputView::new(
                            t("dialog.rename_title"),
                            p.display_title(),
                            "pane name",
                            InputPurpose::RenamePane(idx),
                        )),
                        origin,
                    );
                    self.dirty = true;
                }
            }
            AppCmd::ConfirmClose(idx) => {
                if self.panes.get(idx).map(|p| p.closing).unwrap_or(true) {
                    return true;
                }
                if let Some(p) = self.panes.get(idx) {
                    self.views.push_at(
                        Box::new(
                            ConfirmView::new(
                                t("dialog.close_title"),
                                tf("dialog.close_body", p.display_title()),
                                t("dialog.close_confirm"),
                                true,
                                AppCmd::ClosePane(idx),
                            )
                            // The user just asked to close this pane; Enter
                            // confirms (#11). Esc/n still cancel.
                            .focus_confirm(),
                        ),
                        origin,
                    );
                    self.dirty = true;
                }
            }
            AppCmd::CopyPath(idx) => {
                if let Some(p) = self.panes.get(idx) {
                    let path = pane_actions::path(p, &self.project_root);
                    self.forward_clipboard(&path);
                    self.toast(format!("Copied {path}"));
                }
            }
            AppCmd::OpenInEditor(idx) => {
                if let Some(p) = self.panes.get(idx) {
                    let path = pane_actions::path(p, &self.project_root);
                    let slug = numbered_slug(&self.panes, "editor-");
                    self.create_window(NewWindowCtx {
                        bootstrap: None,
                        prompt: String::new(),
                        slug,
                        display: format!("edit: {}", p.display_title()),
                        kind: PaneKind::Shell,
                        agent: None,
                        launch_cmd: Some("${EDITOR:-vi} .".into()),
                        injection: None,
                        worktree_path: Some(path.clone()),
                        cwd: Some(path),
                        project_root: None,
                    });
                }
            }
            AppCmd::MergeStart(idx) => {
                let Some(p) = self.panes.get(idx) else {
                    return true;
                };
                let Some(wt) = p.worktree_path.clone() else {
                    return true;
                };
                let slug = p.slug.clone();
                let wt_path = PathBuf::from(&wt);
                let branch = git::current_branch(&wt_path).unwrap_or_else(|| slug.clone());
                let root_branch =
                    git::current_branch(&self.project_root).unwrap_or_else(|| "HEAD".into());
                if git::worktree_dirty(&wt_path) {
                    self.views.push_at(
                        Box::new(InputView::new(
                            format!("Commit & merge '{branch}' into '{root_branch}'"),
                            "",
                            "commit message for uncommitted changes",
                            InputPurpose::MergeCommitMessage { slug },
                        )),
                        origin,
                    );
                } else {
                    self.views.push_at(
                        Box::new(ConfirmView::new(
                            "Merge worktree",
                            format!("Merge '{branch}' into '{root_branch}'?"),
                            "Merge",
                            false,
                            AppCmd::MergeExec {
                                slug,
                                message: None,
                            },
                        )),
                        origin,
                    );
                }
                self.dirty = true;
            }
            AppCmd::MergeExec { slug, message } => {
                let Some(p) = self.panes.iter().find(|p| p.slug == slug) else {
                    return true;
                };
                let Some(wt) = p.worktree_path.clone() else {
                    return true;
                };
                let wt_path = PathBuf::from(&wt);
                let branch = git::current_branch(&wt_path).unwrap_or_else(|| slug.clone());
                let root = self.project_root.clone();
                let tx = self.app_tx.clone();
                self.toast(format!("Merging '{branch}'…"));
                tokio::task::spawn_blocking(move || {
                    let result =
                        git::commit_and_merge(&root, &wt_path, &branch, message.as_deref());
                    let _ = tx.send(AppMsg::MergeDone {
                        slug,
                        branch,
                        result,
                    });
                });
            }
            AppCmd::Noop => {}
            AppCmd::ShowDiff(idx) => {
                let Some(p) = self.panes.get(idx) else {
                    return true;
                };
                let Some(wt) = p.worktree_path.clone() else {
                    return true;
                };
                let title = format!("Diff — {}", p.display_title());
                self.views
                    .push(Box::new(views::DiffView::new(title, PathBuf::from(wt))));
                self.dirty = true;
            }
            AppCmd::DuplicatePane(idx) => {
                let Some(p) = self.panes.get(idx) else {
                    return true;
                };
                let (Some(agent), slug) = (p.agent.clone(), p.slug.clone()) else {
                    self.toast("Only agent panes can be duplicated");
                    return true;
                };
                let project_root = p.project_root.clone();
                let prompt = self
                    .config
                    .panes
                    .iter()
                    .find(|r| r.slug == slug)
                    .map(|r| r.prompt.clone())
                    .unwrap_or_default();
                let mode = self
                    .settings
                    .lock()
                    .unwrap()
                    .get_str("permissionMode")
                    .unwrap_or("")
                    .to_string();
                self.launch_agents(prompt, vec![(agent, 1)], mode, project_root);
            }
            AppCmd::RunHook { idx, name } => {
                let Some(p) = self.panes.get(idx) else {
                    return true;
                };
                let root = p
                    .project_root
                    .clone()
                    .unwrap_or_else(|| self.project_root.to_string_lossy().into_owned());
                let cwd = p.worktree_path.clone().unwrap_or_else(|| root.clone());
                let slug = numbered_slug(&self.panes, "hook-");
                let label = if name == "run_test" { "tests" } else { "dev" };
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    slug,
                    display: format!("{label}: {}", p.display_title()),
                    kind: PaneKind::Shell,
                    agent: None,
                    launch_cmd: Some(format!(
                        "clear; DMUX_ROOT={r} DMUX_WORKTREE_PATH={w} {r}/.dmux-hooks/{name}; echo; echo '[hook exited — close this pane when finished]'",
                        r = shq(&root),
                        w = shq(&cwd),
                    )),
                    injection: None,
                    worktree_path: None,
                    cwd: Some(cwd),
                    project_root: Some(root),
                });
            }
            AppCmd::SearchScrollback(query) => {
                self.last_search = Some(query.clone());
                if let Some(p) = self.panes.get_mut(self.focused) {
                    match p.term.search_back(&query) {
                        Some(offset) => {
                            p.dirty = true;
                            self.dirty = true;
                            self.toast(format!(
                                "Found '{query}' ({offset} lines back) — ⌥PgDn to return"
                            ));
                        }
                        None => self.toast(format!("No match for '{query}' above")),
                    }
                }
            }
            AppCmd::AiMerge { branch } => {
                let (Some(primary), backup) = (
                    self.inference_primary.clone(),
                    self.inference_backup.clone(),
                ) else {
                    self.toast("No inference provider configured");
                    return true;
                };
                let root = self.project_root.clone();
                let tx = self.app_tx.clone();
                let b = branch.clone();
                self.toast(format!("AI-merging '{branch}'…"));
                tokio::spawn(async move {
                    let result = ai_merge(&root, &b, &primary, backup.as_ref()).await;
                    let _ = tx.send(AppMsg::AiMergeDone { branch: b, result });
                });
            }
            AppCmd::ResolveConflicts { branch } => {
                let root = self.project_root.clone();
                let tx = self.app_tx.clone();
                let b = branch.clone();
                self.toast("Re-establishing conflict state…");
                tokio::task::spawn_blocking(move || {
                    let files = git::merge_leaving_conflicts(&root, &b);
                    let _ = tx.send(AppMsg::ConflictsReady { branch: b, files });
                });
            }
            AppCmd::MergeCleanup { slug } => {
                if let Some(idx) = self.panes.iter().position(|p| p.slug == slug) {
                    let wt = self.panes[idx].worktree_path.clone();
                    let branch = wt
                        .as_deref()
                        .map(PathBuf::from)
                        .and_then(|p| git::current_branch(&p))
                        .unwrap_or_else(|| slug.clone());
                    self.close_pane(idx);
                    if let Some(wt) = wt {
                        let root = self.project_root.clone();
                        let wt_path = PathBuf::from(wt);
                        let tx = self.app_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let env = [
                                ("DMUX_WORKTREE_PATH", wt_path.to_string_lossy().into_owned()),
                                ("DMUX_BRANCH", branch.clone()),
                            ];
                            hooks::run_detached(&root, "before_worktree_remove", &root, &env);
                            let _ = git::cleanup_worktree(&root, &wt_path, &branch);
                            hooks::run_detached(&root, "worktree_removed", &root, &env);
                            let _ = tx.send(AppMsg::RefreshDerived);
                        });
                    }
                    self.toast("Worktree merged and cleaned up");
                }
            }
            AppCmd::CreatePr(idx) => {
                let Some(p) = self.panes.get(idx) else {
                    return true;
                };
                let Some(wt) = p.worktree_path.clone() else {
                    return true;
                };
                let wt_path = PathBuf::from(&wt);
                let branch = git::current_branch(&wt_path).unwrap_or_else(|| p.slug.clone());
                if git::worktree_dirty(&wt_path) {
                    self.toast("Uncommitted changes — merge flow can commit them first");
                    return true;
                }
                // Interactive in a pane so gh auth/questions stay visible.
                let slug = numbered_slug(&self.panes, "pr-");
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    slug,
                    display: format!("PR: {branch}"),
                    kind: PaneKind::Shell,
                    agent: None,
                    launch_cmd: Some(format!(
                        "clear; git push -u origin {b} && gh pr create --head {b} --fill; echo; echo '[done — close this pane when finished]'",
                        b = shq(&branch)
                    )),
                    injection: None,
                    worktree_path: Some(wt.clone()),
                    cwd: Some(wt),
                    project_root: None,
                });
            }
            AppCmd::RenamePane { idx, name } => self.rename_pane(idx, name),
            AppCmd::ToggleHidden(idx) => self.toggle_hidden(idx),
            AppCmd::ClosePane(idx) => self.close_pane(idx),
            AppCmd::NewTerminal => self.new_terminal(None),
            AppCmd::NewTerminalInProject { project_root } => self.new_terminal(Some(project_root)),
            AppCmd::PromptAddProject => {
                // Filesystem picker rooted at dmux's launch directory (#32);
                // rename/settings inputs stay simple text fields.
                let start = std::env::current_dir().unwrap_or_else(|_| self.project_root.clone());
                self.views
                    .push_at(Box::new(PathPickerView::new(start)), origin);
                self.dirty = true;
            }
            AppCmd::OpenProjectAt(raw) => {
                let expanded = expand_user_path(&raw);
                if !expanded.is_dir() {
                    self.toast(format!("Not a directory: {}", expanded.display()));
                } else {
                    let name = expanded
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| raw.clone());
                    let root = expanded.to_string_lossy().into_owned();
                    // Register in sidebarProjects (typed, TS-compatible).
                    let exists = self.config.sidebar_projects.iter().any(|e| {
                        e.project_root.trim_end_matches('/') == root.trim_end_matches('/')
                    });
                    if !exists {
                        self.config
                            .sidebar_projects
                            .push(dmux_core::SidebarProjectEntry {
                                project_root: root.clone(),
                                project_name: Some(name.clone()),
                                color_theme: None,
                                color_theme_source: None,
                                extra: serde_json::Map::new(),
                            });
                        self.rebuild_sidebar_groups();
                        self.save_config(audit::Reason::ProjectAdded);
                        self.toast(format!("Added project '{name}'"));
                    }
                    return self.execute_cmd(AppCmd::NewTerminalAt { path: root, name });
                }
            }
            AppCmd::ResumeWorktree { path, slug, agent } => {
                let mode = {
                    let s = self.settings.lock().unwrap();
                    s.get_str("permissionMode").unwrap_or("").to_string()
                };
                // Prefer the exact captured session id when tracking saved one.
                let session_id = self
                    .config
                    .panes
                    .iter()
                    .find(|r| r.slug == slug)
                    .and_then(|r| r.extra.get("agentSessionId"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let cmd = agents::agent(&agent)
                    .and_then(|def| {
                        agents::compose_resume_session(def, session_id.as_deref(), &mode)
                    })
                    .unwrap_or_else(|| agent.clone());
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    display: slug.clone(),
                    slug,
                    kind: PaneKind::Worktree,
                    agent: Some(agent),
                    launch_cmd: Some(format!("clear; {cmd}")),
                    injection: None,
                    worktree_path: Some(path.clone()),
                    cwd: Some(path),
                    project_root: None,
                });
                self.toast("Resuming agent session…");
            }
            AppCmd::RestoreSession => {
                let plans = std::mem::take(&mut self.pending_restore);
                let n = plans.len();
                for plan in plans {
                    match plan {
                        session::RestorePlan::Agent {
                            slug,
                            display,
                            path,
                            agent,
                        } => {
                            let mode = {
                                let s = self.settings.lock().unwrap();
                                s.get_str("permissionMode").unwrap_or("").to_string()
                            };
                            // Exact recorded session when tracked; agent
                            // default resume otherwise (same path as the
                            // welcome resume cards).
                            let session_id = self
                                .config
                                .panes
                                .iter()
                                .find(|r| r.slug == slug)
                                .and_then(|r| r.extra.get("agentSessionId"))
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let cmd = agents::agent(&agent)
                                .and_then(|def| {
                                    agents::compose_resume_session(
                                        def,
                                        session_id.as_deref(),
                                        &mode,
                                    )
                                })
                                .unwrap_or_else(|| agent.clone());
                            self.create_window(NewWindowCtx {
                                bootstrap: None,
                                prompt: String::new(),
                                display,
                                slug,
                                kind: PaneKind::Worktree,
                                agent: Some(agent),
                                launch_cmd: Some(format!("clear; {cmd}")),
                                injection: None,
                                worktree_path: Some(path.clone()),
                                cwd: Some(path),
                                project_root: None,
                            });
                        }
                        session::RestorePlan::Shell {
                            slug,
                            display,
                            cwd,
                            project_root,
                        } => {
                            self.create_window(NewWindowCtx {
                                bootstrap: None,
                                prompt: String::new(),
                                display,
                                slug,
                                kind: PaneKind::Shell,
                                agent: None,
                                launch_cmd: None,
                                injection: None,
                                worktree_path: None,
                                cwd: Some(cwd),
                                project_root,
                            });
                        }
                    }
                }
                self.toast(format!("Restoring {n} pane(s) from the last session…"));
            }
            AppCmd::NewTerminalAt { path, name } => {
                let slug = numbered_slug(&self.panes, "terminal-");
                self.create_window(NewWindowCtx {
                    bootstrap: None,
                    prompt: String::new(),
                    slug,
                    display: name,
                    kind: PaneKind::Shell,
                    agent: None,
                    launch_cmd: None,
                    injection: None,
                    worktree_path: Some(path.clone()),
                    cwd: Some(path.clone()),
                    project_root: (Path::new(&path) != self.project_root.as_path()).then_some(path),
                });
            }
            AppCmd::LaunchAgents {
                prompt,
                allocations,
                mode,
                project_root,
            } => self.launch_agents(prompt, allocations, mode, project_root),
            AppCmd::SetSetting { key, value, scope } => self.set_setting(&key, value, scope),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_paths_expand_home_prefixes_only() {
        let home = dirs_home();
        assert_eq!(expand_user_path("~"), home);
        assert_eq!(expand_user_path("~/proj/x"), home.join("proj/x"));
        assert_eq!(expand_user_path("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_user_path("rel/x"), PathBuf::from("rel/x"));
        // "~user" is NOT expanded — taken literally, matching the old arm.
        assert_eq!(expand_user_path("~other"), PathBuf::from("~other"));
    }

    #[test]
    fn numbered_slugs_count_only_their_own_family() {
        use crate::registry::adopt_panes;
        use crate::session::parse_pane_list;
        use dmux_cc::Reply;
        let reply = Reply {
            lines: [
                "%1\u{1}@1\u{1}terminal-1\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
                "%2\u{1}@2\u{1}terminal-2\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
                "%3\u{1}@3\u{1}editor-1\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
            ]
            .iter()
            .map(|l| l.as_bytes().to_vec())
            .collect(),
            ok: true,
            rtt: std::time::Duration::ZERO,
        };
        let panes = adopt_panes(None, &parse_pane_list(&reply));
        assert_eq!(numbered_slug(&panes, "terminal-"), "terminal-3");
        assert_eq!(numbered_slug(&panes, "editor-"), "editor-2");
        assert_eq!(numbered_slug(&panes, "hook-"), "hook-1");
    }

    #[test]
    fn global_menu_tail_keeps_commands_wired_to_leader_hints() {
        let tail = global_menu_tail();
        let pairs: Vec<(&str, &AppCmd)> = tail.iter().map(|i| (i.hint.as_str(), &i.cmd)).collect();
        assert!(matches!(pairs[0], ("^b n", AppCmd::OpenNewAgent)));
        assert!(matches!(pairs[1], ("^b t", AppCmd::NewTerminal)));
        assert!(matches!(pairs[2], ("^b p", AppCmd::PromptAddProject)));
        assert!(matches!(pairs[3], ("^b s", AppCmd::OpenSettings)));
        assert!(matches!(pairs[4], ("^b l", AppCmd::OpenLogs)));
        assert!(matches!(pairs[5], ("^b ?", AppCmd::OpenShortcuts)));
        assert!(matches!(pairs[6], ("^b d", AppCmd::Quit)));
    }
}
