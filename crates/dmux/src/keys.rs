//! User-configurable keymap: direct (no-leader) chords for global actions,
//! overridable from settings (`"keybindings"` object in settings.json).
//!
//! Defaults map the TS-era single letters onto Alt chords (⌥s settings,
//! ⌥n agents, …), add `ctrl+,` for settings (works wherever the host speaks
//! kitty CSI-u), and keep the Super chords for kitty hosts. The leader itself
//! is rebindable (`"leader"`).

use std::collections::HashMap;

use dmux_host::{KeyCode, KeyEvent, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey {
    Char(char),
    F(u8),
    Esc,
    Enter,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: ChordKey,
    /// ctrl=1, alt=2, super=4 (shift is carried by the char itself).
    pub mods: u8,
}

pub const MOD_CTRL: u8 = 1;
pub const MOD_ALT: u8 = 2;
pub const MOD_SUPER: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    OpenSettings,
    OpenNewAgent,
    NewTerminal,
    AddProject,
    OpenMenu,
    OpenShortcuts,
    RenameFocused,
    HideFocused,
    CloseFocused,
    ToggleHud,
    Quit,
    FocusNext,
    FocusPrev,
    ScrollUp,
    ScrollDown,
}

const ACTION_NAMES: &[(&str, Action)] = &[
    ("settings", Action::OpenSettings),
    ("new-agents", Action::OpenNewAgent),
    ("new-terminal", Action::NewTerminal),
    ("add-project", Action::AddProject),
    ("menu", Action::OpenMenu),
    ("shortcuts", Action::OpenShortcuts),
    ("rename", Action::RenameFocused),
    ("hide", Action::HideFocused),
    ("close", Action::CloseFocused),
    ("hud", Action::ToggleHud),
    ("quit", Action::Quit),
    ("focus-next", Action::FocusNext),
    ("focus-prev", Action::FocusPrev),
    ("scroll-up", Action::ScrollUp),
    ("scroll-down", Action::ScrollDown),
];

/// Parse "ctrl+,", "alt+s", "super+n", "f10", "ctrl+shift+p" (shift folded
/// into the char), "alt+pageup". Returns None for invalid specs.
pub fn parse_chord(spec: &str) -> Option<Chord> {
    let mut mods = 0u8;
    let mut key: Option<ChordKey> = None;
    for part in spec.split('+') {
        let part = part.trim();
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" | "c" => mods |= MOD_CTRL,
            "alt" | "opt" | "option" | "meta" | "m" => mods |= MOD_ALT,
            "super" | "cmd" | "command" | "win" => mods |= MOD_SUPER,
            "shift" => {} // carried by the char itself
            "esc" | "escape" => key = Some(ChordKey::Esc),
            "enter" | "return" | "cr" => key = Some(ChordKey::Enter),
            "tab" => key = Some(ChordKey::Tab),
            "backspace" | "bs" => key = Some(ChordKey::Backspace),
            "up" => key = Some(ChordKey::Up),
            "down" => key = Some(ChordKey::Down),
            "left" => key = Some(ChordKey::Left),
            "right" => key = Some(ChordKey::Right),
            "pageup" | "pgup" => key = Some(ChordKey::PageUp),
            "pagedown" | "pgdn" => key = Some(ChordKey::PageDown),
            _ => {
                let mut chars = part.chars();
                match (chars.next(), chars.next()) {
                    (Some('f'), Some(_)) if lower.len() <= 3 => {
                        key = lower[1..].parse().ok().map(ChordKey::F);
                    }
                    (Some(c), None) => key = Some(ChordKey::Char(c)),
                    _ => return None,
                }
            }
        }
    }
    key.map(|key| Chord { key, mods })
}

/// Human-readable chord for hints (mac-flavored glyphs).
pub fn chord_label(c: &Chord) -> String {
    let mut s = String::new();
    if c.mods & MOD_CTRL != 0 {
        s.push('^');
    }
    if c.mods & MOD_ALT != 0 {
        s.push('⌥');
    }
    if c.mods & MOD_SUPER != 0 {
        s.push('⌘');
    }
    match c.key {
        ChordKey::Char(ch) => s.push(ch),
        ChordKey::F(n) => s.push_str(&format!("F{n}")),
        ChordKey::Esc => s.push_str("esc"),
        ChordKey::Enter => s.push('⏎'),
        ChordKey::Tab => s.push('⇥'),
        ChordKey::Backspace => s.push('⌫'),
        ChordKey::Up => s.push('↑'),
        ChordKey::Down => s.push('↓'),
        ChordKey::Left => s.push('←'),
        ChordKey::Right => s.push('→'),
        ChordKey::PageUp => s.push_str("PgUp"),
        ChordKey::PageDown => s.push_str("PgDn"),
    }
    s
}

/// Convert an incoming key event to a chord (for map lookup).
pub fn event_chord(k: &KeyEvent) -> Option<Chord> {
    let mut mods = 0u8;
    if k.modifiers.contains(Modifiers::CTRL) {
        mods |= MOD_CTRL;
    }
    if k.modifiers.contains(Modifiers::ALT) {
        mods |= MOD_ALT;
    }
    if k.modifiers.contains(Modifiers::SUPER) {
        mods |= MOD_SUPER;
    }
    let key = match k.key {
        KeyCode::Char(c) => ChordKey::Char(c),
        KeyCode::Function(n) => ChordKey::F(n),
        KeyCode::Escape => ChordKey::Esc,
        KeyCode::Enter => ChordKey::Enter,
        KeyCode::Tab => ChordKey::Tab,
        KeyCode::Backspace => ChordKey::Backspace,
        KeyCode::UpArrow => ChordKey::Up,
        KeyCode::DownArrow => ChordKey::Down,
        KeyCode::LeftArrow => ChordKey::Left,
        KeyCode::RightArrow => ChordKey::Right,
        KeyCode::PageUp => ChordKey::PageUp,
        KeyCode::PageDown => ChordKey::PageDown,
        _ => return None,
    };
    Some(Chord { key, mods })
}

pub struct Keymap {
    map: HashMap<Chord, Action>,
    pub leader: Chord,
}

impl Keymap {
    fn defaults() -> Vec<(&'static str, Action)> {
        vec![
            // The TS-era single letters, on Alt.
            ("alt+s", Action::OpenSettings),
            ("alt+n", Action::OpenNewAgent),
            ("alt+t", Action::NewTerminal),
            ("alt+p", Action::AddProject),
            ("alt+m", Action::OpenMenu),
            ("alt+r", Action::RenameFocused),
            ("alt+h", Action::HideFocused),
            ("alt+x", Action::CloseFocused),
            ("alt+?", Action::OpenShortcuts),
            // Direct settings chord (kitty CSI-u hosts).
            ("ctrl+,", Action::OpenSettings),
            // Long-standing bare chords.
            ("ctrl+q", Action::Quit),
            ("ctrl+y", Action::ToggleHud),
            // Navigation.
            ("alt+right", Action::FocusNext),
            ("alt+left", Action::FocusPrev),
            ("alt+pageup", Action::ScrollUp),
            ("alt+pagedown", Action::ScrollDown),
            // Super chords (kitty hosts that forward Cmd).
            ("super+,", Action::OpenSettings),
            ("super+n", Action::OpenNewAgent),
            ("super+t", Action::NewTerminal),
            ("super+k", Action::OpenMenu),
            ("super+r", Action::RenameFocused),
            ("super+h", Action::HideFocused),
            ("super+w", Action::CloseFocused),
            ("super+]", Action::FocusNext),
            ("super+[", Action::FocusPrev),
        ]
    }

    /// Build from defaults + user overrides. `overrides` maps action name →
    /// chord spec; an override REPLACES every default chord for that action
    /// ("" or "none" unbinds it). A "leader" entry rebinds the leader.
    pub fn from_overrides(overrides: &serde_json::Map<String, serde_json::Value>) -> Self {
        let mut leader = Chord {
            key: ChordKey::Char('b'),
            mods: MOD_CTRL,
        };
        if let Some(spec) = overrides.get("leader").and_then(|v| v.as_str()) {
            if let Some(c) = parse_chord(spec) {
                leader = c;
            }
        }
        let overridden: HashMap<Action, Option<Chord>> = overrides
            .iter()
            .filter(|(name, _)| name.as_str() != "leader")
            .filter_map(|(name, v)| {
                let action = ACTION_NAMES
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, a)| *a)?;
                let spec = v.as_str()?;
                if spec.is_empty() || spec.eq_ignore_ascii_case("none") {
                    Some((action, None))
                } else {
                    Some((action, parse_chord(spec)))
                }
            })
            .collect();

        let mut map = HashMap::new();
        for (spec, action) in Self::defaults() {
            if overridden.contains_key(&action) {
                continue;
            }
            if let Some(chord) = parse_chord(spec) {
                map.insert(chord, action);
            }
        }
        for (action, chord) in &overridden {
            if let Some(chord) = chord {
                map.insert(*chord, *action);
            }
        }
        Self { map, leader }
    }

    pub fn lookup(&self, chord: &Chord) -> Option<Action> {
        self.map.get(chord).copied()
    }

    pub fn is_leader(&self, chord: &Chord) -> bool {
        *chord == self.leader
    }

    /// (chord label, action description) pairs for the shortcuts overlay,
    /// deduped to the first (preferred) chord per action, defaults order.
    pub fn describe(&self) -> Vec<(String, &'static str)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut chords: Vec<(&Chord, &Action)> = self.map.iter().collect();
        // Stable-ish ordering: ctrl first, then alt, then super.
        chords.sort_by_key(|(c, _)| (c.mods, format!("{:?}", c.key)));
        for (chord, action) in chords {
            if !seen.insert(*action) {
                continue;
            }
            let desc = match action {
                Action::OpenSettings => "settings",
                Action::OpenNewAgent => "new agents",
                Action::NewTerminal => "new terminal",
                Action::AddProject => "add project",
                Action::OpenMenu => "pane menu",
                Action::OpenShortcuts => "shortcuts",
                Action::RenameFocused => "rename pane",
                Action::HideFocused => "hide/show pane",
                Action::CloseFocused => "close pane",
                Action::ToggleHud => "perf HUD",
                Action::Quit => "detach",
                Action::FocusNext => "next pane",
                Action::FocusPrev => "previous pane",
                Action::ScrollUp => "scroll back",
                Action::ScrollDown => "scroll forward",
            };
            out.push((chord_label(chord), desc));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs() {
        assert_eq!(
            parse_chord("ctrl+,"),
            Some(Chord {
                key: ChordKey::Char(','),
                mods: MOD_CTRL
            })
        );
        assert_eq!(
            parse_chord("alt+s"),
            Some(Chord {
                key: ChordKey::Char('s'),
                mods: MOD_ALT
            })
        );
        assert_eq!(
            parse_chord("super+N"),
            Some(Chord {
                key: ChordKey::Char('N'),
                mods: MOD_SUPER
            })
        );
        assert_eq!(
            parse_chord("f10"),
            Some(Chord {
                key: ChordKey::F(10),
                mods: 0
            })
        );
        assert_eq!(
            parse_chord("ctrl+alt+pageup"),
            Some(Chord {
                key: ChordKey::PageUp,
                mods: MOD_CTRL | MOD_ALT
            })
        );
        assert_eq!(parse_chord("bogus+key"), None);
    }

    #[test]
    fn overrides_replace_defaults() {
        let mut o = serde_json::Map::new();
        o.insert("settings".into(), serde_json::Value::String("f2".into()));
        o.insert("hud".into(), serde_json::Value::String("none".into()));
        o.insert("leader".into(), serde_json::Value::String("ctrl+a".into()));
        let km = Keymap::from_overrides(&o);
        // New binding works; old defaults for that action are gone.
        assert_eq!(
            km.lookup(&parse_chord("f2").unwrap()),
            Some(Action::OpenSettings)
        );
        assert_eq!(km.lookup(&parse_chord("ctrl+,").unwrap()), None);
        assert_eq!(km.lookup(&parse_chord("alt+s").unwrap()), None);
        // Unbound action.
        assert_eq!(km.lookup(&parse_chord("ctrl+y").unwrap()), None);
        // Untouched defaults remain.
        assert_eq!(
            km.lookup(&parse_chord("alt+n").unwrap()),
            Some(Action::OpenNewAgent)
        );
        // Leader rebound.
        assert!(km.is_leader(&parse_chord("ctrl+a").unwrap()));
        assert!(!km.is_leader(&parse_chord("ctrl+b").unwrap()));
    }

    #[test]
    fn default_map() {
        let km = Keymap::from_overrides(&serde_json::Map::new());
        assert_eq!(
            km.lookup(&parse_chord("ctrl+,").unwrap()),
            Some(Action::OpenSettings)
        );
        assert_eq!(
            km.lookup(&parse_chord("alt+s").unwrap()),
            Some(Action::OpenSettings)
        );
        assert!(km.is_leader(&parse_chord("ctrl+b").unwrap()));
        assert!(!km.describe().is_empty());
    }
}
