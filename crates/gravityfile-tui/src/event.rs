//! Event handling for the TUI.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Key action that can be performed in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    // Navigation
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    JumpToTop,
    JumpToBottom,
    PageUp,
    PageDown,

    // Tree operations
    #[allow(dead_code)]
    Expand,
    #[allow(dead_code)]
    Collapse,
    ToggleExpand,

    // Directory navigation
    DrillDown,
    NavigateBack,

    // Actions
    Delete,
    ToggleDetails,
    ToggleHelp,
    ToggleTheme,
    Refresh,
    Search,
    Sort,
    CommandMode,

    // Deletion
    Confirm,
    Cancel,
    ClearMarks,

    // View switching
    NextTab,
    PrevTab,

    // Application
    Quit,
    ForceQuit,

    // No action
    None,
}

impl KeyAction {
    /// Convert a key event to an action.
    pub fn from_key_event(event: KeyEvent) -> Self {
        match (event.code, event.modifiers) {
            // Quit
            (KeyCode::Char('q'), KeyModifiers::NONE) => KeyAction::Quit,
            (KeyCode::Esc, _) => KeyAction::Quit,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => KeyAction::ForceQuit,

            // Navigation - vim style
            (KeyCode::Char('j'), KeyModifiers::NONE) => KeyAction::MoveDown,
            (KeyCode::Char('k'), KeyModifiers::NONE) => KeyAction::MoveUp,
            (KeyCode::Char('h'), KeyModifiers::NONE) => KeyAction::MoveLeft,
            (KeyCode::Char('l'), KeyModifiers::NONE) => KeyAction::MoveRight,

            // Navigation - arrow keys
            (KeyCode::Down, _) => KeyAction::MoveDown,
            (KeyCode::Up, _) => KeyAction::MoveUp,
            (KeyCode::Left, _) => KeyAction::MoveLeft,
            (KeyCode::Right, _) => KeyAction::MoveRight,

            // Jump
            (KeyCode::Char('g'), KeyModifiers::NONE) => KeyAction::JumpToTop, // Simplified: single g
            (KeyCode::Char('G'), KeyModifiers::SHIFT) => KeyAction::JumpToBottom,
            (KeyCode::Home, _) => KeyAction::JumpToTop,
            (KeyCode::End, _) => KeyAction::JumpToBottom,

            // Page navigation
            (KeyCode::PageUp, _) => KeyAction::PageUp,
            (KeyCode::PageDown, _) => KeyAction::PageDown,
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => KeyAction::PageUp,
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => KeyAction::PageDown,

            // Tree operations (Space for toggle, Enter reserved for confirm)
            (KeyCode::Char(' '), KeyModifiers::NONE) => KeyAction::ToggleExpand,
            (KeyCode::Char('o'), KeyModifiers::NONE) => KeyAction::ToggleExpand, // vim-like open

            // Actions
            (KeyCode::Char('d'), KeyModifiers::NONE) => KeyAction::Delete,
            (KeyCode::Delete, _) => KeyAction::Delete,
            (KeyCode::Char('i'), KeyModifiers::NONE) => KeyAction::ToggleDetails,
            (KeyCode::Char('?'), KeyModifiers::NONE) => KeyAction::ToggleHelp,
            (KeyCode::Char('t'), KeyModifiers::NONE) => KeyAction::ToggleTheme,
            (KeyCode::Char('r'), KeyModifiers::NONE) => KeyAction::Refresh,
            (KeyCode::Char('/'), KeyModifiers::NONE) => KeyAction::Search,
            (KeyCode::Char('s'), KeyModifiers::NONE) => KeyAction::Sort,

            // Directory navigation
            (KeyCode::Enter, _) => KeyAction::DrillDown,
            (KeyCode::Backspace, _) => KeyAction::NavigateBack,
            (KeyCode::Char('-'), KeyModifiers::NONE) => KeyAction::NavigateBack, // Alternate for back

            // Command palette
            (KeyCode::Char(':'), KeyModifiers::NONE) => KeyAction::CommandMode,
            (KeyCode::Char(':'), KeyModifiers::SHIFT) => KeyAction::CommandMode,

            // Deletion confirmation
            (KeyCode::Char('y'), KeyModifiers::NONE) => KeyAction::Confirm,
            (KeyCode::Char('n'), KeyModifiers::NONE) => KeyAction::Cancel,
            (KeyCode::Char('x'), KeyModifiers::NONE) => KeyAction::ClearMarks,

            // View switching
            (KeyCode::Tab, KeyModifiers::NONE) => KeyAction::NextTab,
            (KeyCode::BackTab, _) => KeyAction::PrevTab,
            (KeyCode::Char('1'), KeyModifiers::NONE) => KeyAction::NextTab, // Also cycle with number keys
            (KeyCode::Char('2'), KeyModifiers::NONE) => KeyAction::NextTab,
            (KeyCode::Char('3'), KeyModifiers::NONE) => KeyAction::NextTab,

            _ => KeyAction::None,
        }
    }
}

/// Key binding for display in help.
pub struct KeyBinding {
    pub keys: &'static str,
    pub description: &'static str,
}

/// Get all key bindings for help display.
pub fn get_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding {
            keys: "Tab/S-Tab",
            description: "Switch view",
        },
        KeyBinding {
            keys: "j/k, ↑/↓",
            description: "Navigate up/down",
        },
        KeyBinding {
            keys: "h/l, ←/→",
            description: "Collapse/expand",
        },
        KeyBinding {
            keys: "Enter",
            description: "Drill into directory",
        },
        KeyBinding {
            keys: "Backspace",
            description: "Navigate back up",
        },
        KeyBinding {
            keys: "Space/o",
            description: "Toggle expand",
        },
        KeyBinding {
            keys: "g/G",
            description: "Jump to top/bottom",
        },
        KeyBinding {
            keys: "PgUp/PgDn",
            description: "Page up/down",
        },
        KeyBinding {
            keys: "d/Del",
            description: "Mark for deletion",
        },
        KeyBinding {
            keys: "x",
            description: "Clear all marks",
        },
        KeyBinding {
            keys: "y",
            description: "Confirm deletion",
        },
        KeyBinding {
            keys: ":",
            description: "Command palette",
        },
        KeyBinding {
            keys: "i",
            description: "Toggle details",
        },
        KeyBinding {
            keys: "?",
            description: "Toggle help",
        },
        KeyBinding {
            keys: "r",
            description: "Refresh",
        },
        KeyBinding {
            keys: "q/Esc",
            description: "Quit",
        },
    ]
}
