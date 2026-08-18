//! Input routing: termwiz events → dmux global actions, sidebar interaction,
//! or focused-pane bytes injected via control-mode `send-keys -H`.

use dmux_host::{
    KeyCode, KeyCodeEncodeModes, KeyEvent, KeyboardEncoding, Modifiers, MouseButtons, MouseEvent,
};
use dmux_vt::InputModes;

/// What the app loop should do with one input event.
#[derive(Debug, PartialEq, Eq)]
pub enum Routed {
    Quit,
    ToggleHud,
    FocusNext,
    FocusPrev,
    FocusIndex(usize),
    /// Raw bytes for the focused pane's pty.
    PaneBytes(Vec<u8>),
    /// Scroll the focused pane's local view (positive = into history).
    ScrollView(i32),
    SidebarClick { row: u16, col: u16 },
    SidebarWheel(i32),
    PaneClick { col: u16, row: u16 },
    Ignore,
}

pub fn route_key(key: &KeyEvent, modes: InputModes) -> Routed {
    let ctrl = key.modifiers.contains(Modifiers::CTRL);
    let alt = key.modifiers.contains(Modifiers::ALT);

    // dmux-global hotkeys come first; everything else goes to the pane.
    match (&key.key, ctrl, alt) {
        (KeyCode::Char('q'), true, false) => return Routed::Quit,
        (KeyCode::Char('y'), true, false) => return Routed::ToggleHud,
        (KeyCode::RightArrow, false, true) => return Routed::FocusNext,
        (KeyCode::LeftArrow, false, true) => return Routed::FocusPrev,
        (KeyCode::Char(c @ '1'..='9'), false, true) => {
            return Routed::FocusIndex(*c as usize - '1' as usize)
        }
        (KeyCode::PageUp, false, true) => return Routed::ScrollView(10),
        (KeyCode::PageDown, false, true) => return Routed::ScrollView(-10),
        _ => {}
    }

    match encode_key(key, modes) {
        Some(bytes) if !bytes.is_empty() => Routed::PaneBytes(bytes),
        _ => Routed::Ignore,
    }
}

pub fn encode_key(key: &KeyEvent, modes: InputModes) -> Option<Vec<u8>> {
    let encode_modes = KeyCodeEncodeModes {
        // Phase 0 always encodes legacy xterm; kitty-protocol passthrough of
        // the pane's advertised flags is a follow-up (needs host-side kitty
        // support probing too).
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

/// Classify a termwiz mouse event into dmux terms. `sidebar_width` splits the
/// x axis. termwiz reports 1-based coordinates.
pub fn route_mouse(ev: &MouseEvent, sidebar_width: u16) -> Routed {
    let col = ev.x.saturating_sub(1);
    let row = ev.y.saturating_sub(1);
    let wheel_up = ev.mouse_buttons.contains(MouseButtons::VERT_WHEEL) && ev.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE);
    let wheel_down = ev.mouse_buttons.contains(MouseButtons::VERT_WHEEL) && !ev.mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE);

    if col < sidebar_width {
        if wheel_up {
            return Routed::SidebarWheel(-1);
        }
        if wheel_down {
            return Routed::SidebarWheel(1);
        }
        if ev.mouse_buttons.contains(MouseButtons::LEFT) {
            return Routed::SidebarClick { row, col };
        }
        return Routed::Ignore;
    }

    if wheel_up {
        return Routed::ScrollView(3);
    }
    if wheel_down {
        return Routed::ScrollView(-3);
    }
    if ev.mouse_buttons.contains(MouseButtons::LEFT) {
        return Routed::PaneClick { col, row };
    }
    Routed::Ignore
}

/// Convert routed pane bytes into a control-mode `send-keys -H` command.
/// Chunked by the caller if needed (tmux command lines should stay short).
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

    #[test]
    fn global_hotkeys() {
        assert_eq!(route_key(&key(KeyCode::Char('q'), Modifiers::CTRL), InputModes::default()), Routed::Quit);
        assert_eq!(
            route_key(&key(KeyCode::Char('3'), Modifiers::ALT), InputModes::default()),
            Routed::FocusIndex(2)
        );
    }

    #[test]
    fn plain_chars_become_pane_bytes() {
        match route_key(&key(KeyCode::Char('a'), Modifiers::NONE), InputModes::default()) {
            Routed::PaneBytes(b) => assert_eq!(b, b"a"),
            other => panic!("{other:?}"),
        }
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
