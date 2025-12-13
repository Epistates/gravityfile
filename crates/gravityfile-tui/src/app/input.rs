//! Input state for text input modes (rename, create).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// State for text input operations.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// The current input buffer.
    buffer: String,
    /// Cursor position within the buffer.
    cursor: usize,
    /// Original value (for rename operations to show what's being renamed).
    original: Option<String>,
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
            original: Some(value.to_string()),
            error: None,
            context_path: None,
        }
    }

    /// Create an input state with context path.
    pub fn with_context(context_path: std::path::PathBuf) -> Self {
        Self {
            context_path: Some(context_path),
            ..Default::default()
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

    /// Get the original value (if any).
    pub fn original(&self) -> Option<&str> {
        self.original.as_deref()
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
                    self.cursor -= 1;
                    self.buffer.remove(self.cursor);
                }
                InputResult::Continue
            }

            // Delete - delete character at cursor
            (KeyCode::Delete, _) => {
                if self.cursor < self.buffer.len() {
                    self.buffer.remove(self.cursor);
                }
                InputResult::Continue
            }

            // Left arrow - move cursor left
            (KeyCode::Left, _) => {
                self.cursor = self.cursor.saturating_sub(1);
                InputResult::Continue
            }

            // Right arrow - move cursor right
            (KeyCode::Right, _) => {
                self.cursor = (self.cursor + 1).min(self.buffer.len());
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
                    // Find the start of the word
                    let before = &self.buffer[..self.cursor];
                    let word_start = before
                        .rfind(|c: char| c.is_whitespace())
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    self.buffer.replace_range(word_start..self.cursor, "");
                    self.cursor = word_start;
                }
                InputResult::Continue
            }

            // Regular character input
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.buffer.insert(self.cursor, c);
                self.cursor += 1;
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
        let name = &self.buffer;

        if name.is_empty() {
            return Err("Name cannot be empty".into());
        }

        if name.len() > 255 {
            return Err("Name is too long (max 255 characters)".into());
        }

        // Check for invalid characters (Unix)
        if name.contains('/') {
            return Err("Name cannot contain '/'".into());
        }

        if name.contains('\0') {
            return Err("Name cannot contain null character".into());
        }

        // Check for . and .. which are reserved
        if name == "." || name == ".." {
            return Err("'.' and '..' are reserved names".into());
        }

        // Check for leading/trailing spaces (problematic)
        if name.starts_with(' ') || name.ends_with(' ') {
            return Err("Name cannot start or end with spaces".into());
        }

        // Check for trailing dots (problematic on Windows)
        if name.ends_with('.') && name != "." {
            return Err("Name cannot end with a dot".into());
        }

        Ok(())
    }

    /// Check if the buffer has changed from the original value.
    pub fn has_changed(&self) -> bool {
        self.original.as_deref() != Some(&self.buffer)
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
