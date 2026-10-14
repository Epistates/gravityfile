//! Command palette handling.

use crossterm::event::{KeyCode, KeyEvent};

use std::path::PathBuf;

use super::App;
use super::state::{AppMode, BookmarkListState, LayoutMode, SortMode, View};
use crate::theme::Theme;

/// Command input state, sharing Unicode editing with filename input.
#[derive(Debug, Clone, Default)]
pub struct CommandInput {
    input: super::input::InputState,
}

impl CommandInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.input = super::input::InputState::new();
    }

    pub fn buffer(&self) -> &str {
        self.input.buffer()
    }

    /// UTF-8 byte offset at a grapheme boundary.
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> CommandKeyResult {
        if key.code == KeyCode::Backspace && self.input.buffer().is_empty() {
            return CommandKeyResult::Cancel;
        }
        match self.input.handle_key(key) {
            super::input::InputResult::Submit(command) => {
                self.clear();
                CommandKeyResult::Execute(command)
            }
            super::input::InputResult::Cancel => {
                self.clear();
                CommandKeyResult::Cancel
            }
            super::input::InputResult::Continue => CommandKeyResult::Continue,
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
    /// No action (empty input).
    None,
    /// Invalid input with an actionable explanation.
    Invalid(&'static str),
    /// Quit the application.
    Quit,
    /// Refresh/rescan.
    Refresh,
    /// Run duplicate and age analysis for the current full scan.
    Analyze,
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

    // Bookmarks
    /// Show bookmark list.
    ShowBookmarks,
    /// Set a bookmark with the given key.
    SetBookmark(Option<char>),
    /// Jump to a bookmark with the given key.
    JumpToBookmark(Option<char>),
    /// Delete a bookmark.
    DeleteBookmark(char),

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
    /// Bulk rename marked items.
    BulkRename,

    // Archive operations
    /// Extract archive to destination.
    Extract(Option<String>),
    /// Compress files to archive.
    Compress(String),
}

impl CommandAction {
    /// Whether the command changes files or rescans, which a snapshot
    /// cannot allow.
    pub fn changes_files(&self) -> bool {
        matches!(
            self,
            Self::Refresh
                | Self::Paste
                | Self::Delete
                | Self::Rename(_)
                | Self::CreateFile(_)
                | Self::CreateDirectory(_)
                | Self::Take(_)
                | Self::Undo
                | Self::BulkRename
                | Self::Extract(_)
                | Self::Compress(_)
        )
    }
}

/// Theme command variants.
#[derive(Debug, Clone, Copy)]
pub enum ThemeCommand {
    /// The palette with this name.
    Named(&'static str),
    /// The next palette.
    Next,
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
    let argument = cmd
        .trim()
        .split_once(char::is_whitespace)
        .map_or("", |(_, argument)| argument.trim_start());
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return CommandAction::None;
    }

    if matches!(
        parts[0],
        "bookmark" | "mark" | "bm" | "delmark" | "delbookmark" | "unbookmark" | "go" | "'"
    ) && !argument.is_empty()
    {
        let mut chars = argument.chars();
        if !chars
            .next()
            .is_some_and(super::state::Bookmarks::is_valid_key)
            || chars.next().is_some()
        {
            return CommandAction::Invalid("Bookmark key must be one ASCII letter or digit");
        }
    }
    if matches!(parts[0], "theme" | "t" | "layout" | "view" | "sort" | "s") && parts.len() > 2 {
        return CommandAction::Invalid("This command accepts one option; use :help for syntax");
    }

    match parts[0] {
        // Quit commands
        "q" | "quit" | "exit" => CommandAction::Quit,

        // Refresh/rescan
        "r" | "refresh" | "rescan" => CommandAction::Refresh,

        // Analyze current scan
        "analyze" | "analyse" | "analysis" => CommandAction::Analyze,

        // Change directory
        "cd" => {
            if parts.len() > 1 {
                CommandAction::NavigateTo(argument.to_owned())
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
        "treemap" | "map" | "heatmap" => CommandAction::SwitchView(View::Treemap),

        // Clear marks
        "clear" | "unmark" => CommandAction::ClearMarks,

        // Toggle details
        "details" | "info" | "i" => CommandAction::ToggleDetails,

        // Theme commands
        "theme" | "t" => match parts.get(1) {
            None | Some(&"next" | &"toggle") => CommandAction::SetTheme(ThemeCommand::Next),
            Some(name) => match crate::theme::Palette::named(name) {
                Some(palette) => CommandAction::SetTheme(ThemeCommand::Named(palette.name)),
                None => CommandAction::Invalid(
                    "Unknown theme; use gestalt, latte, nord, dracula or ansi (or dark, light)",
                ),
            },
        },
        "dark" => CommandAction::SetTheme(ThemeCommand::Named("gestalt")),
        "light" => CommandAction::SetTheme(ThemeCommand::Named("latte")),

        // Layout commands
        "layout" | "view" => {
            if parts.len() > 1 {
                match parts[1] {
                    "tree" => CommandAction::SetLayout(LayoutCommand::Tree),
                    "miller" | "columns" => CommandAction::SetLayout(LayoutCommand::Miller),
                    "toggle" => CommandAction::SetLayout(LayoutCommand::Toggle),
                    _ => CommandAction::Invalid("Unknown layout; use tree, miller, or toggle"),
                }
            } else {
                CommandAction::SetLayout(LayoutCommand::Toggle)
            }
        }
        "miller" | "columns" => CommandAction::SetLayout(LayoutCommand::Miller),

        // Bookmark commands
        "bookmark" | "mark" | "bm" => {
            if parts.len() > 1 {
                // :bookmark <key> - set bookmark with key
                let key = parts[1].chars().next();
                CommandAction::SetBookmark(key)
            } else {
                // :bookmark - show bookmark list
                CommandAction::ShowBookmarks
            }
        }
        "bookmarks" | "marks" | "bms" => CommandAction::ShowBookmarks,
        "delmark" | "delbookmark" | "unbookmark" => {
            if parts.len() > 1 {
                if let Some(key) = parts[1].chars().next() {
                    CommandAction::DeleteBookmark(key)
                } else {
                    CommandAction::Invalid("Specify a bookmark key to remove")
                }
            } else {
                CommandAction::Invalid("Specify a bookmark key to remove")
            }
        }
        // Jump to a bookmark: :go h or :' h
        "go" | "'" => {
            if parts.len() > 1 {
                let key = parts[1].chars().next();
                CommandAction::JumpToBookmark(key)
            } else {
                CommandAction::JumpToBookmark(None)
            }
        }

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
                    "date" | "dt" | "modified" | "mod" => {
                        CommandAction::SetSort(SortCommand::DateDesc)
                    }
                    "date-" | "dt-" | "date-desc" => CommandAction::SetSort(SortCommand::DateDesc),
                    "date+" | "dt+" | "date-asc" => CommandAction::SetSort(SortCommand::DateAsc),
                    "count" | "ct" | "children" => CommandAction::SetSort(SortCommand::CountDesc),
                    "count-" | "ct-" | "count-desc" => {
                        CommandAction::SetSort(SortCommand::CountDesc)
                    }
                    "count+" | "ct+" | "count-asc" => CommandAction::SetSort(SortCommand::CountAsc),
                    "reverse" | "rev" => CommandAction::SetSort(SortCommand::Reverse),
                    _ => CommandAction::Invalid(
                        "Unknown sort order; use size, name, date, count, or reverse",
                    ),
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
                CommandAction::Rename(Some(argument.to_owned()))
            } else {
                CommandAction::Rename(None)
            }
        }

        // Create file
        "touch" | "new" | "create" => {
            if parts.len() > 1 {
                CommandAction::CreateFile(Some(argument.to_owned()))
            } else {
                CommandAction::CreateFile(None)
            }
        }

        // Create directory
        "mkdir" | "md" => {
            if parts.len() > 1 {
                CommandAction::CreateDirectory(Some(argument.to_owned()))
            } else {
                CommandAction::CreateDirectory(None)
            }
        }

        // Take (create directory and cd into it)
        "take" => {
            if parts.len() > 1 {
                CommandAction::Take(Some(argument.to_owned()))
            } else {
                CommandAction::Take(None)
            }
        }

        // Undo
        "undo" | "u" => CommandAction::Undo,

        // Bulk rename
        "rename-bulk" | "bulk-rename" | "bulkrename" | "brn" => CommandAction::BulkRename,

        // Archive operations
        "extract" | "unzip" | "untar" | "decompress" => {
            if parts.len() > 1 {
                // :extract <destination> - extract to specified location
                CommandAction::Extract(Some(argument.to_owned()))
            } else {
                // :extract - extract to current directory
                CommandAction::Extract(None)
            }
        }
        "compress" | "zip" | "archive" => {
            if parts.len() > 1 {
                // :compress <archive_name> - create archive with name
                CommandAction::Compress(argument.to_owned())
            } else {
                CommandAction::Invalid("Specify an archive name, for example :compress backup.zip")
            }
        }

        _ => CommandAction::Invalid("Unknown command; use :help to see available commands"),
    }
}

impl App {
    /// Execute a command from the command palette.
    pub(super) fn execute_command(&mut self, cmd: &str) {
        let action = parse_command(cmd);
        if action.changes_files() && self.refuse_in_snapshot() {
            return;
        }
        match action {
            CommandAction::None => {}
            CommandAction::Invalid(message) => {
                self.error = Some(message.to_string());
            }
            CommandAction::Quit => {
                if self.cancel_archive_operation() {
                    return;
                }
                if self.archive_rx.is_some() || self.take_rx.is_some() {
                    self.error =
                        Some("Filesystem operation in progress; wait for it to finish".to_string());
                } else {
                    self.mode = AppMode::Quit;
                }
            }
            CommandAction::Refresh => self.refresh_view(),
            CommandAction::Analyze => {
                self.start_analysis_for_current_tree();
            }
            CommandAction::NavigateTo(target) => {
                self.cd_to_path(&target);
            }
            CommandAction::GoToRoot => {
                self.go_to_root();
            }
            CommandAction::NavigateBack => {
                self.go_back();
            }
            CommandAction::ShowHelp => {
                self.mode = AppMode::Help;
            }
            CommandAction::SwitchView(view) => {
                self.view = view;
                if self.view == View::Treemap {
                    self.update_cached_treemap_len();
                    self.nav.treemap_state.reset();
                }
                self.maybe_start_analysis_for_view();
            }
            CommandAction::ClearMarks => {
                self.marked.clear();
            }
            CommandAction::ToggleDetails => {
                self.show_details = !self.show_details;
            }
            CommandAction::SetTheme(theme_cmd) => match theme_cmd {
                ThemeCommand::Named(name) => {
                    if let Some(theme) = Theme::named(name) {
                        self.theme = theme;
                    }
                }
                ThemeCommand::Next => self.theme = self.theme.next(),
            },
            CommandAction::SetLayout(layout_cmd) => match layout_cmd {
                LayoutCommand::Tree => {
                    if self.layout_mode != LayoutMode::Tree {
                        self.toggle_layout();
                    }
                }
                LayoutCommand::Miller => {
                    if self.layout_mode != LayoutMode::Miller {
                        self.toggle_layout();
                    }
                }
                LayoutCommand::Toggle => {
                    self.toggle_layout();
                }
            },
            CommandAction::SetSort(sort_cmd) => {
                match sort_cmd {
                    SortCommand::SizeDesc => self.sort_mode = SortMode::SizeDescending,
                    SortCommand::SizeAsc => self.sort_mode = SortMode::SizeAscending,
                    SortCommand::NameAsc => self.sort_mode = SortMode::NameAscending,
                    SortCommand::NameDesc => self.sort_mode = SortMode::NameDescending,
                    SortCommand::DateDesc => self.sort_mode = SortMode::ModifiedDescending,
                    SortCommand::DateAsc => self.sort_mode = SortMode::ModifiedAscending,
                    SortCommand::CountDesc => self.sort_mode = SortMode::CountDescending,
                    SortCommand::CountAsc => self.sort_mode = SortMode::CountAscending,
                    SortCommand::Cycle => self.sort_mode = self.sort_mode.next(),
                    SortCommand::Reverse => self.sort_mode = self.sort_mode.reverse(),
                }
                self.apply_sort();
            }
            CommandAction::Yank => {
                self.yank_selection();
            }
            CommandAction::Cut => {
                self.cut_selection();
            }
            CommandAction::Paste => {
                self.paste_clipboard();
            }
            CommandAction::Delete => {
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                }
            }
            CommandAction::Rename(name) => {
                if let Some(name) = name {
                    // Direct rename with provided name
                    if let Some(path) = self.get_selected_path() {
                        self.execute_rename(path, name);
                    }
                } else {
                    // Enter rename mode
                    self.start_rename();
                }
            }
            CommandAction::CreateFile(name) => {
                if let Some(name) = name {
                    // Direct create with provided name
                    self.execute_create_file(self.nav.view_root.clone(), name);
                } else {
                    // Enter create file mode
                    self.start_create_file();
                }
            }
            CommandAction::CreateDirectory(name) => {
                if let Some(name) = name {
                    // Direct create with provided name
                    self.execute_create_directory(self.nav.view_root.clone(), name);
                } else {
                    // Enter create directory mode
                    self.start_create_directory();
                }
            }
            CommandAction::Take(name) => {
                if let Some(name) = name {
                    // Direct take with provided name
                    self.execute_take(self.nav.view_root.clone(), name);
                } else {
                    // Enter take mode
                    self.start_take();
                }
            }
            CommandAction::Undo => {
                self.execute_undo();
            }
            CommandAction::ShowBookmarks => {
                self.bookmark_list_state =
                    Some(BookmarkListState::new(&self.user_settings.bookmarks));
                self.mode = AppMode::BookmarkList;
            }
            CommandAction::SetBookmark(key) => {
                if let Some(k) = key {
                    self.set_bookmark(k);
                } else {
                    self.mode = AppMode::SettingBookmark;
                }
            }
            CommandAction::JumpToBookmark(key) => {
                if let Some(k) = key {
                    self.jump_to_bookmark(k);
                } else {
                    self.mode = AppMode::JumpingToBookmark;
                }
            }
            CommandAction::DeleteBookmark(key) => {
                self.user_settings.bookmarks.remove(key);
                if let Err(error) = self.user_settings.save() {
                    self.error = Some(format!("Failed to save bookmarks: {error}"));
                }
            }
            CommandAction::BulkRename => {
                self.start_bulk_rename();
            }
            CommandAction::Extract(destination) => {
                self.extract_archive(destination);
            }
            CommandAction::Compress(archive_name) => {
                self.compress_files(&archive_name);
            }
        }
    }

    /// Validate a user-provided path for safety.
    /// Returns error message if path is invalid.
    pub(super) fn validate_user_path(path_str: &str) -> Result<PathBuf, String> {
        // Reject empty paths
        if path_str.is_empty() {
            return Err("Path cannot be empty".to_string());
        }

        // Reject paths with null bytes (security)
        if path_str.contains('\0') {
            return Err("Invalid characters in path".to_string());
        }

        let path = PathBuf::from(path_str);

        // Reject paths with parent directory components (prevents escape attempts)
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("Parent directory references (..) not allowed".to_string());
        }

        // Reject paths starting with ~ (could expand to home directory unexpectedly)
        if path_str.starts_with('~') {
            return Err("Tilde expansion not supported, use absolute path".to_string());
        }

        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_commands_report_errors_instead_of_fallback_actions() {
        use super::*;
        for command in [
            "unknown",
            "theme blue",
            "layout sideways",
            "sort typo",
            "sort name extra",
            "compress",
            "zip",
            "delmark",
            "bookmark ab",
            "go a b",
            "unbookmark é",
            "bm !",
        ] {
            assert!(
                matches!(parse_command(command), CommandAction::Invalid(message) if !message.is_empty()),
                "{command}"
            );
        }
        assert!(matches!(parse_command(" \t "), CommandAction::None));
        assert!(matches!(
            parse_command("sort"),
            CommandAction::SetSort(SortCommand::Cycle)
        ));
        assert!(matches!(
            parse_command("bookmark"),
            CommandAction::ShowBookmarks
        ));
        assert!(matches!(
            parse_command("go"),
            CommandAction::JumpToBookmark(None)
        ));
        assert!(matches!(
            parse_command("delmark a"),
            CommandAction::DeleteBookmark('a')
        ));
    }

    #[test]
    fn path_arguments_preserve_internal_whitespace() {
        use super::*;
        for command in [
            "cd",
            "rename",
            "mv",
            "touch",
            "new",
            "create",
            "mkdir",
            "md",
            "take",
            "extract",
            "unzip",
            "untar",
            "decompress",
            "compress",
            "zip",
            "archive",
        ] {
            for argument in [
                "two  spaces",
                "café\u{2003}日本",
                "path\twith\tspacing",
                "literal 'quotes'",
            ] {
                let result = parse_command(&format!("  {command}\t  {argument}  "));
                let actual = match result {
                    CommandAction::NavigateTo(path) | CommandAction::Compress(path) => path,
                    CommandAction::Rename(Some(path))
                    | CommandAction::CreateFile(Some(path))
                    | CommandAction::CreateDirectory(Some(path))
                    | CommandAction::Take(Some(path))
                    | CommandAction::Extract(Some(path)) => path,
                    _ => panic!("unexpected command result"),
                };
                assert_eq!(actual, argument, "{command}");
            }
        }
    }

    #[test]
    fn unicode_commands_edit_render_and_submit_without_byte_splitting() {
        use super::*;
        use crossterm::event::KeyModifiers;
        use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
        use unicode_width::UnicodeWidthStr;
        let mut input = CommandInput::new();
        let command = "cd café/日本/👩‍💻";
        for character in command.chars() {
            input.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert_eq!(input.buffer(), command);
        input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(&input.buffer()[input.cursor()..], "👩‍💻");
        for cursor in 0..=command.len() + 1 {
            for width in 0..=20 {
                let (before, current, after) =
                    crate::ui::visible_input_parts(command, cursor, width);
                assert!(before.width() + current.width() + after.width() <= width);
                let area = Rect::new(0, 0, width as u16, 1);
                let mut buffer = Buffer::empty(area);
                crate::ui::modals::CommandPalette::new(
                    &crate::theme::Theme::default(),
                    command,
                    cursor,
                )
                .render(area, &mut buffer);
            }
        }
        input.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(input.buffer(), "cd café/日本/");
        assert!(
            matches!(input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            CommandKeyResult::Execute(command) if command == "cd café/日本/")
        );
        assert!(input.buffer().is_empty());
        assert_eq!(input.cursor(), 0);
        assert!(matches!(
            input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            CommandKeyResult::Cancel
        ));
    }
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
        assert!(matches!(
            parse_command("treemap"),
            CommandAction::SwitchView(View::Treemap)
        ));
    }
}
