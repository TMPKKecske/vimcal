//! Reusable, Unicode-safe, single-line Vim-like editor for *every* text field.
//! The caret is a character index (never a byte offset).
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode { Normal, Insert }

#[derive(Clone, Debug)]
pub struct VimInput {
    pub mode: InputMode,
    pub cursor: usize,
    pending: Option<char>,
    yank: String,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
}

impl VimInput {
    pub fn new(value: &str) -> Self {
        Self { mode: InputMode::Insert, cursor: value.chars().count(), pending: None,
            yank: String::new(), undo: Vec::new(), redo: Vec::new() }
    }
    pub fn switch_field(&mut self, value: &str) {
        self.cursor = if self.mode == InputMode::Normal {
            value.chars().count().saturating_sub(1)
        } else { value.chars().count() };
        self.pending = None;
        self.undo.clear();
        self.redo.clear();
    }
    pub fn cursor_for(&self, value: &str) -> usize {
        self.cursor.min(value.chars().count())
    }
    fn save_undo(&mut self, value: &str) {
        self.undo.push((value.to_owned(), self.cursor));
        if self.undo.len() > 100 { self.undo.remove(0); }
        self.redo.clear();
    }
    fn idx(value: &str, at: usize) -> usize {
        value.char_indices().nth(at).map(|(i, _)| i).unwrap_or(value.len())
    }
    fn len(value: &str) -> usize { value.chars().count() }
    fn delete_span(&mut self, value: &mut String, from: usize, to: usize) {
        let a = from.min(Self::len(value));
        let b = to.min(Self::len(value));
        if a >= b { return; }
        self.save_undo(value);
        self.yank = value[Self::idx(value, a)..Self::idx(value, b)].into();
        let begin = Self::idx(value, a);
        let end = Self::idx(value, b);
        value.replace_range(begin..end, "");
        self.cursor = a.min(Self::len(value));
    }
    fn prev_word(value: &str, from: usize) -> usize {
        let chars: Vec<char> = value.chars().collect();
        let mut n = from.min(chars.len());
        while n > 0 && !chars[n - 1].is_alphanumeric() && chars[n - 1] != '_' { n -= 1; }
        while n > 0 && (chars[n - 1].is_alphanumeric() || chars[n - 1] == '_') { n -= 1; }
        n
    }
    fn next_word(value: &str, from: usize) -> usize {
        let chars: Vec<char> = value.chars().collect();
        let mut n = from.min(chars.len());
        if n < chars.len() && (chars[n].is_alphanumeric() || chars[n] == '_') {
            while n < chars.len() && (chars[n].is_alphanumeric() || chars[n] == '_') { n += 1; }
        }
        while n < chars.len() && !chars[n].is_alphanumeric() && chars[n] != '_' { n += 1; }
        n
    }
    fn insert(&mut self, value: &mut String, text: &str) {
        self.save_undo(value);
        let at = Self::idx(value, self.cursor);
        value.insert_str(at, text);
        self.cursor += text.chars().count();
    }
    pub fn handle(&mut self, value: &mut String, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => { let cursor = self.cursor; self.delete_span(value, 0, cursor); return true; }
                KeyCode::Char('w') => {
                    let start = Self::prev_word(value, self.cursor);
                    let cursor = self.cursor; self.delete_span(value, start, cursor); return true;
                }
                KeyCode::Char('r') => {
                    if let Some((next, at)) = self.redo.pop() {
                        self.undo.push((value.clone(), self.cursor));
                        *value = next; self.cursor = at;
                    }
                    return true;
                }
                _ => return false,
            }
        }
        let len = Self::len(value);
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.cursor = self.cursor.min(len.saturating_sub(1));
                self.pending = None;
                return true;
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                self.pending = None; return true;
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(if self.mode == InputMode::Normal {
                    len.saturating_sub(1) } else { len });
                self.pending = None; return true;
            }
            KeyCode::Home => { self.cursor = 0; self.pending = None; return true; }
            KeyCode::End => {
                self.cursor = if self.mode == InputMode::Normal { len.saturating_sub(1) } else { len };
                self.pending = None; return true;
            }
            KeyCode::Backspace if self.mode == InputMode::Insert => {
                let cursor = self.cursor;
                if cursor > 0 { self.delete_span(value, cursor - 1, cursor); }
                return true;
            }
            KeyCode::Delete if self.mode == InputMode::Insert => {
                let cursor = self.cursor;
                self.delete_span(value, cursor, cursor + 1); return true;
            }
            _ => {}
        }
        if self.mode == InputMode::Insert {
            if let KeyCode::Char(c) = key.code {
                if key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) { return false; }
                self.insert(value, &c.to_string());
                return true;
            }
            return false;
        }
        // Normal mode: movement and operators mirror common single-line Vim commands.
        if let Some(op) = self.pending.take() {
            match (op, key.code) {
                ('d', KeyCode::Char('d')) | ('c', KeyCode::Char('c')) => {
                    self.delete_span(value, 0, len);
                    if op == 'c' { self.mode = InputMode::Insert; }
                    return true;
                }
                ('d' | 'c', KeyCode::Char('w')) => {
                    let to = Self::next_word(value, self.cursor);
                    let cursor = self.cursor;
                    self.delete_span(value, cursor, to);
                    if op == 'c' { self.mode = InputMode::Insert; }
                    return true;
                }
                ('d' | 'c', KeyCode::Char('$')) => {
                    let cursor = self.cursor;
                    self.delete_span(value, cursor, len);
                    if op == 'c' { self.mode = InputMode::Insert; }
                    return true;
                }
                ('g', KeyCode::Char('g')) => { self.cursor = 0; return true; }
                _ => {} // Unknown combinations safely cancel the pending operator.
            }
        }
        match key.code {
            KeyCode::Char('h') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('l') => self.cursor = (self.cursor + 1).min(len.saturating_sub(1)),
            KeyCode::Char('0') => self.cursor = 0,
            KeyCode::Char('$') => self.cursor = len.saturating_sub(1),
            KeyCode::Char('w') => self.cursor = Self::next_word(value, self.cursor).min(len.saturating_sub(1)),
            KeyCode::Char('b') => self.cursor = Self::prev_word(value, self.cursor),
            KeyCode::Char('i') => self.mode = InputMode::Insert,
            KeyCode::Char('a') => { self.cursor = (self.cursor + 1).min(len); self.mode = InputMode::Insert; }
            KeyCode::Char('I') => { self.cursor = 0; self.mode = InputMode::Insert; }
            KeyCode::Char('A') => { self.cursor = len; self.mode = InputMode::Insert; }
            KeyCode::Char('x') => {
                let cursor = self.cursor;
                self.delete_span(value, cursor, cursor + 1);
                self.cursor = self.cursor.min(Self::len(value).saturating_sub(1));
            }
            KeyCode::Char('X') => {
                let cursor = self.cursor;
                if cursor > 0 { self.delete_span(value, cursor - 1, cursor); }
            }
            KeyCode::Char('D') => { let cursor = self.cursor; self.delete_span(value, cursor, len); },
            KeyCode::Char('C') => {
                let cursor = self.cursor;
                    self.delete_span(value, cursor, len); self.mode = InputMode::Insert;
            }
            KeyCode::Char('s') => {
                let cursor = self.cursor;
                self.delete_span(value, cursor, cursor + 1);
                self.mode = InputMode::Insert;
            }
            KeyCode::Char('d' | 'c' | 'g') => {
                if let KeyCode::Char(c) = key.code { self.pending = Some(c); }
            }
            KeyCode::Char('u') => {
                if let Some((prev, at)) = self.undo.pop() {
                    self.redo.push((value.clone(), self.cursor));
                    *value = prev; self.cursor = at.min(Self::len(value).saturating_sub(1));
                }
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                if !self.yank.is_empty() {
                    self.cursor += if key.code == KeyCode::Char('p') && len > 0 { 1 } else { 0 };
                    self.cursor = self.cursor.min(len);
                    let text = self.yank.clone();
                    self.insert(value, &text);
                    self.cursor = self.cursor.saturating_sub(1);
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }
    fn send(input: &mut VimInput, s: &mut String, code: KeyCode) { input.handle(s, key(code)); }

    #[test]
    fn inserts_in_middle_and_backspaces_at_cursor() {
        let mut s = "I love Rst".to_owned();
        let mut input = VimInput::new(&s);
        send(&mut input, &mut s, KeyCode::Left);
        send(&mut input, &mut s, KeyCode::Left);
        send(&mut input, &mut s, KeyCode::Char('u'));
        assert_eq!(s, "I love Rust");
        send(&mut input, &mut s, KeyCode::Backspace);
        assert_eq!(s, "I love Rst");
    }
    #[test]
    fn vim_normal_mode_words_and_insert() {
        let mut s = "red green blue".to_owned();
        let mut input = VimInput::new(&s);
        send(&mut input, &mut s, KeyCode::Esc);
        send(&mut input, &mut s, KeyCode::Char('0'));
        send(&mut input, &mut s, KeyCode::Char('w'));
        send(&mut input, &mut s, KeyCode::Char('c'));
        send(&mut input, &mut s, KeyCode::Char('w'));
        for c in "yellow ".chars() { send(&mut input, &mut s, KeyCode::Char(c)); }
        send(&mut input, &mut s, KeyCode::Esc);
        assert_eq!(s, "red yellow blue");
    }
    #[test]
    fn unicode_safe_cursor_and_undo() {
        let mut s = "café 🌸".to_owned();
        let mut input = VimInput::new(&s);
        send(&mut input, &mut s, KeyCode::Left);
        send(&mut input, &mut s, KeyCode::Char('!'));
        assert_eq!(s, "café !🌸");
        send(&mut input, &mut s, KeyCode::Esc);
        send(&mut input, &mut s, KeyCode::Char('u'));
        assert_eq!(s, "café 🌸");
    }
}
