//! Input state for text input modes (rename, create).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

/// State for text input operations.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// The current input buffer.
    buffer: String,
    /// UTF-8 byte offset at a grapheme boundary within the buffer.
    cursor: usize,
    /// Validation error message.
    error: Option<String>,
    /// Path context (parent directory for create, full path for rename).
    pub context_path: Option<std::path::PathBuf>,
}

impl InputState {
    /// Create a new empty input state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an input state with an initial value (for rename).
    pub fn with_initial(value: &str) -> Self {
        Self {
            buffer: value.to_string(),
            cursor: value.len(),
            error: None,
            context_path: None,
        }
    }

    /// Get the current buffer contents.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Get the cursor position.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Get the current error message (if any).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Set an error message.
    pub fn set_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    /// Clear the error message.
    pub fn clear_error(&mut self) {
        self.error = None;
    }

    /// Handle a key event.
    ///
    /// Returns the result of handling the key.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputResult {
        self.clear_error();

        match (key.code, key.modifiers) {
            // Submit
            (KeyCode::Enter, _) => {
                let value = self.buffer.clone();
                InputResult::Submit(value)
            }

            // Cancel
            (KeyCode::Esc, _) => InputResult::Cancel,

            // Backspace - delete character before cursor
            (KeyCode::Backspace, _) => {
                if self.cursor > 0 {
                    let start = self.previous_boundary();
                    self.buffer.replace_range(start..self.cursor, "");
                    self.cursor = start;
                    self.snap_cursor_forward();
                }
                InputResult::Continue
            }

            // Delete - delete character at cursor
            (KeyCode::Delete, _) => {
                if self.cursor < self.buffer.len() {
                    let end = self.next_boundary();
                    self.buffer.replace_range(self.cursor..end, "");
                    self.snap_cursor_forward();
                }
                InputResult::Continue
            }

            // Left arrow - move cursor left
            (KeyCode::Left, _) => {
                self.cursor = self.previous_boundary();
                InputResult::Continue
            }

            // Right arrow - move cursor right
            (KeyCode::Right, _) => {
                self.cursor = self.next_boundary();
                InputResult::Continue
            }

            // Home or Ctrl-A - move to start
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.cursor = 0;
                InputResult::Continue
            }

            // End or Ctrl-E - move to end
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.cursor = self.buffer.len();
                InputResult::Continue
            }

            // Ctrl-U - clear line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.buffer.clear();
                self.cursor = 0;
                InputResult::Continue
            }

            // Ctrl-K - delete from cursor to end
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.buffer.truncate(self.cursor);
                InputResult::Continue
            }

            // Ctrl-W - delete word before cursor
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                if self.cursor > 0 {
                    let mut start = self.cursor;
                    let mut found_word = false;
                    for (index, grapheme) in self.buffer[..self.cursor].grapheme_indices(true).rev()
                    {
                        let whitespace = grapheme.chars().all(char::is_whitespace);
                        if found_word && whitespace {
                            break;
                        }
                        found_word |= !whitespace;
                        start = index;
                    }
                    self.buffer.replace_range(start..self.cursor, "");
                    self.cursor = start;
                    self.snap_cursor_forward();
                }
                InputResult::Continue
            }

            // Regular character input
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.buffer.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                self.snap_cursor_forward();
                InputResult::Continue
            }

            // Ignore other keys
            _ => InputResult::Continue,
        }
    }

    /// Validate the buffer as a filename.
    ///
    /// Returns Ok(()) if valid, Err with message if invalid.
    pub fn validate_filename(&self) -> Result<(), String> {
        gravityfile_ops::validate_filename(&self.buffer)
    }

    fn previous_boundary(&self) -> usize {
        self.buffer
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .take_while(|&index| index < self.cursor)
            .last()
            .unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.buffer
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|&index| index > self.cursor)
            .unwrap_or(self.buffer.len())
    }

    fn snap_cursor_forward(&mut self) {
        self.cursor = self
            .buffer
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|&index| index >= self.cursor)
            .unwrap_or(self.buffer.len());
    }
}

/// Result of handling input.
#[derive(Debug, Clone)]
pub enum InputResult {
    /// Continue accepting input.
    Continue,
    /// User cancelled the input.
    Cancel,
    /// User submitted the input with this value.
    Submit(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn unicode_editing_preserves_grapheme_boundaries() {
        use ratatui::{buffer::Buffer, layout::Rect, style::Modifier, widgets::Widget};
        for value in ["é日", "e\u{301}x", "👩‍💻🇺🇸1️⃣", "a\u{2003}word  "] {
            let mut input = InputState::new();
            for character in value.chars() {
                input.handle_key(key_event(KeyCode::Char(character), KeyModifiers::NONE));
                assert!(input.buffer.is_char_boundary(input.cursor));
                for width in [0, 1, 6, 20, 80] {
                    let area = Rect::new(0, 0, width, 24);
                    let mut buffer = Buffer::empty(area);
                    crate::ui::modals::InputModal::new(
                        &crate::theme::Theme::default(),
                        &input,
                        "Rename",
                        "Name",
                    )
                    .render(area, &mut buffer);
                }
            }
            assert_eq!(input.buffer(), value);
            let boundaries: Vec<_> = value
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .chain(std::iter::once(value.len()))
                .collect();
            for &boundary in boundaries.iter().rev().skip(1) {
                input.handle_key(key_event(KeyCode::Left, KeyModifiers::NONE));
                assert_eq!(input.cursor(), boundary);
            }
            for &boundary in boundaries.iter().skip(1) {
                input.handle_key(key_event(KeyCode::Right, KeyModifiers::NONE));
                assert_eq!(input.cursor(), boundary);
            }
            for remaining in (0..value.graphemes(true).count()).rev() {
                input.handle_key(key_event(KeyCode::Backspace, KeyModifiers::NONE));
                assert_eq!(input.buffer.graphemes(true).count(), remaining);
            }
            let mut input = InputState::with_initial(value);
            input.handle_key(key_event(KeyCode::Home, KeyModifiers::NONE));
            for remaining in (0..value.graphemes(true).count()).rev() {
                input.handle_key(key_event(KeyCode::Delete, KeyModifiers::NONE));
                assert_eq!(input.buffer.graphemes(true).count(), remaining);
            }
        }
        let mut input = InputState::with_initial("é\u{2003}word  ");
        input.handle_key(key_event(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(input.buffer(), "é\u{2003}");
        input.handle_key(key_event(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(input.buffer().is_empty());
        let mut input = InputState::with_initial("🇺x🇸");
        input.handle_key(key_event(KeyCode::Left, KeyModifiers::NONE));
        input.handle_key(key_event(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.buffer(), "🇺🇸");
        assert_eq!(input.cursor(), input.buffer().len());
        let mut input = InputState::with_initial("é日");
        input.handle_key(key_event(KeyCode::Left, KeyModifiers::NONE));
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        crate::ui::modals::InputModal::new(
            &crate::theme::Theme::default(),
            &input,
            "Rename",
            "Name",
        )
        .render(area, &mut buffer);
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.symbol() == "日" && cell.modifier.contains(Modifier::REVERSED))
        );
    }

    #[test]
    fn test_input_basic() {
        let mut input = InputState::new();

        // Type "test"
        input.handle_key(key_event(KeyCode::Char('t'), KeyModifiers::NONE));
        input.handle_key(key_event(KeyCode::Char('e'), KeyModifiers::NONE));
        input.handle_key(key_event(KeyCode::Char('s'), KeyModifiers::NONE));
        input.handle_key(key_event(KeyCode::Char('t'), KeyModifiers::NONE));

        assert_eq!(input.buffer(), "test");
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn test_input_backspace() {
        let mut input = InputState::with_initial("test");

        input.handle_key(key_event(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(input.buffer(), "tes");
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn test_input_cursor_movement() {
        let mut input = InputState::with_initial("test");

        // Move to start
        input.handle_key(key_event(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(input.cursor(), 0);

        // Move to end
        input.handle_key(key_event(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(input.cursor(), 4);

        // Move left
        input.handle_key(key_event(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(input.cursor(), 3);

        // Move right
        input.handle_key(key_event(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn test_validate_filename() {
        let mut input = InputState::new();

        // Valid names
        input.buffer = "test.txt".to_string();
        assert!(input.validate_filename().is_ok());

        input.buffer = ".hidden".to_string();
        assert!(input.validate_filename().is_ok());

        input.buffer = "my-file_v2".to_string();
        assert!(input.validate_filename().is_ok());

        // Invalid names
        input.buffer = "".to_string();
        assert!(input.validate_filename().is_err());

        input.buffer = "test/file".to_string();
        assert!(input.validate_filename().is_err());

        input.buffer = ".".to_string();
        assert!(input.validate_filename().is_err());

        input.buffer = "..".to_string();
        assert!(input.validate_filename().is_err());
    }

    #[test]
    fn test_submit_and_cancel() {
        let mut input = InputState::with_initial("test");

        let result = input.handle_key(key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(result, InputResult::Submit(s) if s == "test"));

        let result = input.handle_key(key_event(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(result, InputResult::Cancel));
    }
}
