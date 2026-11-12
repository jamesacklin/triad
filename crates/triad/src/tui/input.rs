//! The one-line message editor: a string and a cursor. The cursor counts
//! `char`s; display widths come from ratatui's text types (unicode-width).

use ratatui::text::Span;

/// Display width of a string in terminal columns.
pub fn width(s: &str) -> usize {
    Span::raw(s).width()
}

fn char_width(c: char) -> usize {
    let mut buf = [0u8; 4];
    width(c.encode_utf8(&mut buf))
}

#[derive(Default, Debug)]
pub struct Input {
    text: String,
    /// Cursor position in chars, `0..=len`.
    cursor: usize,
}

impl Input {
    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of the char at `i` (or the end).
    fn byte(&self, i: usize) -> usize {
        self.text
            .char_indices()
            .nth(i)
            .map_or(self.text.len(), |(b, _)| b)
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.len() {
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.len();
    }

    /// Ctrl-U: delete everything before the cursor.
    pub fn kill_to_start(&mut self) {
        let at = self.byte(self.cursor);
        self.text.drain(..at);
        self.cursor = 0;
    }

    /// Ctrl-W: delete the word before the cursor, and the spaces after it.
    pub fn kill_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let (a, b) = (self.byte(start), self.byte(self.cursor));
        self.text.drain(a..b);
        self.cursor = start;
    }

    /// Empties the editor and returns what it held.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// The part of the text shown in a box `cols` wide, and the cursor's
    /// column in it. The view scrolls just enough to keep the cursor in.
    pub fn visible(&self, cols: usize) -> (String, usize) {
        if cols == 0 {
            return (String::new(), 0);
        }
        let chars: Vec<char> = self.text.chars().collect();
        // Leftmost char so that the cursor (plus one cell for it) fits.
        let mut start = self.cursor;
        let mut used = 1;
        while start > 0 {
            let w = char_width(chars[start - 1]);
            if used + w > cols {
                break;
            }
            used += w;
            start -= 1;
        }
        let mut shown = String::new();
        let mut col = 0;
        let mut cursor_col = 0;
        for (i, c) in chars.iter().enumerate().skip(start) {
            if i == self.cursor {
                cursor_col = col;
            }
            let w = char_width(*c);
            if col + w > cols {
                break;
            }
            shown.push(*c);
            col += w;
        }
        if self.cursor == chars.len() {
            cursor_col = col;
        }
        (shown, cursor_col.min(cols - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Input {
        let mut i = Input::default();
        for c in s.chars() {
            i.insert(c);
        }
        i
    }

    #[test]
    fn insert_and_move() {
        let mut i = typed("helo");
        i.left();
        i.insert('l');
        assert_eq!(i.text(), "hello");
        i.home();
        i.insert('>');
        i.end();
        i.insert('!');
        assert_eq!(i.text(), ">hello!");
        i.right();
        i.insert('?');
        assert_eq!(i.text(), ">hello!?");
    }

    #[test]
    fn backspace_and_delete() {
        let mut i = typed("abc");
        i.backspace();
        assert_eq!(i.text(), "ab");
        i.home();
        i.backspace();
        assert_eq!(i.text(), "ab");
        i.delete();
        assert_eq!(i.text(), "b");
        i.end();
        i.delete();
        assert_eq!(i.text(), "b");
    }

    #[test]
    fn multibyte_chars() {
        let mut i = typed("héllo wörld 🌱");
        i.backspace();
        i.left();
        i.backspace();
        assert_eq!(i.text(), "héllo wörl ");
        i.home();
        i.right();
        i.delete();
        assert_eq!(i.text(), "hllo wörl ");
    }

    #[test]
    fn kills() {
        let mut i = typed("one two  three");
        i.kill_word();
        assert_eq!(i.text(), "one two  ");
        i.kill_word();
        assert_eq!(i.text(), "one ");
        let mut i = typed("one two");
        i.left();
        i.left();
        i.left();
        i.kill_to_start();
        assert_eq!(i.text(), "two");
        i.end();
        assert_eq!(i.take(), "two");
        assert!(i.is_empty());
        i.insert('x');
        assert_eq!(i.text(), "x");
    }

    #[test]
    fn horizontal_scroll() {
        let i = typed("abcdefghij");
        assert_eq!(i.visible(5), ("ghij".to_string(), 4));
        let mut i = typed("abcdefghij");
        i.home();
        assert_eq!(i.visible(5), ("abcde".to_string(), 0));
        i.right();
        i.right();
        assert_eq!(i.visible(5).1, 2);
        assert_eq!(typed("ab").visible(10), ("ab".to_string(), 2));
        // Wide chars take two columns.
        let i = typed("🌱🌱🌱");
        assert_eq!(i.visible(5), ("🌱🌱".to_string(), 4));
        assert_eq!(Input::default().visible(0), (String::new(), 0));
    }
}
