//! Input routing: termwiz events → dmux global actions, leader-key commands,
//! or focused-pane bytes injected via control-mode `send-keys -H`.
//!
//! Key policy (see ROADMAP.md): pane apps own the keyboard. dmux commands
//! live behind a `Ctrl+b` leader (double-press sends a literal Ctrl+b), plus
//! collision-free `Super`/Cmd chords when the host terminal speaks the kitty
//! keyboard protocol, plus a small set of Alt alternates.

use dmux_host::{
    KeyCode, KeyCodeEncodeModes, KeyEvent, KeyboardEncoding, Modifiers, MouseButtons, MouseEvent,
};
use dmux_vt::InputModes;

/// What the app loop should do with one input event.
#[derive(Debug, PartialEq, Eq)]
pub enum Routed {
    Quit,
    Detach,
    ToggleHud,
    FocusNext,
    FocusPrev,
    FocusIndex(usize),
    OpenMenu,
    OpenSettings,
    OpenNewAgent,
    OpenShortcuts,
    OpenLogs,
    NewTerminal,
    AddProject,
    RenameFocused,
    HideFocused,
    CloseFocused,
    /// Leader was pressed: arm and wait for the command key.
    LeaderArm,
    /// Raw bytes for the focused pane's pty.
    PaneBytes(Vec<u8>),
    /// Scroll the focused pane's local view (positive = into history).
    ScrollView(i32),
    Ignore,
}

pub const LEADER_HINT: &str = "^b: n agents · t term · p proj · s settings · m menu · r rename · h hide · x close · d detach · ?";

fn action_to_routed(action: crate::keys::Action) -> Routed {
    use crate::keys::Action;
    match action {
        Action::OpenSettings => Routed::OpenSettings,
        Action::OpenNewAgent => Routed::OpenNewAgent,
        Action::NewTerminal => Routed::NewTerminal,
        Action::AddProject => Routed::AddProject,
        Action::OpenMenu => Routed::OpenMenu,
        Action::OpenShortcuts => Routed::OpenShortcuts,
        Action::RenameFocused => Routed::RenameFocused,
        Action::HideFocused => Routed::HideFocused,
        Action::CloseFocused => Routed::CloseFocused,
        Action::ToggleHud => Routed::ToggleHud,
        Action::Quit => Routed::Quit,
        Action::FocusNext => Routed::FocusNext,
        Action::FocusPrev => Routed::FocusPrev,
        Action::ScrollUp => Routed::ScrollView(10),
        Action::ScrollDown => Routed::ScrollView(-10),
    }
}

/// Route one key event. `leader_armed` = the previous key was the leader.
pub fn route_key(
    key: &KeyEvent,
    modes: InputModes,
    leader_armed: bool,
    keymap: &crate::keys::Keymap,
) -> Routed {
    if leader_armed {
        return route_leader_command(key, modes);
    }

    if let Some(chord) = crate::keys::event_chord(key) {
        if keymap.is_leader(&chord) {
            return Routed::LeaderArm;
        }
        if let Some(action) = keymap.lookup(&chord) {
            return action_to_routed(action);
        }
    }

    // Modifier+digit pane focus (not remappable; both Alt and Super forms).
    let alt = key.modifiers.contains(Modifiers::ALT);
    let sup = key.modifiers.contains(Modifiers::SUPER);
    if (alt || sup) && !key.modifiers.contains(Modifiers::CTRL) {
        if let KeyCode::Char(c @ '1'..='9') = key.key {
            return Routed::FocusIndex(c as usize - '1' as usize);
        }
    }

    match encode_key(key, modes) {
        Some(bytes) if !bytes.is_empty() => Routed::PaneBytes(bytes),
        _ => Routed::Ignore,
    }
}

fn route_leader_command(key: &KeyEvent, modes: InputModes) -> Routed {
    let ctrl = key.modifiers.contains(Modifiers::CTRL);
    match (&key.key, ctrl) {
        // Leader twice = send the literal Ctrl+b to the pane (tmux convention).
        (KeyCode::Char('b'), true) => Routed::PaneBytes(vec![0x02]),
        (KeyCode::Char('n'), _) => Routed::OpenNewAgent,
        (KeyCode::Char('t'), _) => Routed::NewTerminal,
        (KeyCode::Char('p'), _) => Routed::AddProject,
        (KeyCode::Char('s'), _) => Routed::OpenSettings,
        (KeyCode::Char('m'), _) | (KeyCode::Enter, _) => Routed::OpenMenu,
        (KeyCode::Char('r'), _) => Routed::RenameFocused,
        (KeyCode::Char('h'), _) => Routed::HideFocused,
        (KeyCode::Char('x'), _) => Routed::CloseFocused,
        (KeyCode::Char('d'), _) => Routed::Detach,
        (KeyCode::Char('l'), _) => Routed::OpenLogs,
        (KeyCode::Char('?'), _) => Routed::OpenShortcuts,
        (KeyCode::Char('y'), _) => Routed::ToggleHud,
        (KeyCode::Char(c @ '1'..='9'), _) => Routed::FocusIndex(*c as usize - '1' as usize),
        (KeyCode::RightArrow, _) | (KeyCode::DownArrow, _) => Routed::FocusNext,
        (KeyCode::LeftArrow, _) | (KeyCode::UpArrow, _) => Routed::FocusPrev,
        (KeyCode::PageUp, _) => Routed::ScrollView(10),
        (KeyCode::PageDown, _) => Routed::ScrollView(-10),
        (KeyCode::Escape, _) => Routed::Ignore,
        // Unknown leader key: swallow it (don't leak half a chord to the pane)
        // but do nothing. `modes` unused here on purpose.
        _ => {
            let _ = modes;
            Routed::Ignore
        }
    }
}

pub fn encode_key(key: &KeyEvent, modes: InputModes) -> Option<Vec<u8>> {
    let encode_modes = KeyCodeEncodeModes {
        // Phase 0 always encodes legacy xterm toward panes; pane-side kitty
        // passthrough is a follow-up (host-side kitty is already used for our
        // own Super chords).
        encoding: KeyboardEncoding::Xterm,
        application_cursor_keys: modes.app_cursor,
        newline_mode: false,
        modify_other_keys: None,
    };
    key.key.encode(key.modifiers, encode_modes, true).ok().map(|s| s.into_bytes())
}

/// Wrap paste text in bracketed-paste markers when the pane requested them.
pub fn encode_paste(text: &str, modes: InputModes) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

/// Encode a mouse event for a pane app that enabled SGR mouse reporting.
/// `col`/`row` are pane-local 0-based; output uses 1-based SGR coords.
pub fn encode_sgr_mouse(button: u8, pressed: bool, col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<{};{};{}{}", button, col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
}

/// Classify a mouse event: (col, row, kind, shift), 0-based. SGR 1002 mode
/// reports press and drag identically (LEFT held); the app distinguishes them
/// by tracking drag state. An event with no buttons is the release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    LeftHeld,
    WheelUp,
    WheelDown,
    Release,
}

pub fn classify_mouse(ev: &MouseEvent) -> (u16, u16, MouseKind, bool) {
    let col = ev.x.saturating_sub(1);
    let row = ev.y.saturating_sub(1);
    let shift = ev.modifiers.contains(Modifiers::SHIFT);
    let kind = if ev.mouse_buttons.contains(MouseButtons::VERT_WHEEL) {
        if ev.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE) {
            MouseKind::WheelUp
        } else {
            MouseKind::WheelDown
        }
    } else if ev.mouse_buttons.contains(MouseButtons::LEFT) {
        MouseKind::LeftHeld
    } else {
        MouseKind::Release
    };
    (col, row, kind, shift)
}

/// Convert routed pane bytes into a control-mode `send-keys -H` command.
pub fn send_keys_hex(pane: dmux_cc::PaneId, bytes: &[u8]) -> String {
    let mut cmd = String::with_capacity(20 + bytes.len() * 3);
    cmd.push_str(&format!("send-keys -t {} -H", pane));
    for b in bytes {
        cmd.push_str(&format!(" {b:02x}"));
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: Modifiers) -> KeyEvent {
        KeyEvent { key: code, modifiers: mods }
    }

    fn km() -> crate::keys::Keymap {
        crate::keys::Keymap::from_overrides(&serde_json::Map::new())
    }

    #[test]
    fn leader_flow() {
        assert_eq!(route_key(&key(KeyCode::Char('b'), Modifiers::CTRL), InputModes::default(), false, &km()), Routed::LeaderArm);
        assert_eq!(route_key(&key(KeyCode::Char('n'), Modifiers::NONE), InputModes::default(), true, &km()), Routed::OpenNewAgent);
        // Double leader = literal Ctrl+b to the pane.
        assert_eq!(
            route_key(&key(KeyCode::Char('b'), Modifiers::CTRL), InputModes::default(), true, &km()),
            Routed::PaneBytes(vec![0x02])
        );
        // Unknown leader command swallowed, not leaked.
        assert_eq!(route_key(&key(KeyCode::Char('z'), Modifiers::NONE), InputModes::default(), true, &km()), Routed::Ignore);
    }

    #[test]
    fn plain_chars_become_pane_bytes() {
        match route_key(&key(KeyCode::Char('a'), Modifiers::NONE), InputModes::default(), false, &km()) {
            Routed::PaneBytes(b) => assert_eq!(b, b"a"),
            other => panic!("{other:?}"),
        }
        // 'b' without ctrl is NOT the leader.
        match route_key(&key(KeyCode::Char('b'), Modifiers::NONE), InputModes::default(), false, &km()) {
            Routed::PaneBytes(b) => assert_eq!(b, b"b"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keymap_chords() {
        // Direct settings chord (kitty hosts).
        assert_eq!(
            route_key(&key(KeyCode::Char(','), Modifiers::CTRL), InputModes::default(), false, &km()),
            Routed::OpenSettings
        );
        // TS-era letters on Alt.
        assert_eq!(route_key(&key(KeyCode::Char('s'), Modifiers::ALT), InputModes::default(), false, &km()), Routed::OpenSettings);
        assert_eq!(route_key(&key(KeyCode::Char('n'), Modifiers::SUPER), InputModes::default(), false, &km()), Routed::OpenNewAgent);
        assert_eq!(route_key(&key(KeyCode::Char('3'), Modifiers::SUPER), InputModes::default(), false, &km()), Routed::FocusIndex(2));
        // A rebound keymap changes routing.
        let mut o = serde_json::Map::new();
        o.insert("settings".into(), serde_json::Value::String("f2".into()));
        let custom = crate::keys::Keymap::from_overrides(&o);
        assert_eq!(route_key(&key(KeyCode::Function(2), Modifiers::NONE), InputModes::default(), false, &custom), Routed::OpenSettings);
        assert!(matches!(
            route_key(&key(KeyCode::Char(','), Modifiers::CTRL), InputModes::default(), false, &custom),
            Routed::PaneBytes(_) | Routed::Ignore
        ));
    }

    #[test]
    fn arrows_respect_app_cursor_mode() {
        let normal = encode_key(&key(KeyCode::UpArrow, Modifiers::NONE), InputModes::default()).unwrap();
        assert_eq!(normal, b"\x1b[A");
        let app = encode_key(
            &key(KeyCode::UpArrow, Modifiers::NONE),
            InputModes { app_cursor: true, ..Default::default() },
        )
        .unwrap();
        assert_eq!(app, b"\x1bOA");
    }

    #[test]
    fn hex_command_format() {
        let cmd = send_keys_hex(dmux_cc::PaneId(5), b"hi\n");
        assert_eq!(cmd, "send-keys -t %5 -H 68 69 0a");
    }

    #[test]
    fn paste_bracketing() {
        let out = encode_paste("x", InputModes { bracketed_paste: true, ..Default::default() });
        assert_eq!(out, b"\x1b[200~x\x1b[201~");
    }
}
