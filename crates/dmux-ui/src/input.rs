use dmux_compositor::{AttrFlags, Cell, CellBuffer, Rect};
use unicode_width::UnicodeWidthChar;

use crate::Theme;

/// Single-line text input with cursor, horizontal scroll, and a placeholder.
/// The one text-entry widget every overlay shares.
#[derive(Debug, Default, Clone)]
pub struct TextInput {
    pub value: String,
    /// Byte offset of the cursor within `value` (always on a char boundary).
    pub cursor: usize,
    pub placeholder: String,
    scroll_cols: u16,
    scroll_rows: u16,
}

#[derive(Debug)]
struct WrappedGlyph {
    text: String,
    row: u16,
    col: u16,
}

#[derive(Debug)]
struct WrappedText {
    glyphs: Vec<WrappedGlyph>,
    cursor_row: u16,
    cursor_col: u16,
    rows: u16,
}

pub enum InputKey {
    Char(char),
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    DeleteWordBack,
    KillToEnd,
    KillToStart,
}

impl TextInput {
    pub fn with_value(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            ..Self::default()
        }
    }

    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }

    pub fn handle(&mut self, key: InputKey) {
        match key {
            InputKey::Char(c) => {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            InputKey::Left => {
                if let Some((i, _)) = self.value[..self.cursor].char_indices().next_back() {
                    self.cursor = i;
                }
            }
            InputKey::Right => {
                if let Some(c) = self.value[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            InputKey::Home => self.cursor = 0,
            InputKey::End => self.cursor = self.value.len(),
            InputKey::Backspace => {
                if let Some((i, _)) = self.value[..self.cursor].char_indices().next_back() {
                    self.value.remove(i);
                    self.cursor = i;
                }
            }
            InputKey::Delete => {
                if self.cursor < self.value.len() {
                    self.value.remove(self.cursor);
                }
            }
            InputKey::DeleteWordBack => {
                let head = &self.value[..self.cursor];
                let trimmed = head.trim_end();
                let cut = trimmed
                    .rfind(char::is_whitespace)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                self.value.replace_range(cut..self.cursor, "");
                self.cursor = cut;
            }
            InputKey::KillToEnd => self.value.truncate(self.cursor),
            InputKey::KillToStart => {
                self.value.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
        }
    }

    /// Number of visual rows used when wrapped within a field of `rect_width`.
    pub fn wrapped_line_count(&self, rect_width: u16) -> u16 {
        wrap_text(
            &self.value,
            self.cursor,
            rect_width.saturating_sub(2).max(1),
        )
        .rows
    }

    /// Draw into `rect` (single row). Returns the screen column of the cursor
    /// so the caller can place the hardware cursor when focused.
    pub fn draw(
        &mut self,
        buf: &mut CellBuffer,
        rect: Rect,
        theme: &Theme,
        focused: bool,
    ) -> Option<(u16, u16)> {
        if rect.is_empty() {
            return None;
        }
        let bg = if focused {
            theme.bg_selected
        } else {
            theme.bg_raised
        };
        buf.fill(
            Rect::new(rect.x, rect.y, rect.w, 1),
            &Cell {
                bg,
                ..Cell::default()
            },
        );

        if self.value.is_empty() {
            buf.draw_text(
                rect.x + 1,
                rect.y,
                &self.placeholder,
                theme.text_faint,
                bg,
                AttrFlags::ITALIC,
                rect,
            );
            return focused.then_some((rect.x + 1, rect.y));
        }

        // Horizontal scroll: keep the cursor inside the visible window.
        let cursor_cols: u16 = self.value[..self.cursor]
            .chars()
            .map(|c| c.width().unwrap_or(0) as u16)
            .sum();
        let inner_w = rect.w.saturating_sub(2);
        if cursor_cols < self.scroll_cols {
            self.scroll_cols = cursor_cols;
        } else if cursor_cols >= self.scroll_cols + inner_w {
            self.scroll_cols = cursor_cols + 1 - inner_w;
        }

        let mut x = rect.x + 1;
        let mut cols = 0u16;
        let mut cursor_screen = None;
        for (i, c) in self.value.char_indices() {
            let w = c.width().unwrap_or(0) as u16;
            if cols + w > self.scroll_cols && x < rect.right() - 1 {
                if i == self.cursor {
                    cursor_screen = Some((x, rect.y));
                }
                x = buf.draw_text(
                    x,
                    rect.y,
                    &c.to_string(),
                    theme.text,
                    bg,
                    AttrFlags::empty(),
                    rect,
                );
            }
            cols += w;
        }
        if self.cursor >= self.value.len() {
            cursor_screen = Some((x.min(rect.right() - 1), rect.y));
        }
        focused.then_some(cursor_screen).flatten()
    }

    /// Draw a visually wrapped field. Explicit newlines start new rows, and
    /// the vertical window follows the cursor when the content exceeds `rect`.
    pub fn draw_wrapped(
        &mut self,
        buf: &mut CellBuffer,
        rect: Rect,
        theme: &Theme,
        focused: bool,
    ) -> Option<(u16, u16)> {
        if rect.is_empty() {
            return None;
        }
        let bg = if focused {
            theme.bg_selected
        } else {
            theme.bg_raised
        };
        buf.fill(
            rect,
            &Cell {
                bg,
                ..Cell::default()
            },
        );

        let content = Rect::new(rect.x + 1, rect.y, rect.w.saturating_sub(2), rect.h);
        if content.is_empty() {
            return None;
        }
        if self.value.is_empty() {
            buf.draw_text(
                content.x,
                content.y,
                &self.placeholder,
                theme.text_faint,
                bg,
                AttrFlags::ITALIC,
                content,
            );
            return focused.then_some((content.x, content.y));
        }

        let wrapped = wrap_text(&self.value, self.cursor, content.w);
        if wrapped.cursor_row < self.scroll_rows {
            self.scroll_rows = wrapped.cursor_row;
        } else if wrapped.cursor_row >= self.scroll_rows + content.h {
            self.scroll_rows = wrapped.cursor_row + 1 - content.h;
        }
        let visible_end = self.scroll_rows + content.h;
        for glyph in wrapped
            .glyphs
            .iter()
            .filter(|glyph| glyph.row >= self.scroll_rows && glyph.row < visible_end)
        {
            buf.draw_text(
                content.x + glyph.col,
                content.y + glyph.row - self.scroll_rows,
                &glyph.text,
                theme.text,
                bg,
                AttrFlags::empty(),
                content,
            );
        }

        focused.then_some((
            content.x + wrapped.cursor_col.min(content.w.saturating_sub(1)),
            content.y + wrapped.cursor_row - self.scroll_rows,
        ))
    }
}

fn wrap_text(value: &str, cursor: usize, width: u16) -> WrappedText {
    let width = width.max(1);
    let mut glyphs: Vec<WrappedGlyph> = Vec::new();
    let mut row = 0u16;
    let mut col = 0u16;
    let mut cursor_position = None;

    for (index, character) in value.char_indices() {
        if index == cursor {
            cursor_position = Some((row, col));
        }
        if character == '\n' {
            row += 1;
            col = 0;
            continue;
        }

        let character_width = character.width().unwrap_or(0) as u16;
        if character_width == 0 {
            if let Some(glyph) = glyphs.last_mut() {
                glyph.text.push(character);
            }
            continue;
        }
        if col > 0 && col.saturating_add(character_width) > width {
            row += 1;
            col = 0;
        }
        glyphs.push(WrappedGlyph {
            text: character.to_string(),
            row,
            col,
        });
        col = col.saturating_add(character_width.min(width));
        if col >= width {
            row += 1;
            col = 0;
        }
    }
    let (cursor_row, cursor_col) = cursor_position.unwrap_or((row, col));
    WrappedText {
        glyphs,
        cursor_row,
        cursor_col,
        rows: row.max(cursor_row) + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut t = TextInput::default();
        for c in "hello".chars() {
            t.handle(InputKey::Char(c));
        }
        t.handle(InputKey::Left);
        t.handle(InputKey::Backspace);
        assert_eq!(t.value, "helo");
        t.handle(InputKey::End);
        t.handle(InputKey::DeleteWordBack);
        assert_eq!(t.value, "");
    }

    #[test]
    fn unicode_cursor_moves() {
        let mut t = TextInput::with_value("héllo");
        t.handle(InputKey::Home);
        t.handle(InputKey::Right);
        t.handle(InputKey::Right);
        t.handle(InputKey::Backspace);
        assert_eq!(t.value, "hllo");
    }

    #[test]
    fn wrapped_input_respects_visual_wraps_and_newlines() {
        let mut input = TextInput::with_value("alpha beta\ngamma");
        let mut buf = CellBuffer::new(8, 3);
        let theme = Theme::default();
        let cursor = input.draw_wrapped(&mut buf, Rect::new(0, 0, 8, 3), &theme, true);

        let row = |y| (0..8).map(|x| buf.get(x, y).ch).collect::<String>();
        assert!(row(0).contains("alpha"));
        assert!(row(1).contains("beta"));
        assert!(row(2).contains("gamma"));
        assert_eq!(cursor, Some((6, 2)));
        assert_eq!(input.wrapped_line_count(8), 3);
    }

    #[test]
    fn wrapped_input_scrolls_to_keep_the_cursor_visible() {
        let mut input = TextInput::with_value("one\ntwo\nthree");
        let mut buf = CellBuffer::new(10, 2);
        let theme = Theme::default();
        let cursor = input.draw_wrapped(&mut buf, Rect::new(0, 0, 10, 2), &theme, true);

        assert_eq!(input.scroll_rows, 1);
        assert_eq!(cursor, Some((6, 1)));
        let row = |y| (0..10).map(|x| buf.get(x, y).ch).collect::<String>();
        assert!(row(0).contains("two"));
        assert!(row(1).contains("three"));
    }
}
