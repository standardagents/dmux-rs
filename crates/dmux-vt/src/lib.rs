//! Per-pane terminal emulation: wraps `alacritty_terminal` behind a small
//! surface the compositor and status heuristics consume. One `PaneTerm` per
//! dmux pane; bytes from `%output` go in, cells and side effects come out.

pub mod palette;

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as VtColor, CursorShape, NamedColor, Processor};

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};

/// Things the pane's byte stream asked of the outside world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermSideEffect {
    /// OSC 0/2 title change. Captured for the naming service, never forwarded
    /// to the host terminal.
    Title(String),
    /// The app answered a query (DA1, DSR/CPR, color queries, …). These bytes
    /// MUST be written back into the pane's pty via control-mode send-keys or
    /// TUIs hang probing capabilities.
    PtyResponse(Vec<u8>),
    /// OSC 52 clipboard store.
    Clipboard(String),
    Bell,
    /// Opt-in palette provenance (#75): a pane-local OSC 4/10/11 mutation
    /// changed dynamic palette state. `slot` follows alacritty's layout
    /// (0..=255 indexed, 256 default foreground, 257 default background);
    /// `to = None` is a reset back to the default. Emitted only while
    /// palette tracing is enabled, by diffing dynamic-color state around
    /// each advance — queries never mutate state, so they can never appear
    /// here (that is the exclusion rule). Decoded metadata only: no
    /// surrounding content or raw OSC payloads are retained.
    PaletteChange {
        slot: usize,
        to: Option<(u8, u8, u8)>,
    },
}

/// Dynamic palette slots the #75 trace observes: the xterm 256 palette plus
/// the default foreground/background specials.
pub const PALETTE_TRACE_SLOTS: usize = 258;

/// Snapshot of the input-relevant terminal modes, used by the input router to
/// encode keys/mouse the way the pane app expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputModes {
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub sgr_mouse: bool,
    pub alt_screen: bool,
    pub alternate_scroll: bool,
    pub focus_in_out: bool,
    /// The pane expects all modified keys in an extended, unambiguous form.
    pub extended_keys_mode2: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    /// Viewport-relative (col, row); None when the cursor is hidden or the
    /// view is scrolled back.
    pub position: Option<(u16, u16)>,
    /// DECSCUSR shape code (1/2 block, 3/4 underline, 5/6 beam).
    pub shape: u8,
}

/// Pane damage in viewport rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    None,
    Full,
    Rows(Vec<u16>),
}

#[derive(Default)]
struct EventSink(Mutex<Vec<Event>>);

#[derive(Clone, Default)]
struct EventProxy(Arc<EventSink>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        self.0 .0.lock().unwrap().push(event);
    }
}

/// Parser state for the screen/tmux-specific title sequence `ESC k <name>
/// ST|BEL`. zsh and friends emit it to name tmux windows after the running
/// command; it is NOT a standard VT sequence, so alacritty's parser would
/// print the payload as text (the classic "command echoed twice" artifact).
/// We strip it from the stream and surface the name as a Title side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenTitle {
    Normal,
    /// Saw ESC at a chunk boundary; next byte decides.
    Esc,
    /// Inside `ESC k`, accumulating the name.
    Name(Vec<u8>),
    /// Inside the name, saw ESC (possible ST terminator).
    NameEsc(Vec<u8>),
}

pub struct PaneTerm {
    term: Term<EventProxy>,
    parser: Processor,
    sink: Arc<EventSink>,
    cols: u16,
    rows: u16,
    screen_title: ScreenTitle,
    /// Rough activity meter the status engine reads.
    bytes_seen: u64,
    /// Emit PaletteChange side effects (#75). Off = zero cost beyond this
    /// flag check.
    trace_palette: bool,
}

impl PaneTerm {
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
        let proxy = EventProxy::default();
        let sink = proxy.0.clone();
        let config = Config {
            scrolling_history: scrollback_lines,
            ..Config::default()
        };
        let size = TermSize::new(cols.max(1) as usize, rows.max(1) as usize);
        let term = Term::new(config, &size, proxy);
        Self {
            term,
            parser: Processor::new(),
            sink,
            cols: cols.max(1),
            rows: rows.max(1),
            screen_title: ScreenTitle::Normal,
            bytes_seen: 0,
            trace_palette: false,
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn bytes_seen(&self) -> u64 {
        self.bytes_seen
    }

    /// Feed pane output bytes; returns side effects raised while parsing.
    /// Enable palette-mutation tracing (#75).
    pub fn set_trace_palette(&mut self, on: bool) {
        self.trace_palette = on;
    }

    fn palette_snapshot(&self) -> Vec<Option<(u8, u8, u8)>> {
        let colors = self.term.colors();
        (0..PALETTE_TRACE_SLOTS)
            .map(|i| colors[i].map(|rgb| (rgb.r, rgb.g, rgb.b)))
            .collect()
    }

    pub fn advance(&mut self, bytes: &[u8]) -> Vec<TermSideEffect> {
        self.bytes_seen += bytes.len() as u64;
        let before = if self.trace_palette {
            Some(self.palette_snapshot())
        } else {
            None
        };
        let (filtered, titles) = self.strip_screen_titles(bytes);
        self.parser.advance(&mut self.term, &filtered);
        let mut effects = self.drain_events();
        if let Some(before) = before {
            let after = self.palette_snapshot();
            for (slot, (b, a)) in before.iter().zip(after.iter()).enumerate() {
                if b != a {
                    effects.push(TermSideEffect::PaletteChange { slot, to: *a });
                }
            }
        }
        for title in titles {
            effects.push(TermSideEffect::Title(title));
        }
        effects
    }

    /// Remove `ESC k <name> ST|BEL` sequences (chunk-split safe), collecting
    /// the names. Everything else passes through untouched.
    fn strip_screen_titles(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut out = Vec::with_capacity(bytes.len());
        let mut titles = Vec::new();
        let mut state = std::mem::replace(&mut self.screen_title, ScreenTitle::Normal);
        for &b in bytes {
            state = match state {
                ScreenTitle::Normal => {
                    if b == 0x1b {
                        ScreenTitle::Esc
                    } else {
                        out.push(b);
                        ScreenTitle::Normal
                    }
                }
                ScreenTitle::Esc => {
                    if b == b'k' {
                        ScreenTitle::Name(Vec::new())
                    } else if b == 0x1b {
                        out.push(0x1b);
                        ScreenTitle::Esc
                    } else {
                        out.push(0x1b);
                        out.push(b);
                        ScreenTitle::Normal
                    }
                }
                ScreenTitle::Name(mut name) => {
                    if b == 0x07 {
                        titles.push(String::from_utf8_lossy(&name).into_owned());
                        ScreenTitle::Normal
                    } else if b == 0x1b {
                        ScreenTitle::NameEsc(name)
                    } else {
                        name.push(b);
                        ScreenTitle::Name(name)
                    }
                }
                ScreenTitle::NameEsc(mut name) => {
                    if b == b'\\' {
                        titles.push(String::from_utf8_lossy(&name).into_owned());
                        ScreenTitle::Normal
                    } else {
                        name.push(0x1b);
                        name.push(b);
                        ScreenTitle::Name(name)
                    }
                }
            };
        }
        self.screen_title = state;
        (out, titles)
    }

    fn drain_events(&mut self) -> Vec<TermSideEffect> {
        let events = std::mem::take(&mut *self.sink.0.lock().unwrap());
        let mut out = Vec::new();
        for ev in events {
            match ev {
                Event::Title(t) => out.push(TermSideEffect::Title(t)),
                Event::ResetTitle => out.push(TermSideEffect::Title(String::new())),
                Event::PtyWrite(s) => out.push(TermSideEffect::PtyResponse(s.into_bytes())),
                Event::ClipboardStore(_, s) => out.push(TermSideEffect::Clipboard(s)),
                Event::ColorRequest(index, formatter) => {
                    // tmux answers OSC 10/11 (default fg/bg) queries itself,
                    // synchronously — always ahead of any reply we could
                    // send. A second reply from us lingers in the app's
                    // input queue and corrupts its next read (#4), so those
                    // two are tmux's alone (fed correct values via
                    // window-style at attach). OSC 4 (indexed) and OSC 12
                    // (cursor) tmux stays silent on; we answer those.
                    let fg = NamedColor::Foreground as usize;
                    let bg = NamedColor::Background as usize;
                    if index != fg && index != bg {
                        out.push(TermSideEffect::PtyResponse(
                            formatter(palette::color_for(index)).into_bytes(),
                        ));
                    }
                }
                Event::TextAreaSizeRequest(formatter) => {
                    let size = WindowSize {
                        num_lines: self.rows,
                        num_cols: self.cols,
                        cell_width: 0,
                        cell_height: 0,
                    };
                    out.push(TermSideEffect::PtyResponse(formatter(size).into_bytes()));
                }
                Event::Bell => out.push(TermSideEffect::Bell),
                Event::MouseCursorDirty
                | Event::CursorBlinkingChange
                | Event::Wakeup
                | Event::ClipboardLoad(..)
                | Event::Exit
                | Event::ChildExit(_) => {}
            }
        }
        out
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term
            .resize(TermSize::new(cols as usize, rows as usize));
    }

    /// Take and reset accumulated damage, in viewport row indices.
    pub fn take_damage(&mut self) -> Damage {
        use alacritty_terminal::term::TermDamage;
        let damage = match self.term.damage() {
            TermDamage::Full => Damage::Full,
            TermDamage::Partial(iter) => {
                let rows: Vec<u16> = iter.map(|line| line.line as u16).collect();
                if rows.is_empty() {
                    Damage::None
                } else {
                    Damage::Rows(rows)
                }
            }
        };
        self.term.reset_damage();
        damage
    }

    /// Scroll the view by `delta` lines (positive = into history). Returns the
    /// new display offset.
    pub fn scroll_view(&mut self, delta: i32) -> usize {
        self.term.scroll_display(Scroll::Delta(delta));
        self.display_offset()
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn history_len(&self) -> usize {
        self.term.grid().total_lines() - self.term.grid().screen_lines()
    }

    pub fn input_modes(&self) -> InputModes {
        let mode = *self.term.mode();
        InputModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            mouse_drag: mode.contains(TermMode::MOUSE_DRAG),
            mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            focus_in_out: mode.contains(TermMode::FOCUS_IN_OUT),
            extended_keys_mode2: mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL),
        }
    }

    pub fn cursor(&self) -> CursorState {
        let content = self.term.renderable_content();
        let visible =
            self.term.mode().contains(TermMode::SHOW_CURSOR) && content.display_offset == 0;
        let shape = match content.cursor.shape {
            CursorShape::Block => 2,
            CursorShape::Underline => 4,
            CursorShape::Beam => 6,
            CursorShape::HollowBlock | CursorShape::Hidden => 0,
        };
        let position = if visible && !matches!(content.cursor.shape, CursorShape::Hidden) {
            let p = content.cursor.point;
            if p.line.0 >= 0 {
                Some((p.column.0 as u16, p.line.0 as u16))
            } else {
                None
            }
        } else {
            None
        };
        CursorState { position, shape }
    }

    /// Copy the current viewport (honoring scrollback offset) into `buf` at
    /// `rect`, clipped to both the rect and the buffer. Rows beyond the pane's
    /// size are filled with blanks so stale content never lingers.
    pub fn render_into(&self, buf: &mut CellBuffer, rect: Rect) {
        let clip = rect.intersect(&buf.area());
        if clip.is_empty() {
            return;
        }
        let blank = Cell::default();
        // Clear the target region first: pane rows shorter than the rect and
        // cells past line ends must not show stale frame content.
        buf.fill(clip, &blank);

        let display_offset = self.term.grid().display_offset() as i32;
        let selection = self
            .term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term));
        let grid = self.term.grid();
        let colors = self.term.colors();
        let max_rows = clip.h.min(self.rows);
        let max_cols = clip.w.min(self.cols);
        for vrow in 0..max_rows {
            let line = Line(vrow as i32 - display_offset);
            let row = &grid[line];
            for vcol in 0..max_cols {
                let cell = &row[Column(vcol as usize)];
                if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut converted = convert_cell(cell, colors);
                if let Some(range) = &selection {
                    if range.contains(Point::new(line, Column(vcol as usize))) {
                        converted.attrs.toggle(AttrFlags::INVERSE);
                    }
                }
                buf.set(clip.x + vcol, clip.y + vrow, converted);
            }
        }
    }

    fn viewport_point(&self, col: u16, row: u16) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        Point::new(
            Line(row as i32 - offset),
            Column((col as usize).min(self.cols as usize - 1)),
        )
    }

    /// Begin a text selection at viewport (col, row). Double-click semantics
    /// (word select) use `SelectionType::Semantic`.
    pub fn selection_start(&mut self, col: u16, row: u16, word: bool) {
        let ty = if word {
            SelectionType::Semantic
        } else {
            SelectionType::Simple
        };
        let point = self.viewport_point(col, row);
        self.term.selection = Some(Selection::new(ty, point, Side::Left));
    }

    /// Extend the selection to viewport (col, row).
    pub fn selection_update(&mut self, col: u16, row: u16) {
        let point = self.viewport_point(col, row);
        if let Some(sel) = &mut self.term.selection {
            sel.update(point, Side::Right);
        }
    }

    /// Selected text, if any (empty selections yield None).
    pub fn selection_text(&self) -> Option<String> {
        self.term
            .selection_to_string()
            .filter(|s| !s.trim().is_empty())
    }

    pub fn selection_clear(&mut self) -> bool {
        self.term.selection.take().is_some()
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.is_some()
    }

    /// Search upward through scrollback for `needle` (case-insensitive),
    /// starting above the current view. On a match, scrolls the view so the
    /// matching line is at the top and returns the new display offset.
    pub fn search_back(&mut self, needle: &str) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let needle = needle.to_lowercase();
        let history = self.history_len() as i32;
        let start = self.display_offset() as i32 + 1;
        for d in start..=history {
            let line = Line(-d);
            let text = self.row_text(line).to_lowercase();
            if text.contains(&needle) {
                self.term.scroll_display(Scroll::Bottom);
                self.term.scroll_display(Scroll::Delta(d));
                return Some(self.display_offset());
            }
        }
        None
    }

    /// Dynamic-palette entry (OSC 4/10/11) for a slot, if the pane set one.
    /// Slots 0..=255 are indexed colors; 256/257 are default fg/bg.
    pub fn palette_color(&self, slot: usize) -> Option<(u8, u8, u8)> {
        self.term.colors()[slot].map(|rgb| (rgb.r, rgb.g, rgb.b))
    }

    /// Text of a viewport row (verifier incident dumps).
    pub fn row_text_public(&self, row: u16) -> String {
        self.row_text(Line(row as i32))
    }

    fn row_text(&self, line: Line) -> String {
        let grid = self.term.grid();
        let row = &grid[line];
        let mut text = String::new();
        for col in 0..self.cols {
            let cell = &row[Column(col as usize)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
        }
        text.trim_end().to_string()
    }

    /// Last `n` content lines of the viewport (trailing blank rows trimmed,
    /// like tmux capture-pane) — the input for status heuristics / naming.
    pub fn read_tail_text(&self, n: u16) -> String {
        let grid = self.term.grid();
        let mut lines: Vec<String> = Vec::with_capacity(self.rows as usize);
        for vrow in 0..self.rows {
            let line = Line(vrow as i32);
            let row = &grid[line];
            let mut line_text = String::new();
            for col in 0..self.cols {
                let cell = &row[Column(col as usize)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                line_text.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    line_text.extend(zw);
                }
            }
            lines.push(line_text.trim_end().to_string());
        }
        // Trim trailing blank rows, then keep the last `n` lines.
        let content_end = lines
            .iter()
            .rposition(|l| !l.is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        lines.truncate(content_end);
        let skip = lines.len().saturating_sub(n as usize);
        let mut out = lines[skip..].join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }
}

fn convert_color(color: VtColor) -> Color {
    match color {
        VtColor::Named(named) => match named {
            NamedColor::Foreground
            | NamedColor::Background
            | NamedColor::Cursor
            | NamedColor::BrightForeground
            | NamedColor::DimForeground => Color::Default,
            NamedColor::Black => Color::Indexed(0),
            NamedColor::Red => Color::Indexed(1),
            NamedColor::Green => Color::Indexed(2),
            NamedColor::Yellow => Color::Indexed(3),
            NamedColor::Blue => Color::Indexed(4),
            NamedColor::Magenta => Color::Indexed(5),
            NamedColor::Cyan => Color::Indexed(6),
            NamedColor::White => Color::Indexed(7),
            NamedColor::BrightBlack => Color::Indexed(8),
            NamedColor::BrightRed => Color::Indexed(9),
            NamedColor::BrightGreen => Color::Indexed(10),
            NamedColor::BrightYellow => Color::Indexed(11),
            NamedColor::BrightBlue => Color::Indexed(12),
            NamedColor::BrightMagenta => Color::Indexed(13),
            NamedColor::BrightCyan => Color::Indexed(14),
            NamedColor::BrightWhite => Color::Indexed(15),
            NamedColor::DimBlack => Color::Indexed(0),
            NamedColor::DimRed => Color::Indexed(1),
            NamedColor::DimGreen => Color::Indexed(2),
            NamedColor::DimYellow => Color::Indexed(3),
            NamedColor::DimBlue => Color::Indexed(4),
            NamedColor::DimMagenta => Color::Indexed(5),
            NamedColor::DimCyan => Color::Indexed(6),
            NamedColor::DimWhite => Color::Indexed(7),
        },
        VtColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => Color::Indexed(i),
    }
}

/// Resolve a color through the pane's dynamic palette (OSC 4/10/11):
/// themed shells and editors redefine default/indexed colors per pane, and
/// tmux renders those as explicit RGB — so must we, or the theme is lost.
fn resolve_color(colors: &Colors, color: VtColor) -> Color {
    let slot: Option<usize> = match color {
        VtColor::Named(n) => Some(n as usize),
        VtColor::Indexed(i) => Some(i as usize),
        VtColor::Spec(_) => None,
    };
    if let Some(s) = slot {
        if let Some(rgb) = colors[s] {
            return Color::Rgb(rgb.r, rgb.g, rgb.b);
        }
    }
    convert_color(color)
}

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell, colors: &Colors) -> Cell {
    let mut attrs = AttrFlags::empty();
    let f = cell.flags;
    attrs.set(AttrFlags::BOLD, f.contains(Flags::BOLD));
    attrs.set(AttrFlags::DIM, f.contains(Flags::DIM));
    attrs.set(AttrFlags::ITALIC, f.contains(Flags::ITALIC));
    attrs.set(AttrFlags::UNDERLINE, f.contains(Flags::UNDERLINE));
    attrs.set(
        AttrFlags::DOUBLE_UNDERLINE,
        f.contains(Flags::DOUBLE_UNDERLINE),
    );
    attrs.set(AttrFlags::UNDERCURL, f.contains(Flags::UNDERCURL));
    attrs.set(
        AttrFlags::DOTTED_UNDERLINE,
        f.contains(Flags::DOTTED_UNDERLINE),
    );
    attrs.set(
        AttrFlags::DASHED_UNDERLINE,
        f.contains(Flags::DASHED_UNDERLINE),
    );
    attrs.set(AttrFlags::STRIKEOUT, f.contains(Flags::STRIKEOUT));
    attrs.set(AttrFlags::INVERSE, f.contains(Flags::INVERSE));
    attrs.set(AttrFlags::HIDDEN, f.contains(Flags::HIDDEN));
    attrs.set(AttrFlags::WIDE_SPACER, f.contains(Flags::WIDE_CHAR_SPACER));

    Cell {
        // alacritty records a TAB as the literal '\t' character in the cell
        // it lands on; tmux stores a blank there. A tab is cursor movement,
        // not a glyph — and emitting the raw byte to the host would jump ITS
        // cursor to the next tab stop, corrupting the frame (#46).
        ch: if cell.c == '\t' { ' ' } else { cell.c },
        zerowidth: cell.zerowidth().map(|zw| zw.to_vec().into_boxed_slice()),
        fg: resolve_color(colors, cell.fg),
        bg: resolve_color(colors, cell.bg),
        attrs,
        link: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_text_lands_in_grid() {
        let mut t = PaneTerm::new(20, 4, 100);
        t.advance(b"hello \x1b[1;31mred\x1b[0m\r\nline2");
        let tail = t.read_tail_text(4);
        assert!(tail.contains("hello red"), "tail: {tail:?}");
        assert!(tail.contains("line2"));

        let mut buf = CellBuffer::new(20, 4);
        t.render_into(&mut buf, Rect::new(0, 0, 20, 4));
        assert_eq!(buf.get(0, 0).ch, 'h');
        let red = buf.get(6, 0);
        assert_eq!(red.ch, 'r');
        assert!(red.attrs.contains(AttrFlags::BOLD));
        assert_eq!(red.fg, Color::Indexed(1));
    }

    #[test]
    fn title_and_response_side_effects() {
        let mut t = PaneTerm::new(10, 3, 0);
        let fx = t.advance(b"\x1b]2;my title\x07");
        assert_eq!(fx, vec![TermSideEffect::Title("my title".into())]);

        // DA1 query must produce a PtyResponse.
        let fx = t.advance(b"\x1b[c");
        assert!(
            fx.iter()
                .any(|e| matches!(e, TermSideEffect::PtyResponse(_))),
            "expected DA1 response, got {fx:?}"
        );
    }

    #[test]
    fn default_fg_bg_queries_are_left_to_tmux() {
        // tmux answers OSC 10/11 itself (window-style carries our values);
        // a second reply from us corrupts the app's input queue (#4).
        let mut t = PaneTerm::new(10, 3, 0);
        let fx = t.advance(b"\x1b]10;?\x07\x1b]11;?\x07");
        assert!(
            !fx.iter()
                .any(|e| matches!(e, TermSideEffect::PtyResponse(_))),
            "OSC 10/11 must not be answered by dmux, got {fx:?}"
        );
        // OSC 4 (indexed palette) tmux stays silent on — we must answer.
        let fx = t.advance(b"\x1b]4;1;?\x07");
        assert!(
            fx.iter()
                .any(|e| matches!(e, TermSideEffect::PtyResponse(_))),
            "OSC 4 query must be answered, got {fx:?}"
        );
    }

    #[test]
    fn palette_tracing_reports_mutations_not_queries() {
        // #75: sets and resets for fg/bg/indexed slots surface as
        // PaletteChange; queries never mutate state so they never appear;
        // disabled tracing emits nothing.
        let changes = |fx: &[TermSideEffect]| {
            fx.iter()
                .filter_map(|e| match e {
                    TermSideEffect::PaletteChange { slot, to } => Some((*slot, *to)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let mut t = PaneTerm::new(10, 3, 0);
        t.set_trace_palette(true);
        // OSC 11 set: default background.
        let fx = t.advance(b"\x1b]11;#120f1a\x07");
        assert_eq!(changes(&fx), vec![(257, Some((0x12, 0x0f, 0x1a)))]);
        // OSC 4 set: indexed entry 4.
        let fx = t.advance(b"\x1b]4;4;#ff00aa\x07");
        assert_eq!(changes(&fx), vec![(4, Some((0xff, 0x00, 0xaa)))]);
        // OSC 111 reset: background back to default.
        let fx = t.advance(b"\x1b]111\x07");
        assert_eq!(changes(&fx), vec![(257, None)]);
        // Queries do not mutate: no PaletteChange.
        let fx = t.advance(b"\x1b]10;?\x07");
        assert!(changes(&fx).is_empty(), "queries must not trace: {fx:?}");
        // Disabled tracing: silent even for real mutations.
        let mut off = PaneTerm::new(10, 3, 0);
        let fx = off.advance(b"\x1b]11;#000000\x07");
        assert!(changes(&fx).is_empty());
    }

    #[test]
    fn tabs_render_as_blanks_not_glyphs() {
        // #46: a program emitting TAB must leave blank cells, exactly like
        // tmux's grid — never a literal '\t' glyph the emitter would then
        // write to the host as a cursor jump.
        let mut t = PaneTerm::new(20, 2, 0);
        t.advance(b"a\tb");
        let mut buf = dmux_compositor::CellBuffer::new(20, 2);
        t.render_into(&mut buf, dmux_compositor::Rect::new(0, 0, 20, 2));
        assert_eq!(buf.get(0, 0).ch, 'a');
        for col in 1..8 {
            assert_eq!(buf.get(col, 0).ch, ' ', "col {col} must be blank");
        }
        assert_eq!(buf.get(8, 0).ch, 'b');
    }

    #[test]
    fn cursor_and_modes() {
        let mut t = PaneTerm::new(10, 3, 0);
        t.advance(b"ab");
        assert_eq!(t.cursor().position, Some((2, 0)));
        assert!(!t.input_modes().bracketed_paste);
        t.advance(b"\x1b[?2004h\x1b[?1049h");
        let m = t.input_modes();
        assert!(m.bracketed_paste);
        assert!(m.alt_screen);
    }

    #[test]
    fn scrollback_and_offset() {
        let mut t = PaneTerm::new(10, 2, 100);
        for i in 0..20 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        assert!(t.history_len() > 0);
        let off = t.scroll_view(5);
        assert_eq!(off, 5);
        // Scrolled back: cursor hidden.
        assert_eq!(t.cursor().position, None);
        t.scroll_to_bottom();
        assert_eq!(t.display_offset(), 0);
    }

    #[test]
    fn wide_chars_render_with_spacer() {
        let mut t = PaneTerm::new(10, 2, 0);
        t.advance("漢字".as_bytes());
        let mut buf = CellBuffer::new(10, 2);
        t.render_into(&mut buf, Rect::new(0, 0, 10, 2));
        assert_eq!(buf.get(0, 0).ch, '漢');
        assert!(buf.get(1, 0).wide_spacer());
        assert_eq!(buf.get(2, 0).ch, '字');
    }

    #[test]
    fn damage_partial_then_reset() {
        let mut t = PaneTerm::new(10, 4, 0);
        t.advance(b"x");
        assert!(!matches!(t.take_damage(), Damage::None));
        assert!(matches!(t.take_damage(), Damage::None | Damage::Rows(_)));
    }

    #[test]
    fn screen_title_sequence_stripped_not_printed() {
        // `ESC k name ESC \` (zsh's tmux window-naming) must never print.
        let mut t = PaneTerm::new(40, 3, 0);
        let fx = t.advance(b"before \x1bkecho\x1b\\AFTER");
        let tail = t.read_tail_text(3);
        assert!(tail.contains("before AFTER"), "tail: {tail:?}");
        assert!(
            !tail.contains("echo"),
            "title payload leaked into grid: {tail:?}"
        );
        assert!(
            fx.contains(&TermSideEffect::Title("echo".into())),
            "effects: {fx:?}"
        );
    }

    #[test]
    fn screen_title_split_across_chunks() {
        let mut t = PaneTerm::new(40, 3, 0);
        t.advance(b"x\x1b");
        t.advance(b"kssh-clip");
        let fx = t.advance(b"board\x1b\\y");
        assert!(fx.contains(&TermSideEffect::Title("ssh-clipboard".into())));
        let tail = t.read_tail_text(3);
        assert!(tail.contains("xy"), "tail: {tail:?}");
        assert!(!tail.contains("ssh"), "leak: {tail:?}");
    }

    #[test]
    fn screen_title_bel_terminated_and_esc_passthrough() {
        let mut t = PaneTerm::new(40, 3, 0);
        let fx = t.advance(b"\x1bktitle\x07ok \x1b[1mBOLD");
        assert!(fx.contains(&TermSideEffect::Title("title".into())));
        let tail = t.read_tail_text(3);
        assert!(
            tail.contains("ok BOLD"),
            "CSI after title must still work: {tail:?}"
        );
    }

    #[test]
    fn render_offset_rect() {
        let mut t = PaneTerm::new(5, 2, 0);
        t.advance(b"ab\r\ncd");
        let mut buf = CellBuffer::new(20, 10);
        t.render_into(&mut buf, Rect::new(3, 4, 5, 2));
        assert_eq!(buf.get(3, 4).ch, 'a');
        assert_eq!(buf.get(4, 5).ch, 'd');
        // Outside the rect untouched.
        assert_eq!(buf.get(0, 0).ch, ' ');
    }
}

#[cfg(test)]
mod selection_safety_tests {
    use super::*;
    use dmux_compositor::{CellBuffer, Rect};

    #[test]
    fn selection_beyond_grid_is_safe() {
        // #18: the on-screen rect can be larger than the tmux pane, so drag
        // coordinates can exceed the emulator grid. Out-of-grid points must
        // clamp, not panic, through every selection consumer.
        let mut t = PaneTerm::new(10, 5, 100);
        t.advance(b"hello\r\nworld\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\n");
        t.selection_start(3, 2, false);
        t.selection_update(50, 200);
        let _ = t.selection_text();
        let mut buf = CellBuffer::new(10, 5);
        t.render_into(&mut buf, Rect::new(0, 0, 10, 5));
        // Scrolled view: points still clamp inside history bounds.
        t.scroll_view(3);
        t.selection_start(0, 0, false);
        t.selection_update(9, 4);
        let _ = t.selection_text();
        // Word-select (semantic) at an out-of-grid point.
        t.selection_clear();
        t.selection_start(50, 200, true);
        t.selection_update(60, 250);
        let _ = t.selection_text();
        t.render_into(&mut buf, Rect::new(0, 0, 10, 5));
    }
}
