//! The pane registry (#81): the ONE module that owns the mapping between
//! live tmux pane identity and persisted `dmux.config.json` pane records.
//! Adoption, creation, reassignment, removal, and ordering are the explicit
//! transitions below; window creation, reconciliation, sidebar grouping,
//! and the #78 diagnostic all consume these results instead of re-deriving
//! identity policy.
//!
//! Identity model: a record's stable identity is its `id` field; `slug`
//! is the title-contract handle (agent-controlled titles are parsed, never
//! trusted as identity on their own); `pane_id` (`%N`) is the mutable tmux
//! binding, stale after restarts. The matching ladder — exact pane-id,
//! then slug+cwd-ownership (duplicate slugs across projects, #76), then
//! unique plain slug — is shared by every transition, so a duplicate slug
//! or a mutated title can never associate a live pane with another pane's
//! record (ambiguity yields NO match rather than a guess).
//!
//! Transitions (inputs → outcome):
//! - `adopt_panes(config, snapshot)` → `LogicalPane`s bound to records via
//!   the ladder; unmatched panes recover ownership from their live cwd.
//! - `record_adopted_panes(config, panes, snapshot, seed)` → creates
//!   records for record-less panes and rebinds stale `pane_id`s
//!   (reassignment); returns whether the config changed. Persisting the
//!   result flows through the #79 audit boundary.
//! - `record_has_live_pane` / `remove_pane_record` → removal policy: a
//!   record dies only with its own pane.
//! - `order_records` / `order_panes` / `order_panes_preserving` /
//!   `move_pane` → display order follows persisted record order and
//!   survives reconcile without disturbing focus or selection.
//! - `reusable_record_index` → window creation reuses a record only for
//!   the same (slug, project) identity.

use dmux_core::{parse_pane_title, DmuxConfig, DmuxPane, PaneKind};

use dmux_vt::PaneTerm;

use crate::session::{
    is_infra, is_keepalive, LogicalPane, PaneStatus, TmuxPaneInfo, PANE_SCROLLBACK,
};

/// Move a pane to `dst`'s position in the display order (#26). Refuses
/// cross-project moves (reordering must not silently change ownership) and
/// out-of-range indices; returns whether the order changed. tmux window
/// order is untouched — display order is an application-level concept.
pub fn move_pane(panes: &mut Vec<LogicalPane>, src: usize, dst: usize) -> bool {
    if src == dst || src >= panes.len() || dst >= panes.len() {
        return false;
    }
    if panes[src].project_root != panes[dst].project_root {
        return false;
    }
    let pane = panes.remove(src);
    panes.insert(dst, pane);
    true
}

pub fn same_project(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => canon_root(left) == canon_root(right),
        _ => false,
    }
}

fn unique_index<T>(items: &[T], mut matches: impl FnMut(usize, &T) -> bool) -> Option<usize> {
    let mut indices = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| matches(index, item).then_some(index));
    let first = indices.next()?;
    indices.next().is_none().then_some(first)
}

fn record_matches_pane(record: &DmuxPane, pane: &LogicalPane) -> bool {
    record.slug == pane.slug
        && same_project(record.project_root.as_deref(), pane.project_root.as_deref())
}

pub fn record_has_live_pane(record: &DmuxPane, panes: &[LogicalPane]) -> bool {
    panes.iter().any(|pane| {
        record.pane_id == pane.tmux_pane.to_string() || record_matches_pane(record, pane)
    })
}

/// Persist panes adopted without a record and refresh fallback bindings.
/// Records are appended in current display order so later asynchronous tmux
/// snapshots cannot supply a different order for the same pane identities.
pub fn record_adopted_panes(
    config: &mut DmuxConfig,
    panes: &[LogicalPane],
    infos: &[TmuxPaneInfo],
    id_seed: u64,
) -> bool {
    let mut changed = false;
    let mut next_id = 0_u64;
    let mut used_records = std::collections::HashSet::new();
    for pane in panes {
        let record_index = config
            .panes
            .iter()
            .enumerate()
            .find(|(index, record)| {
                !used_records.contains(index) && record.pane_id == pane.tmux_pane.to_string()
            })
            .map(|(index, _)| index)
            .or_else(|| {
                unique_index(&config.panes, |index, record| {
                    !used_records.contains(&index) && record_matches_pane(record, pane)
                })
            });
        if let Some(record_index) = record_index {
            used_records.insert(record_index);
            let record = &mut config.panes[record_index];
            let pane_id = pane.tmux_pane.to_string();
            if record.pane_id != pane_id {
                tracing::info!(pane = %pane.tmux_pane, slug = %pane.slug,
                    root = ?pane.project_root, "persisting reconciled pane identity");
                record.pane_id = pane_id;
                changed = true;
            }
            continue;
        }

        let mut record = DmuxPane::new_record(
            format!("pane-{id_seed}-{next_id}"),
            pane.slug.clone(),
            pane.tmux_pane.to_string(),
            pane.kind,
        );
        next_id += 1;
        record.display_name = (pane.title != pane.slug).then(|| pane.title.clone());
        record.hidden = pane.hidden.then_some(true);
        record.project_root = pane.project_root.clone();
        record.project_name = pane.project_root.as_deref().map(|root| {
            std::path::Path::new(root)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string())
        });
        record.worktree_path = pane.worktree_path.clone();
        record.agent = pane.agent.clone();
        if pane.kind == PaneKind::Shell {
            record.shell_cwd = infos
                .iter()
                .find(|info| info.pane == pane.tmux_pane)
                .map(|info| info.current_path.clone())
                .filter(|path| !path.is_empty());
        }
        tracing::info!(pane = %pane.tmux_pane, slug = %pane.slug,
            root = ?pane.project_root, "persisting adopted pane identity and order");
        config.panes.push(record);
        used_records.insert(config.panes.len() - 1);
        changed = true;
    }
    changed
}

/// Stable-order config records to match the live display order (#26, #72).
/// Pane IDs are authoritative. Project plus slug is the restart fallback,
/// which keeps duplicate slugs in separate projects from changing places.
pub fn order_records(records: &mut Vec<DmuxPane>, panes: &[LogicalPane]) {
    let mut remaining = std::mem::take(records);
    for pane in panes {
        let record_index = remaining
            .iter()
            .position(|record| record.pane_id == pane.tmux_pane.to_string())
            .or_else(|| unique_index(&remaining, |_, record| record_matches_pane(record, pane)));
        if let Some(record_index) = record_index {
            records.push(remaining.remove(record_index));
        }
    }
    records.append(&mut remaining);
}

/// Stable-order live panes by persisted record identity (#26, #72).
/// Unknown panes keep their relative adoption order after recorded panes.
pub fn order_panes(panes: &mut Vec<LogicalPane>, records: &[DmuxPane]) {
    let mut remaining = std::mem::take(panes);
    for (record_index, record) in records.iter().enumerate() {
        let pane_index = remaining
            .iter()
            .position(|pane| record.pane_id == pane.tmux_pane.to_string())
            .or_else(|| {
                unique_index(&remaining, |_, pane| {
                    let pane_id = pane.tmux_pane.to_string();
                    let another_exact = records.iter().enumerate().any(|(index, candidate)| {
                        index != record_index && candidate.pane_id == pane_id
                    });
                    let unique_fallback =
                        unique_index(records, |_, candidate| record_matches_pane(candidate, pane))
                            == Some(record_index);
                    !another_exact && unique_fallback
                })
            });
        if let Some(pane_index) = pane_index {
            panes.push(remaining.remove(pane_index));
        }
    }
    panes.append(&mut remaining);
}

pub fn remove_pane_record(records: &mut Vec<DmuxPane>, pane: &LogicalPane) -> bool {
    let record_index = records
        .iter()
        .position(|record| record.pane_id == pane.tmux_pane.to_string())
        .or_else(|| unique_index(records, |_, record| record_matches_pane(record, pane)));
    if let Some(record_index) = record_index {
        records.remove(record_index);
        return true;
    }
    false
}

/// Pane identities in display order for order-mutation diagnostics (#72).
pub fn pane_order_identities(panes: &[LogicalPane]) -> Vec<String> {
    panes
        .iter()
        .map(|pane| pane.tmux_pane.to_string())
        .collect()
}

pub fn log_pane_order_change(reason: &'static str, before: &[String], panes: &[LogicalPane]) {
    let after = pane_order_identities(panes);
    if before != after {
        tracing::info!(reason, ?before, ?after, "pane display order changed");
    }
}

/// Decide which tmux panes are content panes and pair them with config
/// entries by slug (via the title contract). Config panes with no live tmux
/// pane are skipped in Phase 0 (recreation is a Phase 1 concern); live panes
/// with no config entry are still adopted (matches TS behavior of showing
/// externally created panes).
/// Canonical filesystem identity for a project root (#76): path aliases
/// (symlinks, trailing slashes, `..`) must land in one project group.
/// Falls back to the trimmed string when the path doesn't resolve.
pub fn canon_root(p: &str) -> String {
    std::fs::canonicalize(p)
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.trim_end_matches('/').to_string())
}

/// Ownership recovery for a pane with no usable record (#76): the most
/// specific configured project whose canonical root contains the pane's
/// live working directory. Distinct roots of equal depth cannot both
/// contain the same cwd, so the longest match is unambiguous; None when no
/// configured project contains it.
pub fn recover_project_root(cwd: &str, roots: &[String]) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let cwd = canon_root(cwd);
    roots
        .iter()
        .filter(|r| {
            let c = canon_root(r);
            cwd == c || cwd.starts_with(&format!("{c}/"))
        })
        .max_by_key(|r| canon_root(r).len())
        .cloned()
}

/// Why a live pane paired with a persisted record — shared by adoption and
/// the #78 session diagnostic so both report the same identity semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchReason {
    /// Exact `pane_id` identity.
    PaneId,
    /// Same slug AND the record's project contains the pane's live cwd
    /// (disambiguates duplicate slugs across projects, #76).
    SlugCwd,
    /// Plain slug fallback.
    Slug,
}

/// Ownership context for a launch target: the target root when it differs
/// from the main project, None when it IS the main project.
pub(crate) fn project_context(
    main_root: &std::path::Path,
    target: Option<String>,
) -> Option<String> {
    target.filter(|root| std::path::Path::new(root) != main_root)
}

/// Record lookup (#76): exact pane-id identity first; then, among same-slug
/// records (slugs repeat across projects), the one whose project contains
/// the pane's live cwd; plain slug last.
pub fn record_match<'c>(
    config: &'c DmuxConfig,
    slug: &str,
    info: &TmuxPaneInfo,
) -> Option<(&'c DmuxPane, MatchReason)> {
    if let Some(record) = config
        .panes
        .iter()
        .find(|p| p.pane_id == info.pane.to_string())
    {
        return Some((record, MatchReason::PaneId));
    }
    let roots: Vec<String> = std::iter::once(config.project_root.clone())
        .chain(
            config
                .sidebar_projects
                .iter()
                .map(|p| p.project_root.clone()),
        )
        .collect();
    if let Some(recovered_root) = recover_project_root(&info.current_path, &roots) {
        let recovered_project = (canon_root(&recovered_root) != canon_root(&config.project_root))
            .then_some(recovered_root);
        return unique_index(&config.panes, |_, record| {
            record.slug == slug
                && same_project(record.project_root.as_deref(), recovered_project.as_deref())
        })
        .map(|index| (&config.panes[index], MatchReason::SlugCwd));
    }
    config
        .panes
        .iter()
        .find(|record| record.slug == slug)
        .map(|record| (record, MatchReason::Slug))
}

pub fn adopt_panes(config: Option<&DmuxConfig>, infos: &[TmuxPaneInfo]) -> Vec<LogicalPane> {
    // Configured project roots for cwd-based ownership recovery (#76).
    let known_roots: Vec<String> = config
        .map(|c| {
            std::iter::once(c.project_root.clone())
                .chain(c.sidebar_projects.iter().map(|p| p.project_root.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut adopted = Vec::new();
    let mut used_records = std::collections::HashSet::new();
    for info in infos {
        if is_infra(&info.title, &info.window_name) || is_keepalive(info) {
            continue;
        }
        let parsed = parse_pane_title(&info.title);
        let recovered_root = recover_project_root(&info.current_path, &known_roots);
        let recovered_project = recovered_root.clone().filter(|root| {
            config
                .map(|c| canon_root(root) != canon_root(&c.project_root))
                .unwrap_or(true)
        });
        // Each record binds once. Cwd ownership suppresses cross-project
        // slug fallback when the matching project has no saved record.
        let config_pane_index = config.and_then(|c| {
            c.panes
                .iter()
                .enumerate()
                .find(|(index, pane)| {
                    !used_records.contains(index) && pane.pane_id == info.pane.to_string()
                })
                .map(|(index, _)| index)
                .or_else(|| {
                    recovered_root.as_ref().and_then(|_| {
                        unique_index(&c.panes, |index, pane| {
                            !used_records.contains(&index)
                                && pane.slug == parsed.slug
                                && same_project(
                                    pane.project_root.as_deref(),
                                    recovered_project.as_deref(),
                                )
                        })
                    })
                })
                .or_else(|| {
                    if recovered_root.is_none() {
                        unique_index(&c.panes, |index, pane| {
                            !used_records.contains(&index) && pane.slug == parsed.slug
                        })
                    } else {
                        None
                    }
                })
        });
        if let Some(index) = config_pane_index {
            used_records.insert(index);
        }
        let config_pane = config_pane_index.and_then(|index| config.map(|c| &c.panes[index]));
        let slug = config_pane
            .map(|p| p.slug.clone())
            .unwrap_or_else(|| parsed.slug.clone());
        let title = config_pane
            .map(|p| p.display_title().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                if parsed.display.trim().is_empty() {
                    info.window_name.clone()
                } else {
                    parsed.display.clone()
                }
            });
        adopted.push(LogicalPane {
            slug,
            title,
            kind: config_pane.map(|p| p.kind()).unwrap_or(PaneKind::Worktree),
            tmux_pane: info.pane,
            tmux_window: info.window,
            cols: info.width.max(1),
            rows: info.height.max(1),
            term: PaneTerm::new(info.width.max(1), info.height.max(1), PANE_SCROLLBACK),
            rect: None,
            paused: false,
            reseed_buffer: None,
            closing: false,
            pending_seed: None,
            dirty: true,
            status: PaneStatus::Idle,
            last_output: None,
            window_bytes: 0,
            window_start: std::time::Instant::now(),
            throttled: false,
            resume_at: None,
            hidden: config_pane.map(|p| p.is_hidden()).unwrap_or(false),
            needs_attention: false,
            auto_name: config_pane
                .map(|p| p.kind() == PaneKind::Shell && p.display_name.is_none())
                .unwrap_or(true),
            llm_named: false,
            llm_named_at: None,
            engine: dmux_status::PaneStatusEngine::new(),
            analysis_inflight: false,
            record_stream: false,
            recent_output: Vec::new(),
            ring_truncated: false,
            last_verify: None,
            pending_verify: None,
            reseed_count: 0,
            pending_boundary_resync: false,
            issue_filed: false,
            worktree_path: config_pane.and_then(|p| p.worktree_path.clone()),
            alt_screen: info.alternate_on,
            extended_keys_mode2: info.extended_keys_mode2,
            pane_pid: info.pane_pid,
            project_root: config_pane
                .and_then(|p| p.project_root.clone())
                .or_else(|| {
                    if let Some(r) = &recovered_root {
                        tracing::info!(pane = %info.pane, root = %r, cwd = %info.current_path,
                        "ownership recovered from cwd for unmatched pane");
                    }
                    recovered_project
                }),
            agent: config_pane.and_then(|p| p.agent.clone()),
            current_command: info.current_command.clone(),
        });
    }
    adopted
}

/// Ordering transition for reconciliation (#81): apply persisted record
/// order to the live panes while preserving which pane holds focus and
/// selection. Returns the new (focused, selected) indices.
pub fn order_panes_preserving(
    panes: &mut Vec<LogicalPane>,
    records: &[DmuxPane],
    focused: usize,
    selected: usize,
) -> (usize, usize) {
    let focused_id = panes.get(focused).map(|p| p.tmux_pane);
    let selected_id = panes.get(selected).map(|p| p.tmux_pane);
    order_panes(panes, records);
    let find = |id: Option<dmux_cc::PaneId>, fallback: usize| {
        id.and_then(|id| panes.iter().position(|p| p.tmux_pane == id))
            .unwrap_or(fallback)
    };
    (find(focused_id, focused), find(selected_id, selected))
}

/// Window creation reuses an existing record only when BOTH the slug and
/// the project identity match (#76): a reused slug in another project must
/// get its own record, never repoint this one.
pub fn reusable_record_index(
    records: &[DmuxPane],
    slug: &str,
    project_root: Option<&str>,
) -> Option<usize> {
    records
        .iter()
        .position(|r| r.slug == slug && same_project(r.project_root.as_deref(), project_root))
}

#[cfg(test)]
mod tests;
