//! Pane-record mutation audit (#79): every config save diffs the persisted
//! pane records against the last-saved snapshot and logs one structured
//! line per mutation (target `pane_audit`, in the ordinary dmux log), with
//! a typed reason naming the user action or state transition responsible.
//! The diff lives at the persistence boundary so every current and future
//! call site gets identical coverage; a save with no pane-record changes
//! emits no record-level entries. Redaction is structural: only record id,
//! pane id, slug, project root, and display order are ever captured —
//! prompts, pane output, launch commands, and unrelated config values never
//! reach the snapshot.

use dmux_core::DmuxPane;

/// Why a config save happened — the user action or state transition.
#[derive(Clone, Debug)]
pub enum Reason {
    /// A new window/terminal launch created or rebound a record.
    PaneLaunched,
    /// The user closed a pane; its own record goes with it.
    PaneClosed,
    /// Reconciliation against the live tmux pane set; removals carry the
    /// live identities that authorized them.
    Reconcile { live: Vec<String> },
    /// Sidebar drag reorder (#26).
    Reorder,
    /// The user renamed a pane.
    Rename,
    /// The user hid or unhid a pane.
    Visibility,
    /// Background agent-tracking metadata refresh.
    AgentTracking,
    /// A sidebar project was registered.
    ProjectAdded,
    /// Automatic project color-theme assignment persisted.
    ProjectTheme,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Reason::PaneLaunched => "pane-launched",
            Reason::PaneClosed => "pane-closed",
            Reason::Reconcile { .. } => "reconcile",
            Reason::Reorder => "reorder",
            Reason::Rename => "rename",
            Reason::Visibility => "visibility",
            Reason::AgentTracking => "agent-tracking",
            Reason::ProjectAdded => "project-added",
            Reason::ProjectTheme => "project-theme",
        };
        f.write_str(s)
    }
}

/// The audited subset of one pane record. Constructing a `Snap` is the
/// redaction boundary: nothing else from the record is retained.
#[derive(Clone, Debug, PartialEq)]
pub struct Snap {
    pub id: String,
    pub pane_id: String,
    pub slug: String,
    pub project_root: Option<String>,
}

pub fn snapshot(panes: &[DmuxPane]) -> Vec<Snap> {
    panes
        .iter()
        .map(|p| Snap {
            id: p.id.clone(),
            pane_id: p.pane_id.clone(),
            slug: p.slug.clone(),
            project_root: p.project_root.clone(),
        })
        .collect()
}

fn root(r: &Option<String>) -> &str {
    r.as_deref().unwrap_or("-")
}

/// One line per mutation between two snapshots, keyed by record id:
/// additions, removals, pane-id reassignment, slug changes, project
/// ownership changes, and ordering changes. Pure over its inputs.
pub fn diff_events(old: &[Snap], new: &[Snap], reason: &Reason) -> Vec<String> {
    let mut events = Vec::new();
    for n in new {
        match old.iter().find(|o| o.id == n.id) {
            None => events.push(format!(
                "record-added id={} pane={} slug={} root={} reason={reason}",
                n.id,
                n.pane_id,
                n.slug,
                root(&n.project_root)
            )),
            Some(o) => {
                if o.pane_id != n.pane_id {
                    events.push(format!(
                        "pane-reassigned id={} slug={} pane={}→{} reason={reason}",
                        n.id, n.slug, o.pane_id, n.pane_id
                    ));
                }
                if o.slug != n.slug {
                    events.push(format!(
                        "slug-changed id={} pane={} slug={}→{} reason={reason}",
                        n.id, n.pane_id, o.slug, n.slug
                    ));
                }
                if o.project_root != n.project_root {
                    events.push(format!(
                        "root-changed id={} slug={} root={}→{} reason={reason}",
                        n.id,
                        n.slug,
                        root(&o.project_root),
                        root(&n.project_root)
                    ));
                }
            }
        }
    }
    for o in old {
        if !new.iter().any(|n| n.id == o.id) {
            let authorized = match reason {
                Reason::Reconcile { live } => format!(" live=[{}]", live.join(",")),
                _ => String::new(),
            };
            events.push(format!(
                "record-removed id={} pane={} slug={} root={} reason={reason}{authorized}",
                o.id,
                o.pane_id,
                o.slug,
                root(&o.project_root)
            ));
        }
    }
    // Ordering: only when the surviving set is otherwise unchanged in
    // membership — additions/removals already explain sequence shifts.
    let old_ids: Vec<&str> = old.iter().map(|s| s.id.as_str()).collect();
    let new_ids: Vec<&str> = new.iter().map(|s| s.id.as_str()).collect();
    if events.is_empty() && old_ids != new_ids {
        let order: Vec<&str> = new.iter().map(|s| s.slug.as_str()).collect();
        events.push(format!(
            "order-changed order=[{}] reason={reason}",
            order.join(",")
        ));
    }
    events
}

/// Persistence-boundary hook: log every mutation since `base` and return
/// the new baseline. No pane-record changes → no entries.
pub fn log_and_advance(base: &[Snap], panes: &[DmuxPane], reason: &Reason) -> Vec<Snap> {
    let new = snapshot(panes);
    for line in diff_events(base, &new, reason) {
        tracing::info!(target: "pane_audit", "{line}");
    }
    new
}

impl crate::App {
    pub(crate) fn update_config_pane(
        &mut self,
        slug: &str,
        reason: Reason,
        f: impl FnOnce(&mut DmuxPane),
    ) {
        if let Some(rec) = self.config.panes.iter_mut().find(|p| p.slug == slug) {
            f(rec);
        } else {
            return;
        }
        self.save_config(reason);
    }

    /// The single persistence boundary for the project config: audits the
    /// pane-record diff (#79), stamps lastUpdated, and writes the file.
    pub(crate) fn save_config(&mut self, reason: Reason) {
        self.audit_base = log_and_advance(&self.audit_base, &self.config.panes, &reason);
        if let Some(obj) = self.config.extra.get_mut("lastUpdated") {
            *obj = serde_json::Value::String(crate::iso_now());
        } else {
            self.config.extra.insert(
                "lastUpdated".into(),
                serde_json::Value::String(crate::iso_now()),
            );
        }
        match self.config.save(&self.config_path) {
            Ok(()) => {
                self.config_persisted = true;
            }
            Err(err) => tracing::warn!(%err, "config save failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, pane_id: &str, slug: &str, prj: Option<&str>) -> DmuxPane {
        let mut p = DmuxPane::new_record(
            id.to_string(),
            slug.to_string(),
            pane_id.to_string(),
            dmux_core::PaneKind::Shell,
        );
        p.prompt = "SECRET-PROMPT".into();
        p.project_root = prj.map(str::to_string);
        p
    }

    #[test]
    fn creation_close_and_reconcile_removal_are_reasoned() {
        let a = snapshot(&[pane("1", "%1", "term-1", Some("/a"))]);
        // Creation.
        let ev = diff_events(&[], &a, &Reason::PaneLaunched);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert!(
            ev[0].contains("record-added id=1 pane=%1 slug=term-1 root=/a reason=pane-launched"),
            "{}",
            ev[0]
        );
        // User close.
        let ev = diff_events(&a, &[], &Reason::PaneClosed);
        assert!(ev[0].contains("record-removed") && ev[0].contains("reason=pane-closed"));
        assert!(!ev[0].contains("live="), "{}", ev[0]);
        // Reconciliation removal names the live identities that authorized it.
        let ev = diff_events(
            &a,
            &[],
            &Reason::Reconcile {
                live: vec!["%7".into(), "%9".into()],
            },
        );
        assert!(ev[0].contains("reason=reconcile live=[%7,%9]"), "{}", ev[0]);
    }

    #[test]
    fn reorder_reassignment_slug_reuse_and_ownership_moves() {
        let old = snapshot(&[
            pane("1", "%1", "aa", Some("/a")),
            pane("2", "%2", "bb", Some("/a")),
        ]);
        // Pure reorder: membership identical, sequence flipped.
        let flipped = vec![old[1].clone(), old[0].clone()];
        let ev = diff_events(&old, &flipped, &Reason::Reorder);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert!(ev[0].contains("order-changed order=[bb,aa] reason=reorder"));
        // Pane-id reassignment (rebind after restart) + slug reuse + move.
        let new = snapshot(&[
            pane("1", "%9", "aa", Some("/a")),
            pane("2", "%2", "cc", Some("/b")),
        ]);
        let ev = diff_events(&old, &new, &Reason::PaneLaunched);
        assert!(
            ev.iter()
                .any(|e| e.contains("pane-reassigned id=1 slug=aa pane=%1→%9")),
            "{ev:?}"
        );
        assert!(
            ev.iter()
                .any(|e| e.contains("slug-changed id=2 pane=%2 slug=bb→cc")),
            "{ev:?}"
        );
        assert!(
            ev.iter()
                .any(|e| e.contains("root-changed id=2 slug=cc root=/a→/b")),
            "{ev:?}"
        );
    }

    #[test]
    fn snapshots_are_structurally_redacted_and_quiet_when_unchanged() {
        let panes = [pane("1", "%1", "aa", Some("/a"))];
        let snap = snapshot(&panes);
        // The prompt never reaches the snapshot or any event text.
        let ev = diff_events(&[], &snap, &Reason::PaneLaunched);
        assert!(ev.iter().all(|e| !e.contains("SECRET-PROMPT")), "{ev:?}");
        // Identical snapshots produce zero record-level entries.
        assert!(diff_events(&snap, &snap, &Reason::ProjectTheme).is_empty());
    }
}
