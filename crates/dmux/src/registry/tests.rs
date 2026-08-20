//! Registry transition tests (#81): identity matching, ownership
//! recovery, duplicate-slug resolution, reordering, reconciliation
//! persistence, and record-reuse policy.

use super::*;
use crate::session::parse_pane_list;
use dmux_cc::{PaneId, Reply, WindowId};

fn reply_of(lines: &[&str]) -> Reply {
    Reply {
        lines: lines.iter().map(|l| l.as_bytes().to_vec()).collect(),
        ok: true,
        rtt: std::time::Duration::ZERO,
    }
}

#[test]
fn ownership_recovers_from_cwd_most_specific_and_canonical() {
    // #76: containment picks the most specific configured project;
    // aliases of one directory resolve to one identity; unknown cwds
    // recover nothing.
    let t = std::env::temp_dir().join(format!("dmux-own-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    std::fs::create_dir_all(t.join("outer/inner/deep")).unwrap();
    std::fs::create_dir_all(t.join("outer/x")).unwrap();
    let outer = t.join("outer").to_string_lossy().into_owned();
    let inner = t.join("outer/inner").to_string_lossy().into_owned();
    let roots = vec![outer.clone(), inner.clone()];
    let deep = t.join("outer/inner/deep").to_string_lossy().into_owned();
    assert_eq!(recover_project_root(&deep, &roots), Some(inner.clone()));
    assert_eq!(
        recover_project_root(&t.join("outer/x").to_string_lossy(), &roots),
        Some(outer.clone())
    );
    assert_eq!(recover_project_root("/nowhere/else", &roots), None);
    assert_eq!(recover_project_root("", &roots), None);
    // Path aliases: trailing slash and a symlink share one canon id.
    assert_eq!(canon_root(&format!("{outer}/")), canon_root(&outer));
    let link = t.join("link");
    let _ = std::os::unix::fs::symlink(t.join("outer"), &link);
    assert_eq!(canon_root(&link.to_string_lossy()), canon_root(&outer));
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
fn duplicate_slugs_resolve_by_pane_id_then_cwd() {
    // #76: the same slug exists in two projects; the pane's live cwd
    // picks the right record, and an exact pane-id match wins outright.
    let t = std::env::temp_dir().join(format!("dmux-dup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    std::fs::create_dir_all(t.join("proj-a")).unwrap();
    std::fs::create_dir_all(t.join("proj-b")).unwrap();
    let a = t.join("proj-a").to_string_lossy().into_owned();
    let b = t.join("proj-b").to_string_lossy().into_owned();
    let config: DmuxConfig = serde_json::from_value(serde_json::json!({
        "projectName": "main",
        "projectRoot": t.to_string_lossy(),
        "sidebarProjects": [{"projectRoot": a}, {"projectRoot": b}],
        "panes": [
            {"id":"1","slug":"terminal-5","prompt":"","paneId":"%42","type":"shell",
             "projectRoot": a, "displayName":"a-term"},
            {"id":"2","slug":"terminal-5","prompt":"","paneId":"%43","type":"shell",
             "projectRoot": b, "displayName":"b-term"}
        ]
    }))
    .unwrap();
    let mk = |pane: u32, cwd: &str| TmuxPaneInfo {
        pane: PaneId(pane),
        window: WindowId(pane),
        title: "terminal-5".into(),
        width: 80,
        height: 24,
        alternate_on: false,
        current_command: "zsh".into(),
        window_name: "w".into(),
        pane_pid: 1,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: cwd.into(),
    };
    // Live cwd in proj-b picks b's record even though a's sorts first.
    let adopted = adopt_panes(Some(&config), &[mk(99, &b)]);
    assert_eq!(adopted[0].project_root.as_deref(), Some(b.as_str()));
    assert_eq!(adopted[0].title, "b-term");
    // Exact pane-id identity beats everything.
    let adopted = adopt_panes(Some(&config), &[mk(42, &b)]);
    assert_eq!(adopted[0].project_root.as_deref(), Some(a.as_str()));
    // Unmatched slug entirely: ownership recovered from cwd alone.
    let mut lone = mk(7, &a);
    lone.title = "mystery".into();
    let adopted = adopt_panes(Some(&config), &[lone]);
    assert_eq!(adopted[0].project_root.as_deref(), Some(a.as_str()));
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
fn reorder_moves_within_project_and_persists() {
    // Three panes: two in the main project, one owned by another
    // project; a hidden pane reorders like any other (#26).
    let reply = reply_of(&[
        "%1\u{1}@1\u{1}p__aa__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
        "%2\u{1}@2\u{1}p__bb__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
        "%3\u{1}@3\u{1}p__cc__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
    ]);
    let mut panes = adopt_panes(None, &parse_pane_list(&reply));
    assert_eq!(panes.len(), 3);
    panes[1].hidden = true;
    panes[2].project_root = Some("/other".into());
    let slugs = |p: &[LogicalPane]| p.iter().map(|x| x.slug.clone()).collect::<Vec<_>>();

    // Hidden pane moves fine within its project.
    assert!(move_pane(&mut panes, 1, 0));
    assert_eq!(slugs(&panes), ["p__bb__p", "p__aa__p", "p__cc__p"]);
    assert!(panes[0].hidden, "hidden state rides along");

    // Cross-project moves are refused and change nothing.
    assert!(!move_pane(&mut panes, 2, 0));
    assert_eq!(slugs(&panes), ["p__bb__p", "p__aa__p", "p__cc__p"]);
    // Out-of-range and no-op moves are refused.
    assert!(!move_pane(&mut panes, 0, 9));
    assert!(!move_pane(&mut panes, 1, 1));

    // Persistence round trip: records follow the live order; unknown
    // records keep their relative order at the end; adoption ordering
    // restores the live order from records.
    let mut records: Vec<dmux_core::DmuxPane> = ["p__aa__p", "p__bb__p", "zz", "p__cc__p"]
        .iter()
        .map(|slug| {
            serde_json::from_value(serde_json::json!({
                "id": *slug, "slug": *slug, "prompt": "",
                "paneId": match *slug {
                    "p__aa__p" => "%1",
                    "p__bb__p" => "%2",
                    "p__cc__p" => "%3",
                    _ => "%9",
                },
                "projectRoot": (*slug == "p__cc__p").then_some("/other")
            }))
            .unwrap()
        })
        .collect();
    order_records(&mut records, &panes);
    let rec_slugs: Vec<&str> = records.iter().map(|r| r.slug.as_str()).collect();
    assert_eq!(rec_slugs, ["p__bb__p", "p__aa__p", "p__cc__p", "zz"]);

    // A fresh adoption (tmux order) re-sorts to the persisted order.
    let mut readopted = adopt_panes(None, &parse_pane_list(&reply));
    order_panes(&mut readopted, &records);
    assert_eq!(slugs(&readopted), ["p__bb__p", "p__aa__p", "p__cc__p"]);
}

#[test]
fn asynchronous_reconcile_persists_identity_order_with_duplicate_slugs() {
    let temp = std::env::temp_dir().join(format!("dmux-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(temp.join("a")).unwrap();
    std::fs::create_dir_all(temp.join("b")).unwrap();
    let a = temp.join("a").to_string_lossy().into_owned();
    let b = temp.join("b").to_string_lossy().into_owned();
    let mut config: DmuxConfig = serde_json::from_value(serde_json::json!({
        "projectName": "main",
        "projectRoot": temp.to_string_lossy(),
        "sidebarProjects": [{"projectRoot": a}, {"projectRoot": b}],
        "panes": [
            {"id":"b","slug":"shared","prompt":"","paneId":"%2","type":"shell",
             "projectRoot": b},
            {"id":"a","slug":"shared","prompt":"","paneId":"%1","type":"shell",
             "projectRoot": a}
        ]
    }))
    .unwrap();
    let info = |pane: u32, title: &str, cwd: &str| TmuxPaneInfo {
        pane: PaneId(pane),
        window: WindowId(pane),
        title: title.into(),
        width: 80,
        height: 24,
        alternate_on: false,
        current_command: "zsh".into(),
        window_name: "w".into(),
        pane_pid: pane,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: cwd.into(),
    };

    // A reconcile reply arrives in tmux order. The unrecorded pane is
    // recovered from its cwd and persisted after the existing identities.
    let first_infos = vec![
        info(1, "shared", &a),
        info(3, "new", &b),
        info(2, "shared", &b),
    ];
    let mut first = adopt_panes(Some(&config), &first_infos);
    assert!(record_adopted_panes(&mut config, &first, &first_infos, 42));
    order_panes(&mut first, &config.panes);
    assert_eq!(
        pane_order_identities(&first),
        ["%2".to_string(), "%1".to_string(), "%3".to_string()]
    );
    assert_eq!(first[0].project_root.as_deref(), Some(b.as_str()));
    assert_eq!(first[1].project_root.as_deref(), Some(a.as_str()));
    assert_eq!(config.panes[2].project_root.as_deref(), Some(b.as_str()));

    // A later asynchronous snapshot uses a different enumeration order.
    // Persisted pane identity keeps the display order unchanged.
    let next_infos = vec![
        info(3, "new", &b),
        info(2, "shared", &b),
        info(1, "shared", &a),
    ];
    let mut next = adopt_panes(Some(&config), &next_infos);
    assert!(!record_adopted_panes(&mut config, &next, &next_infos, 43));
    order_panes(&mut next, &config.panes);
    assert_eq!(pane_order_identities(&next), pane_order_identities(&first));

    // A reused slug whose project record is missing remains associated with
    // its cwd. The remaining project's record stays bound to its pane.
    let mut missing_config = config.clone();
    missing_config
        .panes
        .retain(|record| record.project_root.as_deref() == Some(a.as_str()));
    let collision_infos = vec![info(9, "shared", &b), info(1, "shared", &a)];
    let mut collision = adopt_panes(Some(&missing_config), &collision_infos);
    assert_eq!(collision[0].project_root.as_deref(), Some(b.as_str()));
    assert!(record_adopted_panes(
        &mut missing_config,
        &collision,
        &collision_infos,
        44
    ));
    order_panes(&mut collision, &missing_config.panes);
    assert_eq!(
        pane_order_identities(&collision),
        ["%1".to_string(), "%9".to_string()]
    );
    let mut stale_records = missing_config.panes.clone();
    let mut stale = stale_records[1].clone();
    stale.id = "stale".into();
    stale.pane_id = "%99".into();
    stale_records.insert(0, stale);
    collision.reverse();
    order_panes(&mut collision, &stale_records);
    assert_eq!(
        pane_order_identities(&collision),
        ["%1".to_string(), "%9".to_string()]
    );

    // Canonical project identity allows an aliased record to close with its
    // pane while retaining any unrelated records.
    let mut alias_records: Vec<DmuxPane> = vec![serde_json::from_value(serde_json::json!({
        "id": "alias", "slug": "shared", "prompt": "", "paneId": "%99",
        "projectRoot": format!("{a}/")
    }))
    .unwrap()];
    assert!(remove_pane_record(&mut alias_records, &first[1]));
    assert!(alias_records.is_empty());
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn project_context_omits_the_main_root() {
    use std::path::Path;
    assert_eq!(
        project_context(Path::new("/active"), Some("/empty".into())).as_deref(),
        Some("/empty")
    );
    assert_eq!(
        project_context(Path::new("/active"), Some("/active".into())),
        None
    );
}

fn info_of(pane: u32, title: &str, cwd: &str) -> TmuxPaneInfo {
    TmuxPaneInfo {
        pane: PaneId(pane),
        window: WindowId(pane),
        title: title.into(),
        width: 80,
        height: 24,
        alternate_on: false,
        current_command: "zsh".into(),
        window_name: "w".into(),
        pane_pid: 1,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: cwd.into(),
    }
}

#[test]
fn mutated_titles_cannot_steal_another_panes_record() {
    // An agent retitles its pane to mimic another pane's slug. The victim's
    // record must stay with the pane-id it is bound to; the imposter binds
    // nothing (ambiguity yields no match, never a guess).
    let config: DmuxConfig = serde_json::from_value(serde_json::json!({
        "projectName": "main",
        "projectRoot": "/main",
        "panes": [
            {"id":"1","slug":"victim","prompt":"","paneId":"%1","type":"shell"},
            {"id":"2","slug":"other","prompt":"","paneId":"%2","type":"shell"}
        ]
    }))
    .unwrap();
    let victim = info_of(1, "victim", "");
    let imposter = info_of(2, "victim", ""); // %2 retitled to "victim"
    let adopted = adopt_panes(Some(&config), &[victim, imposter]);
    // %1 keeps its own record; %2 matched its own record by pane id too —
    // the mutated title did not repoint anything.
    assert_eq!(adopted[0].slug, "victim");
    assert_eq!(adopted[1].slug, "other");
    // A third pane with the stolen title and no pane-id binding: the slug
    // is taken by a bound record, so it must not adopt the victim's record.
    let stranger = info_of(3, "victim", "");
    let adopted = adopt_panes(Some(&config), &[info_of(1, "victim", ""), stranger]);
    assert_eq!(adopted[1].slug, "victim"); // parsed slug retained…
    assert_eq!(adopted[1].kind, PaneKind::Worktree); // …but no record bound
}

#[test]
fn reconcile_is_idempotent_across_repeated_snapshots() {
    // Concurrent/repeated reconciliation: running the persistence
    // transition twice over the same snapshot must not grow or mutate
    // the records after the first pass.
    let mut config: DmuxConfig = serde_json::from_value(serde_json::json!({
        "projectName": "main", "projectRoot": "/main", "panes": []
    }))
    .unwrap();
    let infos = vec![
        info_of(1, "p__aa__p", "/main"),
        info_of(2, "p__bb__p", "/main"),
    ];
    let panes = adopt_panes(Some(&config), &infos);
    assert!(record_adopted_panes(&mut config, &panes, &infos, 7));
    let snapshot = serde_json::to_string(
        &config
            .panes
            .iter()
            .map(|r| (&r.id, &r.slug, &r.pane_id))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    // Second pass over identical state: no change reported, no drift.
    let panes = adopt_panes(Some(&config), &infos);
    assert!(!record_adopted_panes(&mut config, &panes, &infos, 8));
    let after = serde_json::to_string(
        &config
            .panes
            .iter()
            .map(|r| (&r.id, &r.slug, &r.pane_id))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(snapshot, after);
}

#[test]
fn self_update_reload_rebinds_records_by_fresh_pane_ids() {
    // After a self-update (or tmux restart) pane ids change; adoption binds
    // by slug and the persistence transition rebinds pane_id — the record's
    // stable id survives (reassignment, not removal + creation).
    let mut config: DmuxConfig = serde_json::from_value(serde_json::json!({
        "projectName": "main", "projectRoot": "/main",
        "panes": [{"id":"stable-1","slug":"aa","prompt":"","paneId":"%3","type":"shell"}]
    }))
    .unwrap();
    let infos = vec![info_of(9, "aa", "")]; // fresh pane id after reload
    let panes = adopt_panes(Some(&config), &infos);
    assert_eq!(panes[0].slug, "aa");
    assert!(record_adopted_panes(&mut config, &panes, &infos, 1));
    assert_eq!(config.panes.len(), 1, "reassignment, not duplication");
    assert_eq!(config.panes[0].id, "stable-1");
    assert_eq!(config.panes[0].pane_id, "%9");
}

#[test]
fn ordering_transition_preserves_focus_and_selection_identity() {
    let records: Vec<DmuxPane> = ["aa", "bb", "cc"]
        .iter()
        .enumerate()
        .map(|(i, slug)| {
            serde_json::from_value(serde_json::json!({
                "id": format!("r{i}"), "slug": slug, "prompt": "",
                "paneId": format!("%{}", i + 1), "type": "shell"
            }))
            .unwrap()
        })
        .collect();
    // Live panes arrive in tmux order cc, aa, bb; focus on aa, select bb.
    let reply = reply_of(&[
        "%3\u{1}@3\u{1}p__cc__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
        "%1\u{1}@1\u{1}p__aa__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
        "%2\u{1}@2\u{1}p__bb__p\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w",
    ]);
    let mut panes = adopt_panes(None, &parse_pane_list(&reply));
    let (focused, selected) = order_panes_preserving(&mut panes, &records, 1, 2);
    assert_eq!(panes[0].slug, "p__aa__p");
    assert_eq!(
        panes[focused].tmux_pane,
        PaneId(1),
        "focus follows identity"
    );
    assert_eq!(
        panes[selected].tmux_pane,
        PaneId(2),
        "selection follows identity"
    );
}

#[test]
fn launch_reuses_records_only_within_the_same_project() {
    let records: Vec<DmuxPane> = vec![serde_json::from_value(serde_json::json!({
        "id": "1", "slug": "terminal-1", "prompt": "", "paneId": "%1",
        "type": "shell", "projectRoot": "/proj-a"
    }))
    .unwrap()];
    assert_eq!(
        reusable_record_index(&records, "terminal-1", Some("/proj-a")),
        Some(0)
    );
    // Same slug, different project: no reuse (#76).
    assert_eq!(
        reusable_record_index(&records, "terminal-1", Some("/proj-b")),
        None
    );
    assert_eq!(reusable_record_index(&records, "terminal-1", None), None);
}
