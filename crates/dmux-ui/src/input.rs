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
        Self { cursor: value.len(), value, ..Self::default() }
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
                let cut = trimmed.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
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

    /// Draw into `rect` (single row). Returns the screen column of the cursor
    /// so the caller can place the hardware cursor when focused.
    pub fn draw(&mut self, buf: &mut CellBuffer, rect: Rect, theme: &Theme, focused: bool) -> Option<(u16, u16)> {
        if rect.is_empty() {
            return None;
        }
        let bg = if focused { theme.bg_selected } else { theme.bg_raised };
        buf.fill(Rect::new(rect.x, rect.y, rect.w, 1), &Cell { bg, ..Cell::default() });

        if self.value.is_empty() {
            buf.draw_text(rect.x + 1, rect.y, &self.placeholder, theme.text_faint, bg, AttrFlags::ITALIC, rect);
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
                x = buf.draw_text(x, rect.y, &c.to_string(), theme.text, bg, AttrFlags::empty(), rect);
            }
            cols += w;
        }
        if self.cursor >= self.value.len() {
            cursor_screen = Some((x.min(rect.right() - 1), rect.y));
        }
        focused.then(|| cursor_screen).flatten()
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
}
