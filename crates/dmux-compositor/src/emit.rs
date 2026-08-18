use std::io::Write;

use crate::{AttrFlags, Cell, Color};

/// Stateful ANSI emitter: tracks the terminal's current SGR state and cursor
/// position so consecutive writes emit the minimum escape traffic. All output
/// goes into an internal `Vec<u8>` the caller drains once per frame.
#[derive(Debug)]
pub struct Emitter {
    buf: Vec<u8>,
    // Terminal-side state we believe to be current. `None` = unknown (forces emission).
    cursor: Option<(u16, u16)>, // (col, row), 0-based
    fg: Option<Color>,
    bg: Option<Color>,
    attrs: Option<AttrFlags>,
    link: Option<u32>,
}

/// Underline styles use SGR 4:x subparams (kitty extension, widely supported);
/// terminals that ignore subparams degrade to plain underline.
const STYLE_ATTRS: AttrFlags = AttrFlags::all().difference(AttrFlags::WIDE_SPACER);

impl Emitter {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(64 * 1024),
            cursor: None,
            fg: None,
            bg: None,
            attrs: None,
            link: None,
        }
    }

    /// Forget all assumed terminal state (e.g. after a resize or reattach).
    pub fn invalidate(&mut self) {
        self.cursor = None;
        self.fg = None;
        self.bg = None;
        self.attrs = None;
        self.link = None;
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn begin_sync(&mut self) {
        self.raw(b"\x1b[?2026h");
    }

    pub fn end_sync(&mut self) {
        self.raw(b"\x1b[?2026l");
    }

    pub fn hide_cursor(&mut self) {
        self.raw(b"\x1b[?25l");
    }

    pub fn show_cursor(&mut self) {
        self.raw(b"\x1b[?25h");
    }

    pub fn clear_screen(&mut self) {
        self.raw(b"\x1b[2J");
        self.cursor = None;
    }

    /// Move the cursor to (col, row), 0-based, emitting nothing if already there.
    pub fn move_to(&mut self, col: u16, row: u16) {
        if self.cursor == Some((col, row)) {
            return;
        }
        // CUP is 1-based.
        let _ = write!(self.buf, "\x1b[{};{}H", row + 1, col + 1);
        self.cursor = Some((col, row));
    }

    /// Emit a cell's grapheme at the current cursor position, updating style
    /// state as needed. Caller is responsible for cursor placement and for
    /// skipping WIDE_SPACER cells.
    pub fn put_cell(&mut self, cell: &Cell) {
        self.set_style(cell.fg, cell.bg, cell.attrs & STYLE_ATTRS);
        self.set_link(cell.link);
        let mut utf8 = [0u8; 4];
        self.buf.extend_from_slice(cell.ch.encode_utf8(&mut utf8).as_bytes());
        if let Some(zw) = &cell.zerowidth {
            for ch in zw.iter() {
                self.buf.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
            }
        }
        if let Some((col, row)) = self.cursor {
            self.cursor = Some((col.saturating_add(cell.display_width()), row));
        }
    }

    pub fn set_style(&mut self, fg: Color, bg: Color, attrs: AttrFlags) {
        let attrs = attrs & STYLE_ATTRS;
        if self.fg == Some(fg) && self.bg == Some(bg) && self.attrs == Some(attrs) {
            return;
        }
        // Reset-and-reapply: correct in all cases and cheap in practice because
        // style runs are long; per-attribute transition encoding is a later
        // optimization the frame stats will justify or kill.
        self.buf.extend_from_slice(b"\x1b[0");
        if attrs.contains(AttrFlags::BOLD) {
            self.buf.extend_from_slice(b";1");
        }
        if attrs.contains(AttrFlags::DIM) {
            self.buf.extend_from_slice(b";2");
        }
        if attrs.contains(AttrFlags::ITALIC) {
            self.buf.extend_from_slice(b";3");
        }
        if attrs.contains(AttrFlags::UNDERLINE) {
            self.buf.extend_from_slice(b";4");
        } else if attrs.contains(AttrFlags::DOUBLE_UNDERLINE) {
            self.buf.extend_from_slice(b";4:2");
        } else if attrs.contains(AttrFlags::UNDERCURL) {
            self.buf.extend_from_slice(b";4:3");
        } else if attrs.contains(AttrFlags::DOTTED_UNDERLINE) {
            self.buf.extend_from_slice(b";4:4");
        } else if attrs.contains(AttrFlags::DASHED_UNDERLINE) {
            self.buf.extend_from_slice(b";4:5");
        }
        if attrs.contains(AttrFlags::BLINK) {
            self.buf.extend_from_slice(b";5");
        }
        if attrs.contains(AttrFlags::INVERSE) {
            self.buf.extend_from_slice(b";7");
        }
        if attrs.contains(AttrFlags::HIDDEN) {
            self.buf.extend_from_slice(b";8");
        }
        if attrs.contains(AttrFlags::STRIKEOUT) {
            self.buf.extend_from_slice(b";9");
        }
        match fg {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => {
                let _ = write!(self.buf, ";{}", 30 + i);
            }
            Color::Indexed(i) if i < 16 => {
                let _ = write!(self.buf, ";{}", 90 + (i - 8));
            }
            Color::Indexed(i) => {
                let _ = write!(self.buf, ";38:5:{i}");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(self.buf, ";38:2::{r}:{g}:{b}");
            }
        }
        match bg {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => {
                let _ = write!(self.buf, ";{}", 40 + i);
            }
            Color::Indexed(i) if i < 16 => {
                let _ = write!(self.buf, ";{}", 100 + (i - 8));
            }
            Color::Indexed(i) => {
                let _ = write!(self.buf, ";48:5:{i}");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(self.buf, ";48:2::{r}:{g}:{b}");
            }
        }
        self.buf.push(b'm');
        self.fg = Some(fg);
        self.bg = Some(bg);
        self.attrs = Some(attrs);
    }

    fn set_link(&mut self, link: u32) {
        if self.link == Some(link) {
            return;
        }
        if link == 0 {
            self.buf.extend_from_slice(b"\x1b]8;;\x1b\\");
        }
        // Non-zero link ids are resolved by the app layer via `emit_link_open`,
        // which owns the id→URI table; here we only track state transitions.
        self.link = Some(link);
    }

    /// Open a hyperlink with an explicit URI (app layer resolves ids to URIs).
    pub fn open_link(&mut self, id: u32, uri: &str) {
        if self.link == Some(id) {
            return;
        }
        let _ = write!(self.buf, "\x1b]8;id={id};{uri}\x1b\\");
        self.link = Some(id);
    }

    pub fn reset_style(&mut self) {
        self.set_style(Color::Default, Color::Default, AttrFlags::empty());
        self.set_link(0);
    }

    /// Set cursor shape via DECSCUSR (0 = default).
    pub fn cursor_shape(&mut self, shape: u8) {
        let _ = write!(self.buf, "\x1b[{} q", shape);
    }

    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new()
    }
}
