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

    // Selection
    /// Toggle mark on current item (Space).
    ToggleMark,

    // Clipboard operations (vim-style)
    /// Yank (copy) marked/current to clipboard.
    Yank,
    /// Cut (move) marked/current to clipboard.
    Cut,
    /// Paste from clipboard to current directory.
    Paste,

    // File operations
    /// Delete marked/current items (with confirmation).
    Delete,
    /// Rename current item.
    Rename,
    /// Create new file.
    CreateFile,
    /// Create new directory.
    CreateDirectory,
    /// Create directory and navigate into it (like zsh's `take`).
    Take,
    /// Undo last operation.
    Undo,

    // UI toggles
    ToggleDetails,
    ToggleHelp,
    ToggleTheme,
    /// Toggle between tree and miller layout.
    ToggleLayout,

    // Other actions
    Refresh,
    Search,
    Sort,
    CommandMode,

    // Confirmation
    #[allow(dead_code)]
    Confirm,
    Cancel,
    #[allow(dead_code)]
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
            // Quit - only 'q' quits, Esc is for clearing selection/clipboard
            (KeyCode::Char('q'), KeyModifiers::NONE) => KeyAction::Quit,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => KeyAction::ForceQuit,

            // Cancel/Clear - Esc clears selection, clipboard, or closes dialogs
            (KeyCode::Esc, _) => KeyAction::Cancel,

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
            (KeyCode::Char('g'), KeyModifiers::NONE) => KeyAction::JumpToTop,
            (KeyCode::Char('G'), KeyModifiers::SHIFT) => KeyAction::JumpToBottom,
            (KeyCode::Home, _) => KeyAction::JumpToTop,
            (KeyCode::End, _) => KeyAction::JumpToBottom,

            // Page navigation
            (KeyCode::PageUp, _) => KeyAction::PageUp,
            (KeyCode::PageDown, _) => KeyAction::PageDown,
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => KeyAction::PageUp,
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => KeyAction::PageDown,

            // Selection and marking
            (KeyCode::Char(' '), KeyModifiers::NONE) => KeyAction::ToggleMark,

            // Tree expand/collapse with 'o'
            (KeyCode::Char('o'), KeyModifiers::NONE) => KeyAction::ToggleExpand,

            // Clipboard operations (vim-style)
            (KeyCode::Char('y'), KeyModifiers::NONE) => KeyAction::Yank,
            (KeyCode::Char('x'), KeyModifiers::NONE) => KeyAction::Cut,
            (KeyCode::Char('p'), KeyModifiers::NONE) => KeyAction::Paste,

            // File operations
            (KeyCode::Char('d'), KeyModifiers::NONE) => KeyAction::Delete,
            (KeyCode::Char('D'), KeyModifiers::SHIFT) => KeyAction::Delete,
            (KeyCode::Delete, _) => KeyAction::Delete,
            (KeyCode::Char('r'), KeyModifiers::NONE) => KeyAction::Rename,
            (KeyCode::Char('a'), KeyModifiers::NONE) => KeyAction::CreateFile,
            (KeyCode::Char('A'), KeyModifiers::SHIFT) => KeyAction::CreateDirectory,
            (KeyCode::Char('T'), KeyModifiers::SHIFT) => KeyAction::Take,

            // Undo
            (KeyCode::Char('z'), KeyModifiers::CONTROL) => KeyAction::Undo,

            // UI toggles
            (KeyCode::Char('i'), KeyModifiers::NONE) => KeyAction::ToggleDetails,
            (KeyCode::Char('?'), KeyModifiers::NONE) => KeyAction::ToggleHelp,
            (KeyCode::Char('t'), KeyModifiers::NONE) => KeyAction::ToggleTheme,
            (KeyCode::Char('v'), KeyModifiers::NONE) => KeyAction::ToggleLayout,

            // Refresh (Shift-R since r is rename)
            (KeyCode::Char('R'), KeyModifiers::SHIFT) => KeyAction::Refresh,

            // Search and sort
            (KeyCode::Char('/'), KeyModifiers::NONE) => KeyAction::Search,
            (KeyCode::Char('s'), KeyModifiers::NONE) => KeyAction::Sort,

            // Directory navigation
            (KeyCode::Enter, _) => KeyAction::DrillDown,
            (KeyCode::Backspace, _) => KeyAction::NavigateBack,
            (KeyCode::Char('-'), KeyModifiers::NONE) => KeyAction::NavigateBack,

            // Command palette
            (KeyCode::Char(':'), KeyModifiers::NONE) => KeyAction::CommandMode,
            (KeyCode::Char(':'), KeyModifiers::SHIFT) => KeyAction::CommandMode,

            // Confirmation (for dialogs)
            (KeyCode::Char('n'), KeyModifiers::NONE) => KeyAction::Cancel,
            // ClearMarks available via Escape when items are marked, or :clear command

            // View switching
            (KeyCode::Tab, KeyModifiers::NONE) => KeyAction::NextTab,
            (KeyCode::BackTab, _) => KeyAction::PrevTab,

            _ => KeyAction::None,
        }
    }
}

/// A section of key bindings for the help display.
pub struct HelpSection {
    pub title: &'static str,
    pub bindings: Vec<KeyBinding>,
}

/// Key binding for display in help.
pub struct KeyBinding {
    pub keys: &'static str,
    pub description: &'static str,
}

/// Get all key bindings organized by section for help display.
pub fn get_help_sections() -> Vec<HelpSection> {
    vec![
        HelpSection {
            title: "Navigation",
            bindings: vec![
                KeyBinding { keys: "j/k ↑/↓", description: "Move up/down" },
                KeyBinding { keys: "h/l ←/→", description: "Collapse/expand" },
                KeyBinding { keys: "Enter", description: "Drill into directory" },
                KeyBinding { keys: "Backspace/-", description: "Navigate back" },
                KeyBinding { keys: "g/G", description: "Jump to top/bottom" },
                KeyBinding { keys: "Ctrl-u/d", description: "Page up/down" },
                KeyBinding { keys: "o", description: "Toggle expand node" },
            ],
        },
        HelpSection {
            title: "Selection & Clipboard",
            bindings: vec![
                KeyBinding { keys: "Space", description: "Mark item for multi-select" },
                KeyBinding { keys: "y", description: "Yank (copy) to clipboard" },
                KeyBinding { keys: "x", description: "Cut to clipboard" },
                KeyBinding { keys: "p", description: "Paste from clipboard" },
                KeyBinding { keys: "Esc", description: "Clear clipboard/marks" },
            ],
        },
        HelpSection {
            title: "File Operations",
            bindings: vec![
                KeyBinding { keys: "d/Del", description: "Delete item(s)" },
                KeyBinding { keys: "r", description: "Rename" },
                KeyBinding { keys: "a", description: "Create file (touch)" },
                KeyBinding { keys: "A", description: "Create directory (mkdir)" },
                KeyBinding { keys: "T", description: "Take (mkdir + cd)" },
                KeyBinding { keys: "Ctrl-z", description: "Undo" },
            ],
        },
        HelpSection {
            title: "Views & Display",
            bindings: vec![
                KeyBinding { keys: "Tab/S-Tab", description: "Switch view tab" },
                KeyBinding { keys: "v", description: "Toggle Tree/Miller" },
                KeyBinding { keys: "i", description: "Toggle details panel" },
                KeyBinding { keys: "t", description: "Toggle dark/light theme" },
                KeyBinding { keys: "R", description: "Refresh/rescan" },
            ],
        },
        HelpSection {
            title: "Commands",
            bindings: vec![
                KeyBinding { keys: ":", description: "Open command palette" },
                KeyBinding { keys: "?", description: "Show this help" },
                KeyBinding { keys: "q", description: "Quit" },
            ],
        },
    ]
}

/// Get command palette commands for help display.
pub fn get_command_help() -> Vec<(&'static str, &'static str)> {
    vec![
        (":q :quit", "Quit application"),
        (":cd <path>", "Change directory"),
        (":touch <name>", "Create file"),
        (":mkdir <name>", "Create directory"),
        (":take <name>", "Create dir and cd into it"),
        (":yank :y", "Copy to clipboard"),
        (":cut :x", "Cut to clipboard"),
        (":paste :p", "Paste from clipboard"),
        (":delete :rm", "Delete marked items"),
        (":rename <name>", "Rename current item"),
        (":clear", "Clear all marks"),
        (":theme dark|light", "Set theme"),
        (":layout tree|miller", "Set layout"),
        (":help", "Show help"),
    ]
}

/// Get all key bindings as a flat list (for backwards compatibility).
#[allow(dead_code)]
pub fn get_key_bindings() -> Vec<KeyBinding> {
    get_help_sections()
        .into_iter()
        .flat_map(|s| s.bindings)
        .collect()
}
