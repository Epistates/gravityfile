//! Event handling for the TUI.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

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
    ToggleExpand,

    // Directory navigation
    /// Enter a directory, or open a file with the configured opener.
    DrillDown,
    NavigateBack,

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
    /// Scroll the file preview down.
    PreviewDown,
    /// Scroll the file preview up.
    PreviewUp,
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
    /// Second click on the same row in quick succession; see [`ClickTracker`].
    DoubleClick { x: u16, y: u16 },
    /// Right click at a position.
    RightClick { x: u16, y: u16 },
    /// Scroll up at a position.
    ScrollUp { x: u16, y: u16 },
    /// Scroll down at a position.
    ScrollDown { x: u16, y: u16 },
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

/// Recognizes double clicks: terminals report each press separately.
#[derive(Debug, Default)]
pub struct ClickTracker {
    last: Option<(std::time::Instant, u16)>,
}

impl ClickTracker {
    /// Longest gap between the two clicks of a double click.
    pub const WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

    /// `action`, turned into a double click if it is a second left click on
    /// the same row within [`Self::WINDOW`]. A third click starts over.
    pub fn classify(&mut self, action: MouseAction, now: std::time::Instant) -> MouseAction {
        match action {
            MouseAction::Click { x, y } => {
                if let Some((at, row)) = self.last.take()
                    && row == y
                    && now.saturating_duration_since(at) <= Self::WINDOW
                {
                    return MouseAction::DoubleClick { x, y };
                }
                self.last = Some((now, y));
                action
            }
            // Releasing the button belongs to the click.
            MouseAction::DragEnd | MouseAction::None => action,
            _ => {
                self.last = None;
                action
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn a_quick_second_click_on_the_same_row_is_a_double_click() {
        let start = std::time::Instant::now();
        let at = |millis| start + std::time::Duration::from_millis(millis);
        let click = |y| MouseAction::Click { x: 3, y };
        let mut clicks = ClickTracker::default();
        assert_eq!(clicks.classify(click(5), at(0)), click(5));
        assert_eq!(
            clicks.classify(MouseAction::DragEnd, at(50)),
            MouseAction::DragEnd
        );
        assert_eq!(
            clicks.classify(click(5), at(300)),
            MouseAction::DoubleClick { x: 3, y: 5 }
        );
        // A third click starts over rather than double-clicking again.
        assert_eq!(clicks.classify(click(5), at(350)), click(5));
        // Too slow, another row, or something in between: two single clicks.
        assert_eq!(clicks.classify(click(5), at(900)), click(5));
        assert_eq!(clicks.classify(click(6), at(950)), click(6));
        clicks.classify(MouseAction::ScrollDown { x: 3, y: 6 }, at(960));
        assert_eq!(clicks.classify(click(6), at(970)), click(6));
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
