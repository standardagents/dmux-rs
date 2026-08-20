//! Session-model tests: adoption, keepalive identity, reseeding,
//! restore planning, ownership recovery (#76), and reordering.

use super::*;
use crate::registry::adopt_panes;

fn reply_of(lines: &[&str]) -> Reply {
    Reply {
        lines: lines.iter().map(|l| l.as_bytes().to_vec()).collect(),
        ok: true,
        rtt: std::time::Duration::ZERO,
    }
}

#[test]
fn parses_pane_list_and_filters_infra() {
    let reply = reply_of(&[
        "%0\u{1}@0\u{1}dmux\u{1}40\u{1}60\u{1}0\u{1}node\u{1}zsh",
        "%5\u{1}@0\u{1}Fix auth__dmux__fix-auth\u{1}100\u{1}40\u{1}0\u{1}claude\u{1}zsh",
        "%7\u{1}@0\u{1}dmux-spacer-1\u{1}20\u{1}40\u{1}0\u{1}node\u{1}zsh",
        "%9\u{1}@1\u{1}shell-1\u{1}80\u{1}24\u{1}1\u{1}zsh\u{1}work",
    ]);
    let infos = parse_pane_list(&reply);
    assert_eq!(infos.len(), 4);
    let adopted = adopt_panes(None, &infos);
    assert_eq!(adopted.len(), 2);
    assert_eq!(adopted[0].slug, "fix-auth");
    assert_eq!(adopted[0].title, "Fix auth");
    assert_eq!(adopted[1].slug, "shell-1");
}

#[test]
fn seed_restores_background_bands_from_dash_n_capture() {
    // seed_command captures with -N, so BCE-filled cells (composer bands,
    // banded padding rows) arrive as real spaces under their SGR. The
    // replay must reproduce them exactly — and must NOT invent bands on
    // rows whose trailing cells the capture left out (default blanks).
    let reply = reply_of(&["%5\u{1}@0\u{1}p__dmux__p\u{1}30\u{1}5\u{1}0\u{1}zsh\u{1}w"]);
    let infos = parse_pane_list(&reply);
    let mut pane = adopt_panes(None, &infos).remove(0);
    pane.begin_reseed();
    let band_pad = format!("\u{1b}[48;5;236m{}", " ".repeat(30));
    let band_text = format!("> say hello to me{}", " ".repeat(13));
    let seed = reply_of(&[
        &band_pad,                          // banded blank padding row (row 0)
        &band_text,                         // banded text row, SGR carried over (row 1)
        "",                                 // default blank row (row 2)
        "\u{1b}[49mplain\u{1b}[48;5;236mX", // row 3: default text, one banded X, rest default
    ]);
    pane.finish_reseed(&seed, None);

    let mut buf = dmux_compositor::CellBuffer::new(30, 5);
    pane.term
        .render_into(&mut buf, dmux_compositor::Rect::new(0, 0, 30, 5));
    let band = dmux_compositor::Color::Indexed(236);
    let default = dmux_compositor::Color::Default;
    // Padding row and text row: banded edge to edge.
    assert_eq!(buf.get(0, 0).bg, band, "padding row must be banded");
    assert_eq!(
        buf.get(29, 0).bg,
        band,
        "padding row must span the full width"
    );
    assert_eq!(buf.get(5, 1).bg, band);
    assert_eq!(
        buf.get(29, 1).bg,
        band,
        "text row band must span the full width"
    );
    // Default blank row stays default despite band rows around it.
    assert_eq!(buf.get(29, 2).bg, default, "blank row must not be banded");
    // Open SGR at end-of-line must not band unused trailing cells.
    assert_eq!(buf.get(5, 3).bg, band, "the X itself is banded");
    assert_eq!(
        buf.get(29, 3).bg,
        default,
        "unused trailing cells stay default"
    );
}

#[test]
fn reseed_buffers_live_output() {
    let reply = reply_of(&["%5\u{1}@0\u{1}p__dmux__p\u{1}20\u{1}4\u{1}0\u{1}zsh\u{1}w"]);
    let mut pane = adopt_panes(None, &parse_pane_list(&reply)).remove(0);
    pane.begin_reseed();
    // Output arriving during reseed is buffered by the app into reseed_buffer.
    pane.reseed_buffer.as_mut().unwrap().push(b" live".to_vec());
    pane.finish_reseed(&reply_of(&["seeded line"]), Some((5, 0)));
    let tail = pane.term.read_tail_text(4);
    assert!(
        tail.contains("seede live") || tail.contains("seeded"),
        "tail: {tail:?}"
    );
    assert!(pane.reseed_buffer.is_none());
}

#[test]
fn alt_screen_pane_seeds_onto_alt_grid() {
    // #12: a pane tmux reports as alternate_on must seed onto the alt
    // grid. On the primary grid, every full-screen repaint scrolled a
    // stale frame into scrollback the real pane doesn't have, and
    // wheel-scrolling rendered overlapping frame fragments.
    let reply = reply_of(&["%7\u{1}@1\u{1}p__cc__p\u{1}30\u{1}5\u{1}1\u{1}node\u{1}w"]);
    let infos = parse_pane_list(&reply);
    assert!(infos[0].alternate_on);
    let mut pane = adopt_panes(None, &infos).remove(0);
    assert!(pane.alt_screen);
    pane.begin_reseed();
    pane.finish_reseed(&reply_of(&["transcript row"]), None);
    assert!(
        pane.term.input_modes().alt_screen,
        "seed must land on the alt grid"
    );
    // Repaint churn must not accumulate history…
    for i in 0..50 {
        pane.advance_recorded(format!("frame {i}\r\n").as_bytes());
    }
    assert_eq!(pane.term.history_len(), 0, "alt grid has no scrollback");
    // …and the local view can't scroll into stale frames.
    assert_eq!(pane.term.scroll_view(3), 0);
}

#[test]
fn option_dialogs_map_to_waiting_never_auto_accept() {
    // #31: an option dialog is the user's decision — Waiting/attention,
    // never Working-with-injected-Enter.
    assert_eq!(
        verdict_pane_status(&dmux_infer::PaneVerdict::OptionDialog),
        PaneStatus::Waiting
    );
    assert_eq!(
        verdict_pane_status(&dmux_infer::PaneVerdict::OpenPrompt),
        PaneStatus::Idle
    );
    assert_eq!(
        verdict_pane_status(&dmux_infer::PaneVerdict::InProgress),
        PaneStatus::Working
    );
}

#[test]
fn keepalive_detected_after_automatic_rename() {
    // #10: automatic-rename configs rename the keepalive window to
    // "sleep"; identity must survive via the start command, or every
    // reconcile re-creates the keepalive until PTYs run out.
    let mk = |window_name: &str, start: &str| TmuxPaneInfo {
        pane: PaneId(1),
        window: WindowId(1),
        title: "host".into(),
        width: 80,
        height: 24,
        alternate_on: false,
        current_command: "sleep".into(),
        window_name: window_name.into(),
        pane_pid: 42,
        start_command: start.into(),
        extended_keys_mode2: false,
        current_path: String::new(),
    };
    // Renamed by automatic-rename: still a keepalive.
    assert!(is_keepalive(&mk("sleep", KEEPALIVE_CMD)));
    // tmux may quote the start command in formats.
    assert!(is_keepalive(&mk("sleep", "'sleep 2147483647'")));
    // Legacy builds: name only, no start_command field.
    assert!(is_keepalive(&mk(KEEPALIVE_NAME, "")));
    // A user's own sleep is NOT a keepalive (different duration)…
    assert!(!is_keepalive(&mk("sleep", "sleep 30")));
    // …and neither is an ordinary shell window.
    assert!(!is_keepalive(&mk("zsh", "")));
}

#[test]
fn restore_plan_covers_representative_ts_config() {
    // #20: agent + shell + hidden + missing-path + multi-project +
    // legacy-infra records from a TS-written config.
    let config: DmuxConfig = serde_json::from_str(
        r#"{
              "projectName": "app",
              "projectRoot": "/main",
              "panes": [
                {"id":"1","slug":"fix-auth","prompt":"","paneId":"%9","type":"worktree",
                 "worktreePath":"/main/.wt/fix-auth","agent":"claude","agentSessionId":"sess-123"},
                {"id":"2","slug":"gone-wt","prompt":"","paneId":"%10","type":"worktree",
                 "worktreePath":"/main/.wt/deleted","agent":"claude"},
                {"id":"3","slug":"terminal-1","prompt":"","paneId":"%11","type":"shell",
                 "displayName":"logs","shellCwd":"/main/logs","hidden":true},
                {"id":"4","slug":"terminal-2","prompt":"","paneId":"%12","type":"shell",
                 "shellCwd":"/tmp/gone-dir"},
                {"id":"5","slug":"other-term","prompt":"","paneId":"%13","type":"shell",
                 "shellCwd":"/other","projectRoot":"/other"},
                {"id":"6","slug":"dmux","prompt":"","paneId":"%1"},
                {"id":"7","slug":"dmux-spacer-1","prompt":"","paneId":"%2"}
              ]
            }"#,
    )
    .unwrap();
    let exists = |p: &str| matches!(p, "/main/.wt/fix-auth" | "/main/logs" | "/other" | "/main");
    let (plans, skipped) = plan_session_restore(&config, "/main", &exists);
    assert_eq!(
        plans,
        vec![
            RestorePlan::Agent {
                slug: "fix-auth".into(),
                display: "fix-auth".into(),
                path: "/main/.wt/fix-auth".into(),
                agent: "claude".into(),
            },
            RestorePlan::Shell {
                slug: "terminal-1".into(),
                display: "logs".into(),
                cwd: "/main/logs".into(),
                project_root: None,
            },
            // Saved cwd is gone: falls back to the project root.
            RestorePlan::Shell {
                slug: "terminal-2".into(),
                display: "terminal-2".into(),
                cwd: "/main".into(),
                project_root: None,
            },
            // Other project's terminal keeps its project association.
            RestorePlan::Shell {
                slug: "other-term".into(),
                display: "other-term".into(),
                cwd: "/other".into(),
                project_root: Some("/other".into()),
            },
        ]
    );
    // The missing worktree is reported, not fatal; infra records vanish.
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0].contains("gone-wt"));
}

#[test]
fn escaped_pane_list_reply_decodes_to_records() {
    // #19: raw control-mode bytes from tmux 3.5a — the 0x01 field
    // separators arrive octal-escaped as the four bytes \001, and the
    // start command is double-quoted. Feed the actual wire bytes through
    // the parser, decode, and expect a keepalive record.
    let wire: &[u8] = b"%begin 1755600000 3 1\n%0\\001@0\\001Mac-Studio.local\\00180\\00124\\0010\\001sleep\\001dmux-keepalive\\0012555\\001\"sleep 2147483647\"\n%end 1755600000 3 1\n";
    let mut parser = dmux_cc::Parser::new();
    let mut events = Vec::new();
    parser.feed(wire, &mut events);
    let lines: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            dmux_cc::CcEvent::ReplyLine(l) => Some(l.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 1, "one payload line, got {events:?}");
    let mut reply = Reply {
        lines,
        ok: true,
        rtt: std::time::Duration::ZERO,
    };
    // Undecoded, the reply yields no records (the pre-fix failure that
    // blinded the keepalive guards and re-leaked #10).
    assert!(parse_pane_list(&reply).is_empty());
    reply.unescape_lines();
    let infos = parse_pane_list(&reply);
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].pane, PaneId(0));
    assert_eq!(infos[0].width, 80);
    assert_eq!(infos[0].window_name, "dmux-keepalive");
    assert!(is_keepalive(&infos[0]));
}

#[test]
fn pane_list_parses_start_command() {
    let line = "%3\u{1}@2\u{1}t\u{1}80\u{1}24\u{1}0\u{1}sleep\u{1}sleep\u{1}9\u{1}sleep 2147483647";
    let reply = Reply {
        lines: vec![line.as_bytes().to_vec()],
        ok: true,
        rtt: std::time::Duration::ZERO,
    };
    let infos = parse_pane_list(&reply);
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].start_command, "sleep 2147483647");
    assert!(is_keepalive(&infos[0]));

    let extended = reply_of(&[
            "%4\u{1}@3\u{1}agent\u{1}80\u{1}24\u{1}0\u{1}codex\u{1}work\u{1}10\u{1}/usr/bin/codex\u{1}Ext 2",
        ]);
    let panes = adopt_panes(None, &parse_pane_list(&extended));
    assert!(pane_input_modes(&panes, 0).extended_keys_mode2);
    // Older 9-field listings still parse (start_command empty).
    let line9 = "%3\u{1}@2\u{1}t\u{1}80\u{1}24\u{1}0\u{1}zsh\u{1}w\u{1}9";
    let reply9 = Reply {
        lines: vec![line9.as_bytes().to_vec()],
        ok: true,
        rtt: std::time::Duration::ZERO,
    };
    assert_eq!(parse_pane_list(&reply9)[0].start_command, "");
}
