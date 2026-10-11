//! Keyboard input: mapping keys to actions in each mode.

use std::path::PathBuf;

use crossterm::event::Event;

use super::commands::CommandKeyResult;
use super::input::{InputResult, InputState};
use super::state::{AppMode, BookmarkListState, LayoutMode, SettingsState, UserSettings, View};
use super::{App, bulk_edit, constants, state};
use crate::event::{KeyAction, MouseAction};

impl App {
    /// Dispatch a key event to the appropriate handler for the current mode.
    ///
    /// This is the single authoritative entry point for all key events, used
    /// by both the primary event loop and the drain loop to avoid duplication
    /// and to ensure all modes (including Visual and ConfirmBulkRename) are
    /// handled consistently everywhere.
    pub(super) fn dispatch_key_event(&mut self, key_event: crossterm::event::KeyEvent) {
        if self.mode == AppMode::Command {
            self.handle_command_input(key_event);
        } else if self.mode == AppMode::Search {
            self.handle_search_input(key_event);
        } else if matches!(
            self.mode,
            AppMode::Renaming
                | AppMode::CreatingFile
                | AppMode::CreatingDirectory
                | AppMode::Taking
                | AppMode::GoingTo
        ) {
            self.handle_input_event(key_event);
        } else if self.mode == AppMode::ConflictResolution {
            self.handle_conflict_key(key_event);
        } else if matches!(
            self.mode,
            AppMode::SettingBookmark | AppMode::JumpingToBookmark
        ) {
            self.handle_bookmark_key(key_event);
        } else if self.mode == AppMode::BookmarkList {
            self.handle_bookmark_list_input(key_event);
        } else if self.mode == AppMode::ConfirmBulkRename {
            self.handle_bulk_rename_input(key_event);
        } else if self.mode == AppMode::Visual {
            self.handle_visual_mode_input(key_event);
        } else if self.mode == AppMode::ConfirmDelete {
            self.handle_delete_confirmation(key_event);
        } else {
            let action = self.keymap.action(key_event);
            self.handle_action(action);
        }
    }

    pub(super) fn handle_terminal_event(&mut self, event: Event) {
        match event {
            Event::Key(key_event) if key_event.kind == crossterm::event::KeyEventKind::Press => {
                self.dispatch_key_event(key_event);
            }
            Event::Mouse(mouse_event) if self.mode == AppMode::Normal => {
                let action = self.clicks.classify(
                    MouseAction::from_mouse_event(mouse_event),
                    std::time::Instant::now(),
                );
                self.handle_mouse_action(action);
            }
            _ => {}
        }
    }

    /// Handle a key action.
    pub(super) fn handle_action(&mut self, action: KeyAction) {
        // Clear deletion message on any action
        self.deletion_message = None;

        // Handle special modes
        match self.mode {
            AppMode::BulkRenaming => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.op_token.cancel();
                    self.operation_message =
                        Some((false, "Cancelling bulk rename; waiting for rollback".into()));
                    self.needs_redraw = true;
                }
                // Keep the result channel until rollback and cleanup complete.
                return;
            }
            AppMode::Help => {
                let max = self.help_max_scroll.get();
                let page = self.viewport_height.max(1);
                match action {
                    KeyAction::ToggleHelp | KeyAction::Quit | KeyAction::Cancel => {
                        self.mode = AppMode::Normal;
                    }
                    KeyAction::MoveDown => self.help_scroll = (self.help_scroll + 1).min(max),
                    KeyAction::MoveUp => self.help_scroll = self.help_scroll.saturating_sub(1),
                    KeyAction::PageDown => self.help_scroll = (self.help_scroll + page).min(max),
                    KeyAction::PageUp => self.help_scroll = self.help_scroll.saturating_sub(page),
                    KeyAction::JumpToTop => self.help_scroll = 0,
                    KeyAction::JumpToBottom => self.help_scroll = max,
                    _ => {}
                }
                return;
            }
            AppMode::Settings => {
                self.handle_settings_input(action);
                return;
            }
            // The confirmation reads its keys directly; no action applies
            // behind it.
            AppMode::ConfirmDelete | AppMode::Deleting => {
                return;
            }
            AppMode::Command => {
                match action {
                    KeyAction::Quit | KeyAction::Cancel => {
                        self.command_input.clear();
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            AppMode::ConflictResolution => {
                // Handled in run loop via handle_conflict_key
                return;
            }
            AppMode::Undoing => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.op_token.cancel();
                    self.operation_message =
                        Some((false, "Cancelling undo; waiting for completion".into()));
                    self.needs_redraw = true;
                }
                return;
            }
            AppMode::UpdatingEntry => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.operation_message = Some((
                        false,
                        "Waiting for the filesystem operation to finish".into(),
                    ));
                    self.needs_redraw = true;
                }
                return;
            }
            AppMode::Copying | AppMode::Moving => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.op_token.cancel();
                    self.operation_message =
                        Some((false, "Cancelling operation; waiting for completion".into()));
                    self.needs_redraw = true;
                }
                return;
            }
            _ => {}
        }

        if matches!(action, KeyAction::Cancel | KeyAction::Quit) && self.cancel_archive_operation()
        {
            return;
        }
        if matches!(action, KeyAction::ForceQuit) {
            self.cancel_archive_operation();
        }

        match action {
            KeyAction::Quit => {
                if self.archive_rx.is_some() || self.take_rx.is_some() {
                    self.error =
                        Some("Filesystem operation in progress; wait for it to finish".to_string());
                } else {
                    self.mode = AppMode::Quit;
                }
            }
            KeyAction::ForceQuit => {
                self.mode = AppMode::Quit;
            }
            KeyAction::Cancel => {
                // Acknowledge a visible error without also clearing selection
                // or cancelling work. Subsequent Esc retains normal behavior.
                if self.error.take().is_some() {
                    self.needs_redraw = true;
                } else if !self.clipboard.is_empty() {
                    self.clipboard.clear();
                } else if !self.marked.is_empty() {
                    self.marked.clear();
                } else if self.scan_progress.is_some() || self.analyzing {
                    self.cancel_background_work();
                }
                // If nothing to clear/cancel, Esc does nothing in normal mode
            }

            KeyAction::NextView => {
                self.view = self.view.next();
                if self.view == View::Treemap {
                    self.update_cached_treemap_len();
                    self.nav.treemap_state.reset();
                }
                self.maybe_start_analysis_for_view();
            }
            KeyAction::PrevView => {
                self.view = self.view.prev();
                if self.view == View::Treemap {
                    self.update_cached_treemap_len();
                    self.nav.treemap_state.reset();
                }
                self.maybe_start_analysis_for_view();
            }

            // Directory tabs
            KeyAction::NewDirTab => {
                let navigation = state::Navigation::new(self.nav.view_root.clone());
                if self.tab_manager.open(&mut self.nav, navigation) {
                    self.after_tab_switch();
                }
            }
            KeyAction::CloseDirTab => {
                if self.tab_manager.close_active(&mut self.nav) {
                    self.after_tab_switch();
                }
            }
            KeyAction::NextDirTab => {
                if self.tab_manager.next(&mut self.nav) {
                    self.after_tab_switch();
                }
            }
            KeyAction::PrevDirTab => {
                if self.tab_manager.prev(&mut self.nav) {
                    self.after_tab_switch();
                }
            }
            KeyAction::DirTab(number) => {
                if let Some(index) = (number as usize).checked_sub(1)
                    && self.tab_manager.switch(&mut self.nav, index)
                {
                    self.after_tab_switch();
                }
            }

            KeyAction::MoveUp => self.move_up(),
            KeyAction::MoveDown => self.move_down(),
            KeyAction::PageUp => self.page_up(),
            KeyAction::PageDown => self.page_down(),
            KeyAction::JumpToTop => self.jump_to_top(),
            KeyAction::JumpToBottom => self.jump_to_bottom(),

            KeyAction::MoveRight => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.expand_selected(),
                        LayoutMode::Miller => self.enter_selected(),
                    }
                } else if self.view == View::Duplicates {
                    // l expands group in duplicates view
                    if !self
                        .duplicates_state
                        .is_expanded(self.duplicates_state.selected_group)
                    {
                        self.duplicates_state.toggle_expand();
                    }
                } else if self.view == View::Treemap {
                    self.enter_selected();
                }
            }
            KeyAction::MoveLeft => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.collapse_selected(),
                        LayoutMode::Miller => self.go_back(),
                    }
                } else if self.view == View::Duplicates {
                    // h collapses expanded group in duplicates view
                    if self
                        .duplicates_state
                        .is_expanded(self.duplicates_state.selected_group)
                    {
                        self.duplicates_state.toggle_expand();
                    }
                } else if self.view == View::Treemap {
                    self.go_back();
                }
            }
            KeyAction::ToggleExpand => {
                if self.view == View::Explorer && self.layout_mode == LayoutMode::Tree {
                    self.toggle_selected();
                } else if self.view == View::Duplicates {
                    self.duplicates_state.toggle_expand();
                }
            }

            // Selection
            KeyAction::ToggleMark => {
                self.toggle_mark();
            }
            KeyAction::ClearMarks => {
                self.marked.clear();
            }
            KeyAction::EnterVisual if self.view == View::Explorer => {
                // Enter visual selection mode
                // Only supported in Explorer view for now
                let start_index = match self.layout_mode {
                    LayoutMode::Tree => self.nav.tree_state.selected,
                    LayoutMode::Miller => self.nav.miller_state.selected,
                };
                self.visual_state = Some(state::VisualState::new(start_index));
                self.mode = AppMode::Visual;
            }
            KeyAction::EnterVisual => {}

            // Clipboard operations
            KeyAction::Yank => {
                self.yank_selection();
            }
            KeyAction::Cut => {
                self.cut_selection();
            }
            KeyAction::Paste => {
                self.paste_clipboard();
            }

            // File operations
            KeyAction::Delete => {
                // If nothing marked, auto-mark items for deletion based on current view
                self.delete_auto_marked = self.marked.is_empty();
                if self.marked.is_empty() {
                    if self.view == View::Duplicates {
                        // In duplicates view, special handling
                        // Collect paths first to avoid borrow issues
                        let paths_to_mark: Vec<PathBuf> =
                            if let Some((filtered, _)) = self.get_filtered_duplicates() {
                                let is_header = !self
                                    .duplicates_state
                                    .is_expanded(self.duplicates_state.selected_group)
                                    || self
                                        .duplicates_state
                                        .selected_item(self.duplicates_state.selected_group)
                                        == 0;

                                if is_header {
                                    // Mark all duplicates except the first (the "original")
                                    filtered
                                        .get(self.duplicates_state.selected_group)
                                        .map(|g| g.paths.iter().skip(1).cloned().collect())
                                        .unwrap_or_default()
                                } else {
                                    // Mark the selected file
                                    self.duplicates_state
                                        .selected_file_path(&filtered)
                                        .cloned()
                                        .into_iter()
                                        .collect()
                                }
                            } else {
                                Vec::new()
                            };
                        for path in paths_to_mark {
                            self.mark(path);
                        }
                    } else if self.view == View::Errors {
                        // Only broken symlinks are deletable from the Errors view.
                        if let Some(warning) = self.displayed_warning(self.selected_warning)
                            && warning.kind == gravityfile_core::WarningKind::BrokenSymlink
                        {
                            let path = warning.path.clone();
                            self.mark(path);
                        }
                    } else if let Some(path) = self.get_selected_path()
                        && !self.mark(path)
                    {
                        self.operation_message = Some((
                            false,
                            "The scanned or viewed directory cannot be deleted from here"
                                .to_string(),
                        ));
                    }
                }
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                }
            }
            KeyAction::Rename => {
                self.start_rename();
            }
            KeyAction::CreateFile => {
                self.start_create_file();
            }
            KeyAction::CreateDirectory => {
                self.start_create_directory();
            }
            KeyAction::Take => {
                self.start_take();
            }
            KeyAction::GoTo => {
                self.start_goto();
            }
            KeyAction::Undo => {
                self.execute_undo();
            }

            KeyAction::DrillDown if matches!(self.view, View::Explorer | View::Treemap) => {
                if self.is_selected_file() {
                    self.open_selected_file();
                } else {
                    self.enter_selected();
                }
            }
            KeyAction::DrillDown => {}
            KeyAction::NavigateBack if matches!(self.view, View::Explorer | View::Treemap) => {
                self.go_back();
            }
            KeyAction::NavigateBack => {}

            KeyAction::CommandMode => {
                self.command_input.clear();
                self.mode = AppMode::Command;
            }

            KeyAction::ToggleDetails => {
                self.show_details = !self.show_details;
            }
            KeyAction::ToggleHelp => {
                self.help_scroll = 0;
                self.mode = AppMode::Help;
            }
            KeyAction::ToggleTheme => {
                self.theme = self.theme.next();
            }
            KeyAction::ToggleLayout => {
                self.toggle_layout();
            }
            KeyAction::ToggleSizeMode => self.toggle_size_mode(),
            KeyAction::PreviewDown => self.scroll_preview(constants::PREVIEW_SCROLL_STEP),
            KeyAction::PreviewUp => self.scroll_preview(-constants::PREVIEW_SCROLL_STEP),
            KeyAction::CyclePreviewMode => {
                self.preview_state.cycle_mode();
                if let Some(path) = self.preview_state.path.clone() {
                    self.schedule_preview(path, true);
                } else {
                    self.update_preview();
                }
            }
            KeyAction::OpenSettings => {
                self.settings_state = Some(SettingsState::new(self.user_settings.clone()));
                self.mode = AppMode::Settings;
            }
            KeyAction::Refresh => self.refresh_view(),
            KeyAction::Sort => {
                self.cycle_sort_mode();
            }
            KeyAction::ReverseSort => {
                self.reverse_sort_mode();
            }
            KeyAction::Search => {
                self.start_search();
            }

            // Bookmarks
            KeyAction::SetBookmark => {
                self.mode = AppMode::SettingBookmark;
            }
            KeyAction::JumpToBookmark => {
                self.mode = AppMode::JumpingToBookmark;
            }
            KeyAction::ShowBookmarkList => {
                self.bookmark_list_state =
                    Some(BookmarkListState::new(&self.user_settings.bookmarks));
                self.mode = AppMode::BookmarkList;
            }

            _ => {}
        }
    }

    /// Start go to directory operation.
    pub(super) fn start_goto(&mut self) {
        // Pre-fill with current view root for convenience
        let current_path = self.nav.view_root.to_string_lossy().to_string();
        let mut input = InputState::with_initial(&current_path);
        input.context_path = Some(self.nav.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::GoingTo;
    }

    /// Execute go to directory.
    pub(super) fn execute_goto(&mut self, path_str: &str) {
        let path = if let Some(rest) = path_str.strip_prefix('~') {
            // Expand tilde
            if let Some(home) = gravityfile_core::paths::home() {
                home.join(rest.trim_start_matches('/'))
            } else {
                std::path::PathBuf::from(path_str)
            }
        } else if path_str.starts_with('/') {
            std::path::PathBuf::from(path_str)
        } else {
            // Relative path from current view root
            self.nav.view_root.join(path_str)
        };

        // Canonicalize to resolve .. and .
        let path = path.canonicalize().unwrap_or(path);
        if path.is_dir() {
            self.go_to(path);
        } else {
            self.error = Some(format!("Not a directory: {}", path.display()));
        }
    }

    /// Handle key events in input modes (Renaming, CreatingFile, CreatingDirectory).
    pub(super) fn handle_input_event(&mut self, key: crossterm::event::KeyEvent) {
        let current_mode = self.mode;

        if let Some(ref mut input) = self.input_state {
            match input.handle_key(key) {
                InputResult::Continue => {}
                InputResult::Cancel => {
                    self.input_state = None;
                    self.mode = AppMode::Normal;
                }
                InputResult::Submit(value) => {
                    // An invalid name keeps the prompt open with the reason,
                    // instead of closing it and losing what was typed.
                    if current_mode != AppMode::GoingTo
                        && let Err(error) = input.validate_filename()
                    {
                        input.set_error(error);
                        return;
                    }
                    let context_path = input.context_path.clone();
                    self.input_state = None;
                    self.mode = AppMode::Normal;

                    match current_mode {
                        AppMode::Renaming => {
                            if let Some(source) = context_path {
                                self.execute_rename(source, value);
                            }
                        }
                        AppMode::CreatingFile => {
                            if let Some(parent) = context_path {
                                self.execute_create_file(parent, value);
                            }
                        }
                        AppMode::CreatingDirectory => {
                            if let Some(parent) = context_path {
                                self.execute_create_directory(parent, value);
                            }
                        }
                        AppMode::Taking => {
                            if let Some(parent) = context_path {
                                self.execute_take(parent, value);
                            }
                        }
                        AppMode::GoingTo => {
                            self.execute_goto(&value);
                        }
                        _ => {}
                    }
                }
            }
        } else {
            // No input state, just cancel
            self.mode = AppMode::Normal;
        }
    }

    /// Handle raw key input for command mode.
    pub(super) fn handle_command_input(&mut self, key: crossterm::event::KeyEvent) {
        match self.command_input.handle_key(key) {
            CommandKeyResult::Continue => {}
            CommandKeyResult::Cancel => {
                self.mode = AppMode::Normal;
            }
            CommandKeyResult::Execute(cmd) => {
                self.mode = AppMode::Normal;
                self.execute_command(&cmd);
            }
        }
    }

    /// Handle a key event when in bookmark mode (setting or jumping).
    pub(super) fn handle_bookmark_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
            }
            KeyCode::Char(c) if state::Bookmarks::is_valid_key(c) => {
                match self.mode {
                    AppMode::SettingBookmark => {
                        self.set_bookmark(c);
                    }
                    AppMode::JumpingToBookmark => {
                        self.jump_to_bookmark(c);
                    }
                    _ => {}
                }
                self.mode = AppMode::Normal;
            }
            _ => {
                // Invalid key, return to normal mode
                self.mode = AppMode::Normal;
            }
        }
    }

    /// Set a bookmark for the current directory.
    pub(super) fn set_bookmark(&mut self, key: char) {
        self.user_settings
            .bookmarks
            .set(key, self.nav.view_root.clone());
        if let Err(error) = self.user_settings.save() {
            self.error = Some(format!("Failed to save bookmarks: {error}"));
        }
    }

    /// Resolve and prepare a bookmark destination before changing navigation.
    pub(super) fn jump_to_bookmark(&mut self, key: char) {
        let Some(saved_path) = self.user_settings.bookmarks.get(key).cloned() else {
            self.error = Some(format!("Bookmark '{key}' is not set"));
            return;
        };
        let path = match saved_path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("Cannot open bookmark '{key}': {error}"));
                return;
            }
        };
        if !path.is_dir() {
            self.error = Some(format!("Bookmark '{key}' is not a directory"));
            return;
        }
        self.go_to(path);
    }

    /// Handle input in the bookmark list modal.
    pub(super) fn handle_bookmark_list_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.bookmark_list_state = None;
                self.mode = AppMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(state) = &mut self.bookmark_list_state {
                    state.update_count(&self.user_settings.bookmarks);
                    state.move_down();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(state) = &mut self.bookmark_list_state {
                    state.move_up();
                }
            }
            KeyCode::Enter => {
                // Jump to selected bookmark
                if let Some(state) = &self.bookmark_list_state {
                    let entries = self.user_settings.bookmarks.sorted_entries();
                    if let Some((key, _)) = entries.get(state.selected) {
                        let key = *key;
                        self.bookmark_list_state = None;
                        self.mode = AppMode::Normal;
                        self.jump_to_bookmark(key);
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                // Delete selected bookmark
                if let Some(state) = &self.bookmark_list_state {
                    let entries = self.user_settings.bookmarks.sorted_entries();
                    if let Some((key, _)) = entries.get(state.selected) {
                        let key = *key;
                        self.user_settings.bookmarks.remove(key);
                        if let Err(error) = self.user_settings.save() {
                            self.error = Some(format!("Failed to save bookmarks: {error}"));
                        }
                    }
                }
                // Update state after deletion
                if let Some(state) = &mut self.bookmark_list_state {
                    state.update_count(&self.user_settings.bookmarks);
                }
            }
            // Allow jumping directly by pressing the bookmark key
            KeyCode::Char(c)
                if state::Bookmarks::is_valid_key(c)
                    && self.user_settings.bookmarks.get(c).is_some() =>
            {
                self.bookmark_list_state = None;
                self.mode = AppMode::Normal;
                self.jump_to_bookmark(c);
            }
            _ => {}
        }
    }

    /// Handle input in bulk rename confirmation modal.
    pub(super) fn handle_bulk_rename_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') => {
                // Cancel bulk rename
                self.bulk_rename_state = None;
                self.mode = AppMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(state) = &mut self.bulk_rename_state {
                    state.move_down();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(state) = &mut self.bulk_rename_state {
                    state.move_up();
                }
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                // Check for validation errors before executing
                if let Some(state) = &self.bulk_rename_state
                    && state.error.is_some()
                {
                    // Can't execute with errors
                    return;
                }
                // Execute the bulk rename
                self.execute_bulk_rename();
            }
            KeyCode::Char('e') => {
                // Re-open the pending names in the configured editor through
                // the same private, validated handoff as the first edit.
                let Some(state) = self.bulk_rename_state.take() else {
                    return;
                };
                let (paths, names) = state
                    .entries
                    .into_iter()
                    .map(|entry| (entry.original, entry.new_name))
                    .unzip();
                let prepared = bulk_edit::BulkRenameEdit::with_names(paths, names)
                    .map_err(|error| format!("Cannot prepare bulk rename: {error}"))
                    .and_then(|edit| {
                        crate::opener::editor_command(edit.path(), &self.user_settings.editor)
                            .map(|command| (edit, command))
                            .map_err(|error| {
                                format!("Cannot configure bulk rename editor: {error}")
                            })
                    });
                self.mode = AppMode::Normal;
                match prepared {
                    Ok((edit, command)) => {
                        self.bulk_rename_edit = Some(edit);
                        self.pending_suspend_command = Some(command);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            _ => {}
        }
    }

    /// Handle input in visual selection mode.
    pub(super) fn handle_visual_mode_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        // Get the max count based on current layout
        let max_count = match self.layout_mode {
            LayoutMode::Tree => self.cached_tree_len,
            LayoutMode::Miller => self.cached_miller_len,
        };

        match (key.code, key.modifiers) {
            // Exit visual mode with Esc - cancel selection
            (KeyCode::Esc, _) => {
                self.visual_state = None;
                self.mode = AppMode::Normal;
            }
            // Exit visual mode with Enter or V - apply selection as marks
            (KeyCode::Enter, _) | (KeyCode::Char('V'), KeyModifiers::SHIFT) => {
                self.apply_visual_selection_as_marks();
                self.visual_state = None;
                self.mode = AppMode::Normal;
            }
            // Navigation - vim style
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.move_down(max_count);
                    // Sync the visual cursor with the actual selection state
                    self.sync_visual_cursor_to_view();
                }
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.move_up();
                    self.sync_visual_cursor_to_view();
                }
            }
            // Page navigation
            (KeyCode::Char('u'), KeyModifiers::CONTROL) | (KeyCode::PageUp, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.page_up(constants::PAGE_SIZE);
                    self.sync_visual_cursor_to_view();
                }
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) | (KeyCode::PageDown, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.page_down(constants::PAGE_SIZE, max_count);
                    self.sync_visual_cursor_to_view();
                }
            }
            // Jump to top/bottom
            (KeyCode::Char('g'), KeyModifiers::NONE) | (KeyCode::Home, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.jump_to_top();
                    self.sync_visual_cursor_to_view();
                }
            }
            (KeyCode::Char('G'), KeyModifiers::SHIFT) | (KeyCode::End, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.jump_to_bottom(max_count);
                    self.sync_visual_cursor_to_view();
                }
            }
            // Quick actions while in visual mode
            (KeyCode::Char('d'), KeyModifiers::NONE)
            | (KeyCode::Char('D'), KeyModifiers::SHIFT) => {
                // Delete selected range; the user chose these marks.
                self.apply_visual_selection_as_marks();
                self.delete_auto_marked = false;
                self.visual_state = None;
                self.mode = AppMode::Normal;
                // Now trigger delete confirmation
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                }
            }
            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                // Yank (copy) selected range
                self.apply_visual_selection_as_marks();
                self.visual_state = None;
                self.mode = AppMode::Normal;
                self.yank_selection();
            }
            (KeyCode::Char('x'), KeyModifiers::NONE) => {
                // Cut selected range
                self.apply_visual_selection_as_marks();
                self.visual_state = None;
                self.mode = AppMode::Normal;
                self.cut_selection();
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    /// Handle key input during settings mode.
    pub(super) fn handle_settings_input(&mut self, action: KeyAction) {
        match action {
            KeyAction::Cancel | KeyAction::Quit => {
                // Close settings without saving
                self.settings_state = None;
                self.mode = AppMode::Normal;
            }
            KeyAction::MoveUp => {
                if let Some(ref mut state) = self.settings_state {
                    state.move_up();
                }
            }
            KeyAction::MoveDown => {
                if let Some(ref mut state) = self.settings_state {
                    state.move_down();
                }
            }
            KeyAction::ToggleMark | KeyAction::DrillDown => {
                // Space or Enter toggles the current setting
                if let Some(ref mut state) = self.settings_state {
                    state.toggle_selected();
                }
            }
            KeyAction::Sort => self.save_settings_with(UserSettings::save),
            _ => {}
        }
    }
}
