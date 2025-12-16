//! Command palette handling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::View;

/// Command input state.
#[derive(Debug, Clone, Default)]
pub struct CommandInput {
    /// Input buffer.
    buffer: String,
    /// Cursor position.
    cursor: usize,
}

impl CommandInput {
    /// Create a new empty command input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear the input buffer.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Get the current input buffer.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Get the cursor position.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Handle a key event, returning whether to execute the command.
    pub fn handle_key(&mut self, key: KeyEvent) -> CommandKeyResult {
        match (key.code, key.modifiers) {
            // Execute command on Enter
            (KeyCode::Enter, _) => {
                let cmd = self.buffer.clone();
                self.clear();
                CommandKeyResult::Execute(cmd)
            }
            // Cancel on Escape
            (KeyCode::Esc, _) => {
                self.clear();
                CommandKeyResult::Cancel
            }
            // Delete char before cursor
            (KeyCode::Backspace, _) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.buffer.remove(self.cursor);
                    CommandKeyResult::Continue
                } else if self.buffer.is_empty() {
                    CommandKeyResult::Cancel
                } else {
                    CommandKeyResult::Continue
                }
            }
            // Delete char at cursor
            (KeyCode::Delete, _) => {
                if self.cursor < self.buffer.len() {
                    self.buffer.remove(self.cursor);
                }
                CommandKeyResult::Continue
            }
            // Move cursor
            (KeyCode::Left, _) => {
                self.cursor = self.cursor.saturating_sub(1);
                CommandKeyResult::Continue
            }
            (KeyCode::Right, _) => {
                self.cursor = (self.cursor + 1).min(self.buffer.len());
                CommandKeyResult::Continue
            }
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.cursor = 0;
                CommandKeyResult::Continue
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.cursor = self.buffer.len();
                CommandKeyResult::Continue
            }
            // Clear line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.clear();
                CommandKeyResult::Continue
            }
            // Type character
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.buffer.insert(self.cursor, c);
                self.cursor += 1;
                CommandKeyResult::Continue
            }
            _ => CommandKeyResult::Continue,
        }
    }
}

/// Result of handling a key in command mode.
#[derive(Debug, Clone)]
pub enum CommandKeyResult {
    /// Continue accepting input.
    Continue,
    /// Cancel command mode.
    Cancel,
    /// Execute the given command string.
    Execute(String),
}

/// Action to perform after executing a command.
#[derive(Debug, Clone)]
pub enum CommandAction {
    /// No action.
    None,
    /// Quit the application.
    Quit,
    /// Refresh/rescan.
    Refresh,
    /// Navigate to a path.
    NavigateTo(String),
    /// Go to root.
    GoToRoot,
    /// Navigate back.
    NavigateBack,
    /// Show help.
    ShowHelp,
    /// Switch to a view.
    SwitchView(View),
    /// Clear deletion marks.
    ClearMarks,
    /// Toggle details panel.
    ToggleDetails,
    /// Set theme.
    SetTheme(ThemeCommand),
    /// Set layout.
    SetLayout(LayoutCommand),
    /// Set sort mode.
    SetSort(SortCommand),

    // File operations
    /// Yank (copy) marked items to clipboard.
    Yank,
    /// Cut marked items to clipboard.
    Cut,
    /// Paste clipboard contents.
    Paste,
    /// Delete marked items.
    Delete,
    /// Rename current item.
    Rename(Option<String>),
    /// Create a new file.
    CreateFile(Option<String>),
    /// Create a new directory.
    CreateDirectory(Option<String>),
    /// Create a directory and navigate into it (like zsh's `take`).
    Take(Option<String>),
    /// Undo last operation.
    Undo,
}

/// Theme command variants.
#[derive(Debug, Clone, Copy)]
pub enum ThemeCommand {
    Dark,
    Light,
    Toggle,
}

/// Layout command variants.
#[derive(Debug, Clone, Copy)]
pub enum LayoutCommand {
    Tree,
    Miller,
    Toggle,
}

/// Sort command variants.
#[derive(Debug, Clone, Copy)]
pub enum SortCommand {
    SizeDesc,
    SizeAsc,
    NameAsc,
    NameDesc,
    DateDesc,
    DateAsc,
    CountDesc,
    CountAsc,
    Cycle,
    Reverse,
}

/// Parse and execute a command string.
pub fn parse_command(cmd: &str) -> CommandAction {
    let parts: Vec<&str> = cmd.trim().split_whitespace().collect();
    if parts.is_empty() {
        return CommandAction::None;
    }

    match parts[0] {
        // Quit commands
        "q" | "quit" | "exit" => CommandAction::Quit,

        // Refresh/rescan
        "r" | "refresh" | "rescan" => CommandAction::Refresh,

        // Change directory
        "cd" => {
            if parts.len() > 1 {
                CommandAction::NavigateTo(parts[1..].join(" "))
            } else {
                CommandAction::GoToRoot
            }
        }

        // Go to root
        "root" | "top" => CommandAction::GoToRoot,

        // Go back
        "back" | "up" | ".." => CommandAction::NavigateBack,

        // Help
        "help" | "?" => CommandAction::ShowHelp,

        // View switching
        "explorer" | "e" | "tree" => CommandAction::SwitchView(View::Explorer),
        "duplicates" | "dups" | "d" => CommandAction::SwitchView(View::Duplicates),
        "age" | "a" => CommandAction::SwitchView(View::Age),
        "errors" | "err" => CommandAction::SwitchView(View::Errors),

        // Clear marks
        "clear" | "unmark" => CommandAction::ClearMarks,

        // Toggle details
        "details" | "info" | "i" => CommandAction::ToggleDetails,

        // Theme commands
        "theme" | "t" => {
            if parts.len() > 1 {
                match parts[1] {
                    "dark" => CommandAction::SetTheme(ThemeCommand::Dark),
                    "light" => CommandAction::SetTheme(ThemeCommand::Light),
                    "toggle" => CommandAction::SetTheme(ThemeCommand::Toggle),
                    _ => CommandAction::None,
                }
            } else {
                CommandAction::SetTheme(ThemeCommand::Toggle)
            }
        }
        "dark" => CommandAction::SetTheme(ThemeCommand::Dark),
        "light" => CommandAction::SetTheme(ThemeCommand::Light),

        // Layout commands
        "layout" | "view" => {
            if parts.len() > 1 {
                match parts[1] {
                    "tree" => CommandAction::SetLayout(LayoutCommand::Tree),
                    "miller" | "columns" => CommandAction::SetLayout(LayoutCommand::Miller),
                    "toggle" => CommandAction::SetLayout(LayoutCommand::Toggle),
                    _ => CommandAction::None,
                }
            } else {
                CommandAction::SetLayout(LayoutCommand::Toggle)
            }
        }
        "miller" | "columns" => CommandAction::SetLayout(LayoutCommand::Miller),

        // Sort commands
        "sort" | "s" => {
            if parts.len() > 1 {
                match parts[1].to_lowercase().as_str() {
                    "size" | "sz" => CommandAction::SetSort(SortCommand::SizeDesc),
                    "size-" | "sz-" | "size-desc" => CommandAction::SetSort(SortCommand::SizeDesc),
                    "size+" | "sz+" | "size-asc" => CommandAction::SetSort(SortCommand::SizeAsc),
                    "name" | "nm" => CommandAction::SetSort(SortCommand::NameAsc),
                    "name-" | "nm-" | "name-desc" => CommandAction::SetSort(SortCommand::NameDesc),
                    "name+" | "nm+" | "name-asc" => CommandAction::SetSort(SortCommand::NameAsc),
                    "date" | "dt" | "modified" | "mod" => CommandAction::SetSort(SortCommand::DateDesc),
                    "date-" | "dt-" | "date-desc" => CommandAction::SetSort(SortCommand::DateDesc),
                    "date+" | "dt+" | "date-asc" => CommandAction::SetSort(SortCommand::DateAsc),
                    "count" | "ct" | "children" => CommandAction::SetSort(SortCommand::CountDesc),
                    "count-" | "ct-" | "count-desc" => CommandAction::SetSort(SortCommand::CountDesc),
                    "count+" | "ct+" | "count-asc" => CommandAction::SetSort(SortCommand::CountAsc),
                    "reverse" | "rev" => CommandAction::SetSort(SortCommand::Reverse),
                    _ => CommandAction::SetSort(SortCommand::Cycle),
                }
            } else {
                CommandAction::SetSort(SortCommand::Cycle)
            }
        }

        // File operations
        "yank" | "y" | "copy" | "cp" => CommandAction::Yank,
        "cut" | "x" => CommandAction::Cut,
        "paste" | "p" => CommandAction::Paste,
        "delete" | "del" | "rm" => CommandAction::Delete,

        // Rename
        "rename" | "mv" => {
            if parts.len() > 1 {
                CommandAction::Rename(Some(parts[1..].join(" ")))
            } else {
                CommandAction::Rename(None)
            }
        }

        // Create file
        "touch" | "new" | "create" => {
            if parts.len() > 1 {
                CommandAction::CreateFile(Some(parts[1..].join(" ")))
            } else {
                CommandAction::CreateFile(None)
            }
        }

        // Create directory
        "mkdir" | "md" => {
            if parts.len() > 1 {
                CommandAction::CreateDirectory(Some(parts[1..].join(" ")))
            } else {
                CommandAction::CreateDirectory(None)
            }
        }

        // Take (create directory and cd into it)
        "take" => {
            if parts.len() > 1 {
                CommandAction::Take(Some(parts[1..].join(" ")))
            } else {
                CommandAction::Take(None)
            }
        }

        // Undo
        "undo" | "u" => CommandAction::Undo,

        _ => CommandAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_quit() {
        assert!(matches!(parse_command("q"), CommandAction::Quit));
        assert!(matches!(parse_command("quit"), CommandAction::Quit));
        assert!(matches!(parse_command("exit"), CommandAction::Quit));
    }

    #[test]
    fn test_parse_cd() {
        assert!(matches!(parse_command("cd"), CommandAction::GoToRoot));
        match parse_command("cd /some/path") {
            CommandAction::NavigateTo(path) => assert_eq!(path, "/some/path"),
            _ => panic!("Expected NavigateTo"),
        }
    }

    #[test]
    fn test_parse_view() {
        assert!(matches!(
            parse_command("explorer"),
            CommandAction::SwitchView(View::Explorer)
        ));
        assert!(matches!(
            parse_command("d"),
            CommandAction::SwitchView(View::Duplicates)
        ));
    }
}
