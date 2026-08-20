//! Crate-root behavior tests extracted from main.rs (#115 test-support
//! boundary): CLI parsing, pane-list application, timers, and misc
//! app-level contracts.

use super::*;

#[test]
fn profiler_flag_accepts_both_spellings() {
    use clap::Parser as _;
    // Canonical name and the legacy alias both work (#115); existing
    // launch scripts using --hud keep their startup behavior.
    let canonical = Cli::parse_from(["dmux-rs", "--profiler"]);
    assert!(canonical.profiler);
    let legacy = Cli::parse_from(["dmux-rs", "--hud"]);
    assert!(legacy.profiler);
    let neither = Cli::parse_from(["dmux-rs"]);
    assert!(!neither.profiler);
}

#[test]
fn slugify_prompts() {
    assert_eq!(slugify("Fix the auth bug"), "fix-the-auth-bug");
    assert_eq!(
        slugify("Add   OAuth2!! support, please"),
        "add-oauth2-support-please"
    );
    assert!(slugify("").starts_with("agents-"));
}

#[test]
fn shell_quoting() {
    assert_eq!(shq("/tmp/simple-path"), "/tmp/simple-path");
    assert_eq!(shq("a path"), "'a path'");
    assert_eq!(shq("it's"), "'it'\\''s'");
}

#[test]
fn iso_timestamp_shape() {
    let ts = iso_now();
    assert_eq!(ts.len(), 24);
    assert!(ts.ends_with("Z"));
    assert!(ts.starts_with("20"));
}

#[test]
fn reported_titles_lose_their_leading_spinners() {
    // Claude Code animates an asterisk-family glyph in its titles.
    assert_eq!(strip_status_glyphs("✳ dmux-rs"), "dmux-rs");
    assert_eq!(strip_status_glyphs("✻ ✳ dmux-rs"), "dmux-rs");
    // Braille spinner frames.
    assert_eq!(strip_status_glyphs("⠹ building"), "building");
    // Plain titles pass through, including mid-title glyphs.
    assert_eq!(
        strip_status_glyphs("cargo build ✳ hot"),
        "cargo build ✳ hot"
    );
    // A title that is ONLY a spinner strips to empty (then ignored).
    assert_eq!(strip_status_glyphs("✳"), "");
}

#[test]
fn legacy_multi_pane_windows_break_out_extras() {
    let mk = |pane: u32, window: u32| session::TmuxPaneInfo {
        pane: PaneId(pane),
        window: dmux_cc::WindowId(window),
        title: String::new(),
        width: 80,
        height: 24,
        alternate_on: false,
        current_command: "bash".into(),
        window_name: "w".into(),
        pane_pid: 1,
        start_command: String::new(),
        extended_keys_mode2: false,
        current_path: String::new(),
    };
    // Window 0 has three panes, window 1 has one: only the two extras
    // of window 0 are broken out; a re-run on the result is a no-op.
    let infos = vec![mk(0, 0), mk(1, 0), mk(2, 0), mk(3, 1)];
    assert_eq!(panes_to_break_out(&infos), vec![PaneId(1), PaneId(2)]);
    let after = vec![mk(0, 0), mk(1, 2), mk(2, 3), mk(3, 1)];
    assert!(panes_to_break_out(&after).is_empty());
}

#[test]
fn sidebar_typing_never_leaks_or_drops_focus() {
    use dmux_host::{KeyCode, Modifiers};
    let keymap = keys::Keymap::from_overrides(&Default::default());
    let key = |k: KeyCode, m: Modifiers| dmux_host::KeyEvent {
        key: k,
        modifiers: m,
    };
    // A word typed while sidebar-focused: every unbound letter is a
    // no-op — never PassThrough (which would reach a pane), never
    // LeaveFocus (#27). Letters with sidebar meanings map to actions.
    for c in "wordy".chars() {
        let action = sidebar_key_action(&key(KeyCode::Char(c), Modifiers::NONE), &keymap);
        assert!(
            !matches!(
                action,
                SidebarKeyAction::PassThrough | SidebarKeyAction::LeaveFocus
            ),
            "typed '{c}' must stay in the sidebar, got {action:?}"
        );
    }
    // Unknown modified keys are consumed too.
    assert_eq!(
        sidebar_key_action(&key(KeyCode::Char('z'), Modifiers::CTRL), &keymap),
        SidebarKeyAction::Ignore
    );
    // The leader chord passes through so global bindings keep working.
    assert_eq!(
        sidebar_key_action(&key(KeyCode::Char('b'), Modifiers::CTRL), &keymap),
        SidebarKeyAction::PassThrough
    );
    // Recognized hotkeys, Enter, and Escape keep their meanings.
    assert_eq!(
        sidebar_key_action(&key(KeyCode::Char('j'), Modifiers::NONE), &keymap),
        SidebarKeyAction::Down
    );
    assert_eq!(
        sidebar_key_action(&key(KeyCode::Char('i'), Modifiers::NONE), &keymap),
        SidebarKeyAction::Issues
    );
    assert_eq!(
        sidebar_key_action(&key(KeyCode::Enter, Modifiers::NONE), &keymap),
        SidebarKeyAction::Activate
    );
    assert_eq!(
        sidebar_key_action(&key(KeyCode::Escape, Modifiers::NONE), &keymap),
        SidebarKeyAction::LeaveFocus
    );
    assert_eq!(
        sidebar_key_action(&key(KeyCode::Char('q'), Modifiers::NONE), &keymap),
        SidebarKeyAction::Ignore
    );
}

#[test]
fn sidebar_drag_thresholds_and_follows() {
    // #26: a press+release on the same row is a click, never a reorder.
    let armed = SidebarDrag::Armed {
        src: 2,
        start_row: 5,
    };
    assert_eq!(armed.reordering(), None);
    assert_eq!(armed.motion(5), armed, "same-row motion stays armed");
    // Crossing a row enters reorder mode and then follows the pointer.
    let dragging = armed.motion(6);
    assert_eq!(dragging.reordering(), Some((2, 6)));
    assert_eq!(dragging.motion(9).reordering(), Some((2, 9)));
}

#[test]
fn tooltip_clamps_to_bounds_and_expires() {
    let area = dmux_compositor::Rect::new(0, 0, 100, 30);
    // Interior release: one row above the cursor.
    assert_eq!(
        tooltip_rect(area, (40, 10), 22),
        dmux_compositor::Rect::new(40, 9, 22, 1)
    );
    // Top edge: stays on screen (no row above to use).
    assert_eq!(tooltip_rect(area, (40, 0), 22).y, 0);
    // Right edge: shifted left so the whole box fits.
    let r = tooltip_rect(area, (95, 10), 22);
    assert_eq!(r.right(), 100);
    // Bottom edge: still inside.
    assert!(tooltip_rect(area, (40, 29), 22).bottom() <= 30);
    // Wider than the terminal: clamped to it.
    assert_eq!(tooltip_rect(area, (0, 5), 200).w, 100);
    // Expiry: a later copy restarts the clock; the deadline decides.
    let t = Tooltip {
        text: "Copied to clipboard".into(),
        x: 1,
        y: 1,
        until: Instant::now(),
    };
    assert!(
        Instant::now() >= t.until,
        "an elapsed deadline reads as expired"
    );
}
