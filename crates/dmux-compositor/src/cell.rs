use bitflags::bitflags;
use unicode_width::UnicodeWidthChar;

use crate::Rect;

/// A terminal color in the emission model: default (inherit terminal scheme),
/// one of the 256 indexed colors, or 24-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct AttrFlags: u16 {
        const BOLD             = 1 << 0;
        const DIM              = 1 << 1;
        const ITALIC           = 1 << 2;
        const UNDERLINE        = 1 << 3;
        const DOUBLE_UNDERLINE = 1 << 4;
        const UNDERCURL        = 1 << 5;
        const DOTTED_UNDERLINE = 1 << 6;
        const DASHED_UNDERLINE = 1 << 7;
        const STRIKEOUT        = 1 << 8;
        const INVERSE          = 1 << 9;
        const HIDDEN           = 1 << 10;
        const BLINK            = 1 << 11;
        /// Continuation cell of a preceding wide grapheme; never emitted directly.
        const WIDE_SPACER      = 1 << 12;
    }
}

/// One screen cell. `ch` is the base character; rare combining marks ride in
/// `zerowidth`. A double-width grapheme occupies its own cell plus one
/// `WIDE_SPACER` cell to its right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub zerowidth: Option<Box<[char]>>,
    pub fg: Color,
    pub bg: Color,
    pub attrs: AttrFlags,
    /// Hyperlink id interned by the app layer (OSC 8). 0 = no link.
    pub link: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            zerowidth: None,
            fg: Color::Default,
            bg: Color::Default,
            attrs: AttrFlags::empty(),
            link: 0,
        }
    }
}

impl Cell {
    pub fn wide_spacer(&self) -> bool {
        self.attrs.contains(AttrFlags::WIDE_SPACER)
    }

    pub fn display_width(&self) -> u16 {
        if self.wide_spacer() {
            0
        } else {
            self.ch.width().unwrap_or(1).max(1) as u16
        }
    }
}

/// A rectangular grid of cells with per-row dirty flags. Two of these form the
/// front/back pair the frame differ works on.
#[derive(Debug, Clone)]
pub struct CellBuffer {
    cells: Vec<Cell>,
    cols: u16,
    rows: u16,
    dirty: Vec<bool>,
}

impl CellBuffer {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cells: vec![Cell::default(); cols as usize * rows as usize],
            cols,
            rows,
            dirty: vec![true; rows as usize],
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.cols, self.rows)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        *self = Self::new(cols, rows);
    }

    #[inline]
    fn idx(&self, col: u16, row: u16) -> usize {
        row as usize * self.cols as usize + col as usize
    }

    #[inline]
    pub fn get(&self, col: u16, row: u16) -> &Cell {
        &self.cells[self.idx(col, row)]
    }

    /// Set a cell, marking the row dirty. Out-of-bounds writes are ignored so
    /// callers can paint through clip rects without pre-checking.
    #[inline]
    pub fn set(&mut self, col: u16, row: u16, cell: Cell) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let i = self.idx(col, row);
        self.cells[i] = cell;
        self.dirty[row as usize] = true;
    }

    pub fn row(&self, row: u16) -> &[Cell] {
        let start = self.idx(0, row);
        &self.cells[start..start + self.cols as usize]
    }

    pub fn row_dirty(&self, row: u16) -> bool {
        self.dirty[row as usize]
    }

    pub fn mark_row_dirty(&mut self, row: u16) {
        if row < self.rows {
            self.dirty[row as usize] = true;
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = true);
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = false);
    }

    /// Fill a rect with a cell (clipped to the buffer).
    pub fn fill(&mut self, rect: Rect, cell: &Cell) {
        let clip = rect.intersect(&self.area());
        for row in clip.y..clip.bottom() {
            for col in clip.x..clip.right() {
                let i = self.idx(col, row);
                self.cells[i] = cell.clone();
            }
            self.dirty[row as usize] = true;
        }
    }

    /// Draw a string at (col,row) with the given style, clipped to `clip`.
    /// Returns the column after the last cell written. Handles wide chars by
    /// writing a WIDE_SPACER continuation cell.
    pub fn draw_text(
        &mut self,
        mut col: u16,
        row: u16,
        text: &str,
        fg: Color,
        bg: Color,
        attrs: AttrFlags,
        clip: Rect,
    ) -> u16 {
        let clip = clip.intersect(&self.area());
        if row < clip.y || row >= clip.bottom() {
            return col;
        }
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0) as u16;
            if w == 0 {
                // Attach combining mark to the previous cell if possible.
                if col > clip.x && col - 1 < self.cols {
                    let i = self.idx(col - 1, row);
                    let cell = &mut self.cells[i];
                    let mut zw: Vec<char> = cell
                        .zerowidth
                        .take()
                        .map(|b| b.into_vec())
                        .unwrap_or_default();
                    zw.push(ch);
                    cell.zerowidth = Some(zw.into_boxed_slice());
                }
                continue;
            }
            if col.saturating_add(w) > clip.right() {
                break;
            }
            if col >= clip.x {
                self.set(
                    col,
                    row,
                    Cell {
                        ch,
                        zerowidth: None,
                        fg,
                        bg,
                        attrs,
                        link: 0,
                    },
                );
                if w == 2 {
                    self.set(
                        col + 1,
                        row,
                        Cell {
                            ch: ' ',
                            zerowidth: None,
                            fg,
                            bg,
                            attrs: attrs | AttrFlags::WIDE_SPACER,
                            link: 0,
                        },
                    );
                }
            }
            col = col.saturating_add(w);
        }
        col
    }
}
