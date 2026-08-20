//! Read-only live-session diagnostic (#78): one command joins the installed
//! build, recent attach/reload events, live tmux pane metadata, and the
//! persisted pane records — using the SAME identity semantics as adoption
//! (session::record_match) — so ownership investigations (#72, #76) don't
//! reconstruct the picture by hand. Performs no tmux, config, process, git,
//! or filesystem mutations; emits decoded metadata only (no pane contents,
//! prompts, or environment values).

use std::collections::HashMap;
use std::path::Path;

use dmux_core::{parse_pane_title, DmuxConfig};

use crate::session::{self, MatchReason, TmuxPaneInfo};

/// One classified live pane, ready to print.
pub struct PaneReport {
    pub line: String,
}

/// Classify every live pane against the persisted records. Pure over its
/// inputs so fixtures can drive it (#78 tests).
pub fn classify_panes(config: Option<&DmuxConfig>, infos: &[TmuxPaneInfo]) -> Vec<PaneReport> {
    let mut slug_counts: HashMap<&str, usize> = HashMap::new();
    if let Some(c) = config {
        for r in &c.panes {
            *slug_counts.entry(r.slug.as_str()).or_default() += 1;
        }
    }
    infos
        .iter()
        .map(|info| {
            let parsed = parse_pane_title(&info.title);
            let (record, reason) = match config.and_then(|c| session::record_match(c, &parsed.slug, info)) {
                Some((r, why)) => (Some(r), Some(why)),
                None => (None, None),
            };
            let slug = record.map(|r| r.slug.as_str()).unwrap_or(parsed.slug.as_str());
            let saved_root = record.and_then(|r| r.project_root.as_deref());
            let live_root = saved_root.map(session::canon_root).or_else(|| {
                config.and_then(|c| {
                    let roots: Vec<String> = std::iter::once(c.project_root.clone())
                        .chain(c.sidebar_projects.iter().map(|p| p.project_root.clone()))
                        .collect();
                    session::recover_project_root(&info.current_path, &roots)
                        .map(|r| session::canon_root(&r))
                })
            });
            let mut flags = Vec::new();
            match reason {
                Some(MatchReason::PaneId) => flags.push("match=pane-id".to_string()),
                Some(MatchReason::SlugCwd) => flags.push("match=slug+cwd".to_string()),
                Some(MatchReason::Slug) => {
                    flags.push("match=slug".to_string());
                    if slug_counts.get(slug).copied().unwrap_or(0) > 1 {
                        flags.push("AMBIGUOUS(duplicate-slug)".to_string());
                    }
                }
                None => flags.push("UNMATCHED".to_string()),
            }
            if let (Some(r), Some(_)) = (record, reason) {
                if r.pane_id != info.pane.to_string() {
                    flags.push(format!("STALE(record-pane={})", r.pane_id));
                }
            }
            let line = format!(
                "{pane} slug={slug} window={win} cwd={cwd} start={start} saved-root={saved} live-root={live} {flags}",
                pane = info.pane,
                slug = slug,
                win = info.window,
                cwd = session::canon_root(&info.current_path),
                start = if info.start_command.is_empty() { "-" } else { &info.start_command },
                saved = saved_root.map(session::canon_root).unwrap_or_else(|| "-".into()),
                live = live_root.unwrap_or_else(|| "-".into()),
                flags = flags.join(" "),
            );
            PaneReport { line }
        })
        .collect()
}

/// Persisted records with no live pane at all: stale metadata candidates.
pub fn stale_records(config: Option<&DmuxConfig>, infos: &[TmuxPaneInfo]) -> Vec<String> {
    let Some(c) = config else { return Vec::new() };
    let live_ids: Vec<String> = infos.iter().map(|i| i.pane.to_string()).collect();
    let live_slugs: Vec<String> = infos
        .iter()
        .map(|i| parse_pane_title(&i.title).slug)
        .collect();
    c.panes
        .iter()
        .filter(|r| !live_ids.contains(&r.pane_id) && !live_slugs.contains(&r.slug))
        .map(|r| {
            format!(
                "record slug={} pane={} root={} — NO LIVE PANE",
                r.slug,
                r.pane_id,
                r.project_root
                    .as_deref()
                    .map(session::canon_root)
                    .unwrap_or_else(|| "-".into())
            )
        })
        .collect()
}

/// Entry point for `dmux-rs --diagnose-session` (read-only).
pub fn run(
    config: Option<&DmuxConfig>,
    project_root: &Path,
    session_name: &str,
    tmux: &str,
    socket: Option<&str>,
) -> i32 {
    println!("{}", crate::updater::version_line());
    println!("session: {session_name}");
    println!(
        "project: {}",
        session::canon_root(&project_root.to_string_lossy())
    );

    // Recent attach/reload events from the tracing log (metadata lines only).
    let log = crate::dirs_home()
        .join(".dmux")
        .join("logs")
        .join("dmux-rs.log");
    if let Ok(text) = std::fs::read_to_string(&log) {
        let recent: Vec<&str> = text
            .lines()
            .filter(|l| {
                l.contains("attached") || l.contains("applying update") || l.contains("updating to")
            })
            .rev()
            .take(5)
            .collect();
        println!("recent events:");
        for l in recent.iter().rev() {
            println!("  {l}");
        }
    }

    // Live panes via a one-shot, read-only tmux listing (no control mode,
    // no attach): the exact field set adoption consumes.
    let mut cmd = std::process::Command::new(tmux);
    if let Some(s) = socket {
        cmd.args(["-L", s]);
    }
    let fmt = session::list_panes_command();
    let fmt = fmt.trim_start_matches("list-panes -s -F ");
    let out = cmd
        .args(["list-panes", "-a", "-F"])
        .arg(fmt.trim_matches('\''))
        .output();
    let Ok(out) = out else {
        eprintln!("diagnose: tmux not reachable");
        return 1;
    };
    let reply = dmux_cc::Reply {
        lines: out
            .stdout
            .split(|b| *b == b'\n')
            .map(|l| l.to_vec())
            .filter(|l| !l.is_empty())
            .collect(),
        ok: out.status.success(),
        rtt: std::time::Duration::ZERO,
    };
    let infos = session::parse_pane_list(&reply);

    println!("panes ({}):", infos.len());
    for report in classify_panes(config, &infos) {
        println!("  {}", report.line);
    }
    let stale = stale_records(config, &infos);
    if !stale.is_empty() {
        println!("stale records ({}):", stale.len());
        for s in stale {
            println!("  {s}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_cc::{PaneId, WindowId};

    fn mk(pane: u32, title: &str, cwd: &str) -> TmuxPaneInfo {
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

    fn fixture(name: &str) -> (std::path::PathBuf, DmuxConfig, String, String) {
        let t = std::env::temp_dir().join(format!("dmux-diag-{name}-{}", std::process::id()));
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
                 "projectRoot": a},
                {"id":"2","slug":"terminal-5","prompt":"","paneId":"%43","type":"shell",
                 "projectRoot": b},
                {"id":"3","slug":"lonely","prompt":"","paneId":"%77","type":"shell",
                 "projectRoot": a}
            ]
        }))
        .unwrap();
        (t, config, a, b)
    }

    #[test]
    fn classification_flags_cover_the_identity_ladder() {
        let (t, config, a, _b) = fixture("ladder");
        // Exact pane-id match; the live pane id equals the record's.
        let r = classify_panes(Some(&config), &[mk(42, "terminal-5", &a)]);
        assert!(r[0].line.contains("match=pane-id"), "{}", r[0].line);
        assert!(!r[0].line.contains("STALE"), "{}", r[0].line);
        // Slug+cwd: new pane id, but the live cwd disambiguates the
        // duplicate slug — and the surviving record's pane id is stale.
        let r = classify_panes(Some(&config), &[mk(99, "terminal-5", &a)]);
        assert!(r[0].line.contains("match=slug+cwd"), "{}", r[0].line);
        assert!(
            r[0].line.contains("STALE(record-pane=%42)"),
            "{}",
            r[0].line
        );
        // Slug-only on a duplicate slug: flagged ambiguous.
        let r = classify_panes(Some(&config), &[mk(99, "terminal-5", "/nowhere")]);
        assert!(
            r[0].line.contains("AMBIGUOUS(duplicate-slug)"),
            "{}",
            r[0].line
        );
        // No record at all.
        let r = classify_panes(Some(&config), &[mk(7, "mystery", "/nowhere")]);
        assert!(r[0].line.contains("UNMATCHED"), "{}", r[0].line);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn path_aliases_canonicalize_before_comparison() {
        // macOS: temp_dir() is under /var, a symlink to /private/var. A cwd
        // reported through the alias must still resolve to the saved root.
        let (t, config, a, _b) = fixture("alias");
        let alias = if a.starts_with("/private/var/") {
            a.replacen("/private/var/", "/var/", 1)
        } else if a.starts_with("/var/") {
            format!("/private{a}")
        } else {
            let _ = std::fs::remove_dir_all(&t);
            return; // no alias pair on this platform
        };
        let r = classify_panes(Some(&config), &[mk(99, "terminal-5", &alias)]);
        assert!(r[0].line.contains("match=slug+cwd"), "{}", r[0].line);
        let canon = session::canon_root(&a);
        assert!(
            r[0].line.contains(&format!("cwd={canon}")),
            "{} vs {canon}",
            r[0].line
        );
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn stale_records_lists_only_records_without_any_live_pane() {
        let (t, config, a, _b) = fixture("stale");
        // terminal-5 is live (matching %42); "lonely" (%77) has no live
        // pane by id or slug → the one stale entry.
        let stale = stale_records(Some(&config), &[mk(42, "terminal-5", &a)]);
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(stale[0].contains("slug=lonely"), "{}", stale[0]);
        assert!(stale[0].contains("NO LIVE PANE"), "{}", stale[0]);
        // A live pane titled "lonely" (even with a new id) clears it.
        let stale = stale_records(
            Some(&config),
            &[mk(42, "terminal-5", &a), mk(99, "lonely", &a)],
        );
        assert!(stale.is_empty(), "{stale:?}");
        // No config → nothing to report.
        assert!(stale_records(None, &[]).is_empty());
        let _ = std::fs::remove_dir_all(&t);
    }
}
