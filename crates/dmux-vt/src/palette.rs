//! Default palette used to answer in-pane color queries (OSC 4/10/11 and
//! friends). Pane apps use these to pick themes (Claude Code queries the
//! background to detect dark mode), so the answers should approximate the
//! host terminal; a host-probe can replace this table later.

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

/// Standard xterm 16-color values.
const ANSI16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0xcd, 0x00, 0x00),
    (0x00, 0xcd, 0x00),
    (0xcd, 0xcd, 0x00),
    (0x00, 0x00, 0xee),
    (0xcd, 0x00, 0xcd),
    (0x00, 0xcd, 0xcd),
    (0xe5, 0xe5, 0xe5),
    (0x7f, 0x7f, 0x7f),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x5c, 0x5c, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

const FOREGROUND: (u8, u8, u8) = (0xd0, 0xd0, 0xd0);
const BACKGROUND: (u8, u8, u8) = (0x1a, 0x1a, 0x1a);

/// Resolve a palette index the way alacritty's color array is laid out:
/// 0..=255 are the xterm palette; the named specials follow.
pub fn color_for(index: usize) -> Rgb {
    let (r, g, b) = if index < 16 {
        ANSI16[index]
    } else if index < 232 {
        // 6x6x6 color cube.
        let i = index - 16;
        let comp = |v: usize| if v == 0 { 0u8 } else { (v * 40 + 55) as u8 };
        (comp(i / 36), comp((i / 6) % 6), comp(i % 6))
    } else if index < 256 {
        // Grayscale ramp.
        let v = (8 + (index - 232) * 10) as u8;
        (v, v, v)
    } else if index == NamedColor::Foreground as usize
        || index == NamedColor::BrightForeground as usize
        || index == NamedColor::DimForeground as usize
        || index == NamedColor::Cursor as usize
    {
        FOREGROUND
    } else if index == NamedColor::Background as usize {
        BACKGROUND
    } else {
        FOREGROUND
    };
    Rgb { r, g, b }
}
