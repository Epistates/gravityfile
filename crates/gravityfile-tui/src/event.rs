//! Event handling for the TUI.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

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
    /// Open file with configured opener (Enter on file).
    #[allow(dead_code)]
    OpenFile,

    // Selection
    /// Toggle mark on current item (Space).
    ToggleMark,
    /// Enter visual selection mode (Shift+V).
    EnterVisual,
    /// Clear all marks.
    ClearMarks,

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
    /// Cycle preview mode (auto/text/hex/metadata).
    CyclePreviewMode,
    /// Open settings modal.
    OpenSettings,

    // Other actions
    Refresh,
    Search,
    Sort,
    ReverseSort,
    CommandMode,
    /// Go to a specific directory (opens path input).
    GoTo,

    // Confirmation
    /// Explicit confirmation action (e.g. mapped from 'y' via dedicated confirm logic).
    #[allow(dead_code)]
    Confirm,
    Cancel,

    // View switching (for View enum: Explorer, Duplicates, Age, Errors)
    NextView,
    PrevView,

    // Bookmarks
    /// Set a bookmark (m<key>).
    SetBookmark,
    /// Jump to a bookmark ('<key>).
    JumpToBookmark,
    /// Show bookmark list modal.
    ShowBookmarkList,

    // Directory tabs (multiple directory contexts)
    /// Create a new directory tab.
    NewDirTab,
    /// Close the current directory tab.
    CloseDirTab,
    /// Switch to next directory tab.
    NextDirTab,
    /// Switch to previous directory tab.
    PrevDirTab,
    /// Switch to directory tab 1-9 (Alt+1 through Alt+9).
    DirTab(u8),

    // Application
    Quit,
    ForceQuit,

    // No action
    None,
}

/// Mouse action that can be performed in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Left click at a position.
    Click { x: u16, y: u16 },
    /// Double click at a position.
    /// Reserved for future double-click detection with timing logic.
    #[allow(dead_code)]
    DoubleClick { x: u16, y: u16 },
    /// Right click at a position.
    RightClick { x: u16, y: u16 },
    /// Scroll up at a position.
    ScrollUp { x: u16, y: u16 },
    /// Scroll down at a position.
    ScrollDown { x: u16, y: u16 },
    /// Drag started at a position.
    /// Reserved for future drag-and-drop support.
    #[allow(dead_code)]
    DragStart { x: u16, y: u16 },
    /// Drag continued to a position.
    DragTo { x: u16, y: u16 },
    /// Drag ended.
    DragEnd,
    /// No action.
    None,
}

impl MouseAction {
    /// Convert a mouse event to an action.
    pub fn from_mouse_event(event: MouseEvent) -> Self {
        let x = event.column;
        let y = event.row;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => MouseAction::Click { x, y },
            MouseEventKind::Down(MouseButton::Right) => MouseAction::RightClick { x, y },
            MouseEventKind::Up(_) => MouseAction::DragEnd,
            MouseEventKind::Drag(MouseButton::Left) => MouseAction::DragTo { x, y },
            MouseEventKind::ScrollUp => MouseAction::ScrollUp { x, y },
            MouseEventKind::ScrollDown => MouseAction::ScrollDown { x, y },
            _ => MouseAction::None,
        }
    }
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

            // Navigation - arrow keys (without modifiers; Ctrl+arrows for tabs)
            (KeyCode::Down, KeyModifiers::NONE) => KeyAction::MoveDown,
            (KeyCode::Up, KeyModifiers::NONE) => KeyAction::MoveUp,
            (KeyCode::Left, KeyModifiers::NONE) => KeyAction::MoveLeft,
            (KeyCode::Right, KeyModifiers::NONE) => KeyAction::MoveRight,

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
            (KeyCode::Char('V'), KeyModifiers::SHIFT) => KeyAction::EnterVisual,
            (KeyCode::Char('u'), KeyModifiers::NONE) => KeyAction::ClearMarks,

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
            (KeyCode::Char('P'), KeyModifiers::SHIFT) => KeyAction::CyclePreviewMode,
            (KeyCode::Char('v'), KeyModifiers::NONE) => KeyAction::ToggleLayout,
            (KeyCode::Char(','), KeyModifiers::NONE) => KeyAction::OpenSettings,

            // Refresh (Shift-R since r is rename)
            (KeyCode::Char('R'), KeyModifiers::SHIFT) => KeyAction::Refresh,

            // Search and sort
            (KeyCode::Char('/'), KeyModifiers::NONE) => KeyAction::Search,
            (KeyCode::Char('s'), KeyModifiers::NONE) => KeyAction::Sort,
            (KeyCode::Char('S'), KeyModifiers::SHIFT) => KeyAction::ReverseSort,

            // Directory navigation
            (KeyCode::Enter, _) => KeyAction::DrillDown,
            (KeyCode::Backspace, _) => KeyAction::NavigateBack,
            (KeyCode::Char('-'), KeyModifiers::NONE) => KeyAction::NavigateBack,

            // Command palette
            (KeyCode::Char(':'), KeyModifiers::NONE) => KeyAction::CommandMode,
            (KeyCode::Char(':'), KeyModifiers::SHIFT) => KeyAction::CommandMode,

            // Go to directory
            (KeyCode::Char('g'), KeyModifiers::CONTROL) => KeyAction::GoTo,

            // Bookmarks
            (KeyCode::Char('m'), KeyModifiers::NONE) => KeyAction::SetBookmark,
            (KeyCode::Char('\''), KeyModifiers::NONE) => KeyAction::JumpToBookmark,
            (KeyCode::Char('`'), KeyModifiers::NONE) => KeyAction::JumpToBookmark,
            (KeyCode::Char('B'), KeyModifiers::SHIFT) => KeyAction::ShowBookmarkList,

            // Confirmation (for dialogs)
            (KeyCode::Char('n'), KeyModifiers::NONE) => KeyAction::Cancel,
            // ClearMarks available via Escape when items are marked, or :clear command

            // View switching (Explorer, Duplicates, Age, Errors)
            (KeyCode::Tab, KeyModifiers::NONE) => KeyAction::NextView,
            (KeyCode::BackTab, _) => KeyAction::PrevView,

            // Directory tabs
            (KeyCode::Char('t'), KeyModifiers::CONTROL) => KeyAction::NewDirTab,
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => KeyAction::CloseDirTab,
            (KeyCode::Char(']'), KeyModifiers::NONE) => KeyAction::NextDirTab,
            (KeyCode::Char('['), KeyModifiers::NONE) => KeyAction::PrevDirTab,
            // Direct tab switching: 1-9 keys (only when multiple tabs exist)
            (KeyCode::Char('1'), KeyModifiers::NONE) => KeyAction::DirTab(1),
            (KeyCode::Char('2'), KeyModifiers::NONE) => KeyAction::DirTab(2),
            (KeyCode::Char('3'), KeyModifiers::NONE) => KeyAction::DirTab(3),
            (KeyCode::Char('4'), KeyModifiers::NONE) => KeyAction::DirTab(4),
            (KeyCode::Char('5'), KeyModifiers::NONE) => KeyAction::DirTab(5),
            (KeyCode::Char('6'), KeyModifiers::NONE) => KeyAction::DirTab(6),
            (KeyCode::Char('7'), KeyModifiers::NONE) => KeyAction::DirTab(7),
            (KeyCode::Char('8'), KeyModifiers::NONE) => KeyAction::DirTab(8),
            (KeyCode::Char('9'), KeyModifiers::NONE) => KeyAction::DirTab(9),

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
                KeyBinding {
                    keys: "j/k ↑/↓",
                    description: "Move up/down",
                },
                KeyBinding {
                    keys: "h/l ←/→",
                    description: "Collapse/expand",
                },
                KeyBinding {
                    keys: "Enter",
                    description: "Drill into directory",
                },
                KeyBinding {
                    keys: "Backspace/-",
                    description: "Navigate back",
                },
                KeyBinding {
                    keys: "g/G",
                    description: "Jump to top/bottom",
                },
                KeyBinding {
                    keys: "Ctrl-u/d",
                    description: "Page up/down",
                },
                KeyBinding {
                    keys: "Ctrl-g",
                    description: "Go to directory",
                },
                KeyBinding {
                    keys: "o",
                    description: "Toggle expand node",
                },
            ],
        },
        HelpSection {
            title: "Selection & Clipboard",
            bindings: vec![
                KeyBinding {
                    keys: "Space",
                    description: "Mark item for multi-select",
                },
                KeyBinding {
                    keys: "y",
                    description: "Yank (copy) to clipboard",
                },
                KeyBinding {
                    keys: "x",
                    description: "Cut to clipboard",
                },
                KeyBinding {
                    keys: "p",
                    description: "Paste from clipboard",
                },
                KeyBinding {
                    keys: "Esc",
                    description: "Clear clipboard/marks",
                },
            ],
        },
        HelpSection {
            title: "File Operations",
            bindings: vec![
                KeyBinding {
                    keys: "d/Del",
                    description: "Delete item(s)",
                },
                KeyBinding {
                    keys: "r",
                    description: "Rename",
                },
                KeyBinding {
                    keys: "a",
                    description: "Create file (touch)",
                },
                KeyBinding {
                    keys: "A",
                    description: "Create directory (mkdir)",
                },
                KeyBinding {
                    keys: "T",
                    description: "Take (mkdir + cd)",
                },
                KeyBinding {
                    keys: "Ctrl-z",
                    description: "Undo",
                },
            ],
        },
        HelpSection {
            title: "Views & Display",
            bindings: vec![
                KeyBinding {
                    keys: "Tab/S-Tab",
                    description: "Switch view",
                },
                KeyBinding {
                    keys: "v",
                    description: "Toggle Tree/Miller",
                },
                KeyBinding {
                    keys: "s",
                    description: "Cycle sort mode",
                },
                KeyBinding {
                    keys: "S",
                    description: "Reverse sort order",
                },
                KeyBinding {
                    keys: "i",
                    description: "Toggle details panel",
                },
                KeyBinding {
                    keys: "t",
                    description: "Toggle dark/light theme",
                },
                KeyBinding {
                    keys: "R",
                    description: "Full scan/rescan",
                },
            ],
        },
        HelpSection {
            title: "Treemap View",
            bindings: vec![
                KeyBinding {
                    keys: "j/k",
                    description: "Select rectangle",
                },
                KeyBinding {
                    keys: "Enter/l",
                    description: "Drill into directory",
                },
                KeyBinding {
                    keys: "Backspace/h",
                    description: "Navigate back",
                },
                KeyBinding {
                    keys: "Space",
                    description: "Mark selected item",
                },
            ],
        },
        HelpSection {
            title: "Duplicates View",
            bindings: vec![
                KeyBinding {
                    keys: "j/k",
                    description: "Navigate groups/files",
                },
                KeyBinding {
                    keys: "h/l",
                    description: "Collapse/expand group",
                },
                KeyBinding {
                    keys: "d (on group)",
                    description: "Mark all duplicates (keeps 1st)",
                },
                KeyBinding {
                    keys: "d (on file)",
                    description: "Mark single file for deletion",
                },
                KeyBinding {
                    keys: "Space (group)",
                    description: "Select/deselect all in group",
                },
                KeyBinding {
                    keys: "Space (file)",
                    description: "Toggle single file selection",
                },
            ],
        },
        HelpSection {
            title: "Bookmarks",
            bindings: vec![
                KeyBinding {
                    keys: "m<key>",
                    description: "Set bookmark",
                },
                KeyBinding {
                    keys: "'<key>",
                    description: "Jump to bookmark",
                },
                KeyBinding {
                    keys: "B",
                    description: "Show bookmark list",
                },
            ],
        },
        HelpSection {
            title: "Mouse",
            bindings: vec![
                KeyBinding {
                    keys: "Click",
                    description: "Select item",
                },
                KeyBinding {
                    keys: "Double-click",
                    description: "Enter directory",
                },
                KeyBinding {
                    keys: "Right-click",
                    description: "Toggle mark",
                },
                KeyBinding {
                    keys: "Scroll",
                    description: "Move up/down",
                },
            ],
        },
        HelpSection {
            title: "Directory Tabs",
            bindings: vec![
                KeyBinding {
                    keys: "Ctrl-t",
                    description: "New tab (current dir)",
                },
                KeyBinding {
                    keys: "Ctrl-w",
                    description: "Close current tab",
                },
                KeyBinding {
                    keys: "]/[",
                    description: "Next/prev tab",
                },
                KeyBinding {
                    keys: "1-9",
                    description: "Jump to tab N",
                },
            ],
        },
        HelpSection {
            title: "Commands",
            bindings: vec![
                KeyBinding {
                    keys: ":",
                    description: "Open command palette",
                },
                KeyBinding {
                    keys: ",",
                    description: "Open settings",
                },
                KeyBinding {
                    keys: "?",
                    description: "Show this help",
                },
                KeyBinding {
                    keys: "q",
                    description: "Quit",
                },
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
        (":rename-bulk :brn", "Bulk rename marked items"),
        (":clear", "Clear all marks"),
        (":theme dark|light", "Set theme"),
        (":layout tree|miller", "Set layout"),
        (":sort <mode>", "Set sort (size/name/date/count)"),
        (":treemap :map", "Show disk-usage treemap"),
        (":analyze", "Run duplicate/age analysis"),
        (":bookmark [key]", "Set bookmark (or list all)"),
        (":bookmarks", "Show bookmark list"),
        (":delmark <key>", "Delete a bookmark"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn action(code: KeyCode, modifiers: KeyModifiers) -> KeyAction {
        KeyAction::from_key_event(key(code, modifiers))
    }

    fn plain(c: char) -> KeyAction {
        action(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn shifted(c: char) -> KeyAction {
        action(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn ctrl(c: char) -> KeyAction {
        action(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn vim_navigation_keys_map_to_movement() {
        assert_eq!(plain('j'), KeyAction::MoveDown);
        assert_eq!(plain('k'), KeyAction::MoveUp);
        assert_eq!(plain('h'), KeyAction::MoveLeft);
        assert_eq!(plain('l'), KeyAction::MoveRight);
    }

    #[test]
    fn arrow_keys_mirror_the_vim_bindings() {
        assert_eq!(
            action(KeyCode::Down, KeyModifiers::NONE),
            KeyAction::MoveDown
        );
        assert_eq!(action(KeyCode::Up, KeyModifiers::NONE), KeyAction::MoveUp);
        assert_eq!(
            action(KeyCode::Left, KeyModifiers::NONE),
            KeyAction::MoveLeft
        );
        assert_eq!(
            action(KeyCode::Right, KeyModifiers::NONE),
            KeyAction::MoveRight
        );
    }

    #[test]
    fn destructive_actions_require_their_exact_binding() {
        // Delete must not be reachable by accident from a modified 'd'.
        assert_eq!(plain('d'), KeyAction::Delete);
        assert_eq!(shifted('D'), KeyAction::Delete);
        assert_eq!(
            action(KeyCode::Delete, KeyModifiers::NONE),
            KeyAction::Delete
        );

        // Ctrl-d is paging, not deleting.
        assert_eq!(ctrl('d'), KeyAction::PageDown);
        assert_eq!(ctrl('u'), KeyAction::PageUp);
    }

    #[test]
    fn quit_is_reachable_only_from_its_own_bindings() {
        assert_eq!(plain('q'), KeyAction::Quit);
        assert_eq!(ctrl('c'), KeyAction::ForceQuit);

        // Esc clears state rather than quitting, at every modifier level.
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
        ] {
            assert_eq!(action(KeyCode::Esc, modifiers), KeyAction::Cancel);
        }

        // A modified 'q' is not a quit.
        assert_eq!(ctrl('q'), KeyAction::None);
        assert_eq!(
            action(KeyCode::Char('q'), KeyModifiers::ALT),
            KeyAction::None
        );
    }

    #[test]
    fn clipboard_bindings_are_vim_style() {
        assert_eq!(plain('y'), KeyAction::Yank);
        assert_eq!(plain('x'), KeyAction::Cut);
        assert_eq!(plain('p'), KeyAction::Paste);
        assert_eq!(ctrl('z'), KeyAction::Undo);
    }

    #[test]
    fn shifted_letters_use_the_shift_modifier() {
        // Crossterm reports uppercase letters with SHIFT set, so every
        // uppercase binding is declared that way. Without the modifier they
        // fall through to `None` — this pins the contract so a future edit
        // cannot silently drop the modifier from one arm and not another.
        for (c, expected) in [
            ('G', KeyAction::JumpToBottom),
            ('V', KeyAction::EnterVisual),
            ('D', KeyAction::Delete),
            ('A', KeyAction::CreateDirectory),
            ('T', KeyAction::Take),
            ('P', KeyAction::CyclePreviewMode),
            ('R', KeyAction::Refresh),
            ('S', KeyAction::ReverseSort),
            ('B', KeyAction::ShowBookmarkList),
        ] {
            assert_eq!(shifted(c), expected, "Shift-{c}");
            assert_eq!(
                action(KeyCode::Char(c), KeyModifiers::NONE),
                KeyAction::None,
                "unmodified {c} should not be bound"
            );
        }
    }

    #[test]
    fn the_command_palette_accepts_colon_with_or_without_shift() {
        // ':' is a shifted key on most layouts, and terminals disagree about
        // whether they report the modifier.
        assert_eq!(plain(':'), KeyAction::CommandMode);
        assert_eq!(shifted(':'), KeyAction::CommandMode);
    }

    #[test]
    fn digit_keys_select_directory_tabs() {
        for n in 1u8..=9 {
            let c = char::from_digit(u32::from(n), 10).unwrap();
            assert_eq!(plain(c), KeyAction::DirTab(n));
        }
        // Zero is not a tab.
        assert_eq!(plain('0'), KeyAction::None);
    }

    #[test]
    fn unbound_keys_produce_no_action() {
        for code in [
            KeyCode::Char('~'),
            KeyCode::Char('Z'),
            KeyCode::F(5),
            KeyCode::Insert,
        ] {
            assert_eq!(
                action(code, KeyModifiers::NONE),
                KeyAction::None,
                "{code:?}"
            );
        }
    }

    #[test]
    fn mouse_events_map_to_actions() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        let at = |kind| MouseEvent {
            kind,
            column: 4,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };

        assert_eq!(
            MouseAction::from_mouse_event(at(MouseEventKind::Down(MouseButton::Left))),
            MouseAction::Click { x: 4, y: 7 }
        );
        assert_eq!(
            MouseAction::from_mouse_event(at(MouseEventKind::Down(MouseButton::Right))),
            MouseAction::RightClick { x: 4, y: 7 }
        );
        assert_eq!(
            MouseAction::from_mouse_event(at(MouseEventKind::ScrollUp)),
            MouseAction::ScrollUp { x: 4, y: 7 }
        );
        assert_eq!(
            MouseAction::from_mouse_event(at(MouseEventKind::ScrollDown)),
            MouseAction::ScrollDown { x: 4, y: 7 }
        );
        assert_eq!(
            MouseAction::from_mouse_event(at(MouseEventKind::Drag(MouseButton::Left))),
            MouseAction::DragTo { x: 4, y: 7 }
        );
        assert_eq!(
            MouseAction::from_mouse_event(at(MouseEventKind::Up(MouseButton::Left))),
            MouseAction::DragEnd
        );
        assert_eq!(
            MouseAction::from_mouse_event(at(MouseEventKind::Moved)),
            MouseAction::None
        );
    }

    #[test]
    fn help_sections_are_well_formed() {
        let sections = get_help_sections();
        assert!(!sections.is_empty());

        let mut titles: Vec<&str> = sections.iter().map(|section| section.title).collect();
        let count = titles.len();
        titles.sort_unstable();
        titles.dedup();
        assert_eq!(titles.len(), count, "duplicate help section titles");

        for section in &sections {
            assert!(
                !section.bindings.is_empty(),
                "empty help section: {}",
                section.title
            );
            for binding in &section.bindings {
                assert!(
                    !binding.keys.is_empty(),
                    "{} has a blank key",
                    section.title
                );
                assert!(
                    !binding.description.is_empty(),
                    "{} / {} has no description",
                    section.title,
                    binding.keys
                );
            }
        }
    }

    #[test]
    fn command_help_lists_each_command_once() {
        let commands = get_command_help();
        assert!(!commands.is_empty());

        let mut names: Vec<&str> = commands.iter().map(|(name, _)| *name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate entries in the command help");

        for (name, description) in &commands {
            assert!(name.starts_with(':'), "{name} is not a command");
            assert!(!description.is_empty(), "{name} has no description");
        }
    }
}
