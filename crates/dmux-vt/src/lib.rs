//! Per-pane terminal emulation: wraps `alacritty_terminal` behind a small
//! surface the compositor and status heuristics consume. One `PaneTerm` per
//! dmux pane; bytes from `%output` go in, cells and side effects come out.

mod palette;

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
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
}

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
    pub kitty_keyboard: bool,
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
}

impl PaneTerm {
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Self {
        let proxy = EventProxy::default();
        let sink = proxy.0.clone();
        let config = Config { scrolling_history: scrollback_lines, ..Config::default() };
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
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<TermSideEffect> {
        self.bytes_seen += bytes.len() as u64;
        let (filtered, titles) = self.strip_screen_titles(bytes);
        self.parser.advance(&mut self.term, &filtered);
        let mut effects = self.drain_events();
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
                    out.push(TermSideEffect::PtyResponse(formatter(palette::color_for(index)).into_bytes()));
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
        self.term.resize(TermSize::new(cols as usize, rows as usize));
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
            kitty_keyboard: mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL),
        }
    }

    pub fn cursor(&self) -> CursorState {
        let content = self.term.renderable_content();
        let visible = self.term.mode().contains(TermMode::SHOW_CURSOR) && content.display_offset == 0;
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
        let grid = self.term.grid();
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
                let converted = convert_cell(cell);
                buf.set(clip.x + vcol, clip.y + vrow, converted);
            }
        }
    }

    /// Last `n` viewport lines as trimmed text (status heuristics / naming).
    pub fn read_tail_text(&self, n: u16) -> String {
        let grid = self.term.grid();
        let rows = self.rows;
        let start = rows.saturating_sub(n);
        let mut out = String::new();
        for vrow in start..rows {
            let line = Line(vrow as i32);
            let row = &grid[line];
            let mut line_text = String::new();
            for col in 0..self.cols {
                let cell = &row[Column(col as usize)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                line_text.push(cell.c);
                if let Some(zw) = cell.zerowidth() {
                    line_text.extend(zw);
                }
            }
            let trimmed = line_text.trim_end();
            out.push_str(trimmed);
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

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    let mut attrs = AttrFlags::empty();
    let f = cell.flags;
    attrs.set(AttrFlags::BOLD, f.contains(Flags::BOLD));
    attrs.set(AttrFlags::DIM, f.contains(Flags::DIM));
    attrs.set(AttrFlags::ITALIC, f.contains(Flags::ITALIC));
    attrs.set(AttrFlags::UNDERLINE, f.contains(Flags::UNDERLINE));
    attrs.set(AttrFlags::DOUBLE_UNDERLINE, f.contains(Flags::DOUBLE_UNDERLINE));
    attrs.set(AttrFlags::UNDERCURL, f.contains(Flags::UNDERCURL));
    attrs.set(AttrFlags::DOTTED_UNDERLINE, f.contains(Flags::DOTTED_UNDERLINE));
    attrs.set(AttrFlags::DASHED_UNDERLINE, f.contains(Flags::DASHED_UNDERLINE));
    attrs.set(AttrFlags::STRIKEOUT, f.contains(Flags::STRIKEOUT));
    attrs.set(AttrFlags::INVERSE, f.contains(Flags::INVERSE));
    attrs.set(AttrFlags::HIDDEN, f.contains(Flags::HIDDEN));
    attrs.set(AttrFlags::WIDE_SPACER, f.contains(Flags::WIDE_CHAR_SPACER));

    Cell {
        ch: cell.c,
        zerowidth: cell.zerowidth().map(|zw| zw.to_vec().into_boxed_slice()),
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
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
            fx.iter().any(|e| matches!(e, TermSideEffect::PtyResponse(_))),
            "expected DA1 response, got {fx:?}"
        );
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
        assert!(!tail.contains("echo"), "title payload leaked into grid: {tail:?}");
        assert!(fx.contains(&TermSideEffect::Title("echo".into())), "effects: {fx:?}");
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
        assert!(tail.contains("ok BOLD"), "CSI after title must still work: {tail:?}");
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
