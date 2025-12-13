//! Main application state and logic.

mod commands;
mod constants;
mod deletion;
pub mod input;
mod navigation;
mod render;
pub mod state;
mod scanning;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc;

use gravityfile_analyze::{AgeReport, DuplicateReport};
use gravityfile_core::FileTree;
use gravityfile_ops::{
    Conflict, CopyOptions, CopyResult, MoveOptions, MoveResult, OperationProgress, UndoLog,
};
use gravityfile_scan::ScanProgress;

use crate::event::KeyAction;
use crate::theme::Theme;
use crate::ui::{TreeState, TreeView};

use self::commands::{parse_command, CommandAction, CommandInput, CommandKeyResult, LayoutCommand, ThemeCommand};
use self::constants::{PAGE_SIZE, TICK_INTERVAL_MS};
use self::input::{InputResult, InputState};
use self::render::{render_app, RenderContext};
use self::state::{
    AppMode, ClipboardMode, ClipboardState, DeletionProgress, LayoutMode, PendingOperation,
    ScanResult, ScanView, SelectedInfo, View,
};

/// Application result type.
pub type AppResult<T> = color_eyre::Result<T>;

/// Main application state.
pub struct App {
    /// Path being analyzed (scan root).
    path: PathBuf,
    /// Current view root for drill-down navigation (can be different from scan root).
    view_root: PathBuf,
    /// Navigation history stack for going back up (path, selected_index, expanded_set).
    view_history: Vec<(PathBuf, usize, HashSet<PathBuf>)>,
    /// Current mode.
    mode: AppMode,
    /// Current view (normal mode).
    view: View,
    /// Current view during scanning.
    scan_view: ScanView,
    /// Color theme.
    theme: Theme,
    /// Scanned file tree.
    tree: Option<FileTree>,
    /// Tree view state.
    tree_state: TreeState,
    /// Cached count of visible tree items (to avoid flatten on every nav).
    cached_tree_len: usize,
    /// Duplicate analysis report.
    duplicates: Option<DuplicateReport>,
    /// Age analysis report.
    age_report: Option<AgeReport>,
    /// Selected duplicate group index.
    selected_dup_group: usize,
    /// Selected stale directory index.
    selected_stale_dir: usize,
    /// Show details panel.
    show_details: bool,
    /// Error message to display.
    error: Option<String>,
    /// Paths marked/selected for operations.
    marked: HashSet<PathBuf>,
    /// Clipboard state for yank/cut/paste.
    clipboard: ClipboardState,
    /// Undo log for reversible operations.
    #[allow(dead_code)]
    undo_log: UndoLog,
    /// Layout mode (tree vs miller).
    layout_mode: LayoutMode,
    /// Miller columns state.
    miller_state: crate::ui::MillerState,
    /// Cached count of entries in current directory for Miller view.
    cached_miller_len: usize,
    /// Input state for rename/create modes.
    input_state: Option<InputState>,
    /// Current file operation progress (copy/move).
    operation_progress: Option<OperationProgress>,
    /// Pending conflict requiring resolution.
    pending_conflict: Option<Conflict>,
    /// Pending operation that needs conflict resolution before proceeding.
    pending_operation: Option<PendingOperation>,
    /// Last operation result message.
    operation_message: Option<(bool, String)>,
    /// Last deletion result message.
    deletion_message: Option<(bool, String)>,
    /// Current deletion progress (during async deletion).
    deletion_progress: Option<DeletionProgress>,
    /// Current scan progress (for display during scanning).
    scan_progress: Option<ScanProgress>,
    /// Channel for receiving scan results.
    scan_rx: Option<mpsc::Receiver<ScanResult>>,
    /// Scan warnings/errors (populated in real-time during scan).
    warnings: Vec<gravityfile_core::ScanWarning>,
    /// Selected warning index.
    selected_warning: usize,
    /// Whether analysis is running.
    analyzing: bool,
    /// Flag indicating UI needs redraw.
    needs_redraw: bool,
    /// Command palette input state.
    command_input: CommandInput,
}

impl App {
    /// Create a new application.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: path.clone(),
            view_root: path.clone(),
            view_history: Vec::new(),
            mode: AppMode::default(),
            view: View::default(),
            scan_view: ScanView::default(),
            theme: Theme::dark(),
            tree: None,
            tree_state: TreeState::new(path),
            cached_tree_len: 0,
            duplicates: None,
            age_report: None,
            selected_dup_group: 0,
            selected_stale_dir: 0,
            show_details: true,
            error: None,
            marked: HashSet::new(),
            clipboard: ClipboardState::default(),
            undo_log: UndoLog::new(100),
            layout_mode: LayoutMode::default(),
            miller_state: crate::ui::MillerState::new(),
            cached_miller_len: 0,
            input_state: None,
            operation_progress: None,
            pending_conflict: None,
            pending_operation: None,
            operation_message: None,
            deletion_message: None,
            deletion_progress: None,
            scan_progress: None,
            scan_rx: None,
            warnings: Vec::new(),
            selected_warning: 0,
            analyzing: false,
            needs_redraw: true,
            command_input: CommandInput::new(),
        }
    }

    /// Run the application with async event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> AppResult<()> {
        // Start initial scan
        self.start_scan();

        let period = Duration::from_millis(TICK_INTERVAL_MS);
        let mut interval = tokio::time::interval(period);
        let mut events = EventStream::new();

        while self.mode != AppMode::Quit {
            if self.needs_redraw {
                terminal.draw(|frame| self.render(frame))?;
                self.needs_redraw = false;
            }

            tokio::select! {
                biased;

                Some(Ok(event)) = events.next() => {
                    if let Event::Key(key_event) = event {
                        if key_event.kind == crossterm::event::KeyEventKind::Press {
                            if self.mode == AppMode::Command {
                                self.handle_command_input(key_event);
                            } else if matches!(self.mode, AppMode::Renaming | AppMode::CreatingFile | AppMode::CreatingDirectory | AppMode::Taking) {
                                self.handle_input_event(key_event);
                            } else if self.mode == AppMode::ConflictResolution {
                                self.handle_conflict_key(key_event);
                            } else {
                                let action = KeyAction::from_key_event(key_event);
                                self.handle_action(action);
                            }
                        }
                    }

                    // Drain any additional pending events
                    while crossterm::event::poll(Duration::ZERO)? {
                        if let Ok(Event::Key(key_event)) = crossterm::event::read() {
                            if key_event.kind == crossterm::event::KeyEventKind::Press {
                                if self.mode == AppMode::Command {
                                    self.handle_command_input(key_event);
                                } else if matches!(self.mode, AppMode::Renaming | AppMode::CreatingFile | AppMode::CreatingDirectory | AppMode::Taking) {
                                    self.handle_input_event(key_event);
                                } else if self.mode == AppMode::ConflictResolution {
                                    self.handle_conflict_key(key_event);
                                } else {
                                    let action = KeyAction::from_key_event(key_event);
                                    self.handle_action(action);
                                }
                                if self.mode == AppMode::Quit {
                                    break;
                                }
                            }
                        }
                    }
                    self.needs_redraw = true;
                }

                Some(result) = async {
                    if let Some(rx) = &mut self.scan_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    self.handle_scan_result(result);
                    self.needs_redraw = true;
                }

                _ = interval.tick() => {
                    // Periodic tick for background updates
                }
            }
        }

        Ok(())
    }

    /// Start a background scan.
    fn start_scan(&mut self) {
        self.mode = AppMode::Scanning;
        self.scan_view = ScanView::Progress;
        self.scan_progress = Some(ScanProgress::new());
        self.tree = None;
        self.duplicates = None;
        self.age_report = None;
        self.warnings.clear();
        self.selected_warning = 0;
        self.cached_tree_len = 0;

        self.scan_rx = Some(scanning::start_scan(self.path.clone()));
    }

    /// Handle a scan result from the background task.
    fn handle_scan_result(&mut self, result: ScanResult) {
        match result {
            ScanResult::Progress(progress) => {
                self.scan_progress = Some(progress);
            }
            ScanResult::Warning(warning) => {
                self.warnings.push(warning);
            }
            ScanResult::ScanComplete(Ok(tree)) => {
                let root_path = tree.root_path.clone();

                // Merge any final warnings from tree
                for warning in &tree.warnings {
                    if !self.warnings.iter().any(|w| w.path == warning.path) {
                        self.warnings.push(warning.clone());
                    }
                }

                let tree_for_analysis = tree.clone();

                self.tree = Some(tree);

                // Preserve tree state (selection, expanded) across rescans
                // Only reset if this is a different root path
                if self.tree_state.expanded.is_empty()
                    || !self.tree_state.expanded.iter().any(|p| p.starts_with(&root_path))
                {
                    self.tree_state = TreeState::new(root_path);
                } else {
                    // Just ensure root is expanded
                    self.tree_state.expand(&root_path);
                }
                self.update_cached_tree_len();

                // Clamp selection to valid range
                if self.tree_state.selected >= self.cached_tree_len && self.cached_tree_len > 0 {
                    self.tree_state.selected = self.cached_tree_len - 1;
                }
                self.mode = AppMode::Normal;
                self.error = None;
                self.scan_progress = None;

                // Start background analysis
                self.analyzing = true;
                self.scan_rx = Some(scanning::start_analysis(tree_for_analysis));
            }
            ScanResult::ScanComplete(Err(e)) => {
                self.error = Some(e.to_string());
                self.mode = AppMode::Normal;
                self.scan_progress = None;
                self.scan_rx = None;
                self.analyzing = false;
            }
            ScanResult::AnalysisComplete {
                duplicates,
                age_report,
            } => {
                self.duplicates = Some(duplicates);
                self.age_report = Some(age_report);
                self.analyzing = false;
                self.scan_rx = None;
            }
            ScanResult::DeletionProgress(progress) => {
                self.deletion_progress = Some(progress);
            }
            ScanResult::DeletionComplete {
                deleted,
                failed,
                bytes_freed,
            } => {
                self.deletion_progress = None;
                self.scan_rx = None;

                let (success, msg) = deletion::format_deletion_result(deleted, failed, bytes_freed);
                self.deletion_message = Some((success, msg));
                self.mode = AppMode::Normal;

                // Refresh scan after deletion
                self.start_scan();
            }
            ScanResult::OperationProgress(progress) => {
                self.operation_progress = Some(progress);
            }
            ScanResult::OperationConflict(conflict) => {
                self.pending_conflict = Some(conflict);
                self.mode = AppMode::ConflictResolution;
            }
            ScanResult::OperationComplete {
                operation_type,
                succeeded,
                failed,
                bytes_processed: _,
            } => {
                self.operation_progress = None;
                self.scan_rx = None;

                let action = match operation_type {
                    gravityfile_ops::OperationType::Copy => "Copied",
                    gravityfile_ops::OperationType::Move => "Moved",
                    gravityfile_ops::OperationType::Delete => "Deleted",
                    gravityfile_ops::OperationType::Rename => "Renamed",
                    gravityfile_ops::OperationType::CreateFile => "Created file",
                    gravityfile_ops::OperationType::CreateDirectory => "Created directory",
                };

                let success = failed == 0;
                let msg = if success {
                    format!("{} {} items", action, succeeded)
                } else {
                    format!("{} {} items, {} failed", action, succeeded, failed)
                };
                self.operation_message = Some((success, msg));
                self.mode = AppMode::Normal;

                // Refresh scan after operation
                self.start_scan();
            }
        }
    }

    /// Update cached tree item count.
    fn update_cached_tree_len(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
            self.cached_tree_len = items.len();
        } else {
            self.cached_tree_len = 0;
        }
    }

    /// Update cached miller entry count.
    fn update_cached_miller_len(&mut self) {
        if let Some((node, _)) = self.get_view_root_node() {
            self.cached_miller_len = node.children.len();
        } else {
            self.cached_miller_len = 0;
        }
    }

    /// Render the application.
    fn render(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    /// Handle a key action.
    fn handle_action(&mut self, action: KeyAction) {
        // Clear deletion message on any action
        self.deletion_message = None;

        // Handle special modes
        match self.mode {
            AppMode::Help => {
                match action {
                    KeyAction::ToggleHelp | KeyAction::Quit | KeyAction::Cancel => {
                        if self.scan_progress.is_some() && self.tree.is_none() {
                            self.mode = AppMode::Scanning;
                        } else {
                            self.mode = AppMode::Normal;
                        }
                    }
                    _ => {}
                }
                return;
            }
            AppMode::ConfirmDelete => {
                match action {
                    KeyAction::Confirm | KeyAction::DrillDown => {
                        self.execute_deletion();
                    }
                    KeyAction::Quit | KeyAction::Cancel => {
                        // Clear marks if user cancels (they may have been auto-marked)
                        self.marked.clear();
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            AppMode::Deleting => {
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
            AppMode::Copying | AppMode::Moving => {
                // Operations in progress - ignore most input
                return;
            }
            _ => {}
        }

        match action {
            KeyAction::Quit => {
                self.mode = AppMode::Quit;
            }
            KeyAction::ForceQuit => {
                self.mode = AppMode::Quit;
            }
            KeyAction::Cancel => {
                // Esc clears clipboard first, then marks - does NOT quit
                if !self.clipboard.is_empty() {
                    self.clipboard.clear();
                } else if !self.marked.is_empty() {
                    self.marked.clear();
                }
                // If nothing to clear, Esc does nothing in normal mode
            }

            KeyAction::NextTab => {
                if self.mode == AppMode::Scanning {
                    self.scan_view = self.scan_view.next();
                } else {
                    self.view = self.view.next();
                }
            }
            KeyAction::PrevTab => {
                if self.mode == AppMode::Scanning {
                    self.scan_view = self.scan_view.prev();
                } else {
                    self.view = self.view.prev();
                }
            }

            KeyAction::MoveUp => self.move_up(),
            KeyAction::MoveDown => self.move_down(),
            KeyAction::PageUp => self.page_up(),
            KeyAction::PageDown => self.page_down(),
            KeyAction::JumpToTop => self.jump_to_top(),
            KeyAction::JumpToBottom => self.jump_to_bottom(),

            KeyAction::MoveRight | KeyAction::Expand => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.expand_selected(),
                        LayoutMode::Miller => self.drill_into_miller_selected(),
                    }
                }
            }
            KeyAction::MoveLeft | KeyAction::Collapse => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.collapse_selected(),
                        LayoutMode::Miller => self.navigate_back(),
                    }
                }
            }
            KeyAction::ToggleExpand => {
                if self.view == View::Explorer && self.layout_mode == LayoutMode::Tree {
                    self.toggle_selected();
                }
            }

            // Selection
            KeyAction::ToggleMark => {
                self.toggle_mark();
            }
            KeyAction::ClearMarks => {
                self.marked.clear();
            }

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
                // If nothing marked, auto-mark the current item for deletion
                if self.marked.is_empty() {
                    if let Some(path) = self.get_selected_path() {
                        self.marked.insert(path);
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
            KeyAction::Undo => {
                self.execute_undo();
            }

            KeyAction::Confirm => {
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                } else if self.view == View::Explorer {
                    self.toggle_selected();
                }
            }

            KeyAction::DrillDown => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.drill_into_selected(),
                        LayoutMode::Miller => self.drill_into_miller_selected(),
                    }
                }
            }
            KeyAction::NavigateBack => {
                if self.view == View::Explorer {
                    self.navigate_back();
                }
            }

            KeyAction::CommandMode => {
                self.command_input.clear();
                self.mode = AppMode::Command;
            }

            KeyAction::ToggleDetails => {
                self.show_details = !self.show_details;
            }
            KeyAction::ToggleHelp => {
                self.mode = AppMode::Help;
            }
            KeyAction::ToggleTheme => {
                self.theme = self.theme.toggle();
            }
            KeyAction::ToggleLayout => {
                self.layout_mode = self.layout_mode.toggle();
                // Sync and update cached lengths when switching layouts
                if self.layout_mode == LayoutMode::Miller {
                    self.miller_state.reset();
                    self.update_cached_miller_len();
                } else {
                    self.update_cached_tree_len();
                }
            }
            KeyAction::Refresh => {
                self.marked.clear();
                self.start_scan();
            }

            _ => {}
        }
    }

    /// Toggle marking the currently selected item.
    fn toggle_mark(&mut self) {
        let path = match self.view {
            View::Explorer => {
                if let Some((node, root_path)) = self.get_view_root_node() {
                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let items =
                                TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
                            items.get(self.tree_state.selected).map(|i| i.path.clone())
                        }
                        LayoutMode::Miller => {
                            node.children.get(self.miller_state.selected).map(|child| {
                                self.view_root.join(&*child.name)
                            })
                        }
                    }
                } else {
                    None
                }
            }
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let selected = self.selected_dup_group.min(filtered.len().saturating_sub(1));
                    filtered.get(selected).and_then(|g| {
                        g.paths
                            .iter()
                            .find(|p| p.starts_with(&self.view_root))
                            .cloned()
                    })
                } else {
                    None
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let selected = self.selected_stale_dir.min(filtered.len().saturating_sub(1));
                    filtered.get(selected).map(|d| d.path.clone())
                } else {
                    None
                }
            }
            View::Errors => None,
        };

        if let Some(path) = path {
            if path != self.path {
                if self.marked.contains(&path) {
                    self.marked.remove(&path);
                } else {
                    self.marked.insert(path);
                }
            }
        }
    }

    /// Execute deletion of all marked items asynchronously.
    fn execute_deletion(&mut self) {
        let paths_with_sizes: Vec<(PathBuf, u64)> = self
            .marked
            .iter()
            .map(|p| (p.clone(), self.get_path_size(p).unwrap_or(0)))
            .collect();

        let total = paths_with_sizes.len();
        if total == 0 {
            return;
        }

        self.marked.clear();
        self.mode = AppMode::Deleting;
        self.deletion_progress = Some(DeletionProgress {
            total,
            deleted: 0,
            failed: 0,
            bytes_freed: 0,
            current: paths_with_sizes.first().map(|(p, _)| p.clone()),
        });

        self.scan_rx = Some(deletion::start_deletion(paths_with_sizes));
    }

    /// Get the size of a path from the tree.
    fn get_path_size(&self, target: &PathBuf) -> Option<u64> {
        let tree = self.tree.as_ref()?;

        fn find_size(
            node: &gravityfile_core::FileNode,
            target: &PathBuf,
            current: &PathBuf,
        ) -> Option<u64> {
            if current == target {
                return Some(node.size);
            }
            for child in &node.children {
                let child_path = current.join(&*child.name);
                if let Some(size) = find_size(child, target, &child_path) {
                    return Some(size);
                }
            }
            None
        }

        find_size(&tree.root, target, &tree.root_path)
    }

    /// Get the currently selected path in Explorer view.
    fn get_selected_path(&self) -> Option<PathBuf> {
        if self.view != View::Explorer {
            return None;
        }
        if let Some((node, root_path)) = self.get_view_root_node() {
            match self.layout_mode {
                LayoutMode::Tree => {
                    let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
                    items.get(self.tree_state.selected).map(|i| i.path.clone())
                }
                LayoutMode::Miller => {
                    node.children.get(self.miller_state.selected).map(|child| {
                        self.view_root.join(&*child.name)
                    })
                }
            }
        } else {
            None
        }
    }

    /// Yank (copy) selected items to clipboard.
    fn yank_selection(&mut self) {
        let paths: Vec<PathBuf> = if self.marked.is_empty() {
            self.get_selected_path().into_iter().collect()
        } else {
            self.marked.iter().cloned().collect()
        };

        if !paths.is_empty() {
            self.clipboard.yank(paths, self.view_root.clone());
            self.marked.clear();
        }
    }

    /// Cut (move) selected items to clipboard.
    fn cut_selection(&mut self) {
        let paths: Vec<PathBuf> = if self.marked.is_empty() {
            self.get_selected_path().into_iter().collect()
        } else {
            self.marked.iter().cloned().collect()
        };

        if !paths.is_empty() {
            self.clipboard.cut(paths, self.view_root.clone());
            self.marked.clear();
        }
    }

    /// Paste from clipboard to appropriate destination.
    /// - If cursor is on a directory → paste INTO that directory
    /// - If cursor is on a file → paste into the same parent directory
    fn paste_clipboard(&mut self) {
        self.paste_with_resolution(None);
    }

    /// Paste with a specific conflict resolution (or None to check for conflicts).
    fn paste_with_resolution(&mut self, resolution: Option<gravityfile_ops::ConflictResolution>) {
        if self.clipboard.is_empty() {
            return;
        }

        // Determine destination based on current selection
        let destination = self.get_paste_destination();

        let sources = self.clipboard.paths.clone();
        let mode = self.clipboard.mode;

        // Pre-flight conflict detection (only if no resolution provided)
        if resolution.is_none() {
            if let Some(conflict) = self.check_paste_conflict(&sources, &destination) {
                // Store the pending operation and show conflict modal
                self.pending_operation = Some(PendingOperation::Paste {
                    sources: sources.clone(),
                    destination: destination.clone(),
                    mode,
                });
                self.pending_conflict = Some(conflict);
                self.mode = AppMode::ConflictResolution;
                return;
            }
        }

        match mode {
            ClipboardMode::Copy => {
                self.mode = AppMode::Copying;
                let options = CopyOptions {
                    conflict_resolution: resolution,
                    preserve_timestamps: false,
                };
                let rx = gravityfile_ops::start_copy(sources, destination, options);
                self.scan_rx = Some(Self::adapt_copy_rx(rx));
            }
            ClipboardMode::Cut => {
                self.mode = AppMode::Moving;
                let options = MoveOptions {
                    conflict_resolution: resolution,
                };
                let rx = gravityfile_ops::start_move(sources, destination, options);
                self.scan_rx = Some(Self::adapt_move_rx(rx));
                self.clipboard.clear();
            }
            ClipboardMode::Empty => {}
        }
    }

    /// Check if pasting would cause a conflict with an existing file.
    fn check_paste_conflict(&self, sources: &[PathBuf], destination: &PathBuf) -> Option<Conflict> {
        use gravityfile_ops::{ConflictKind};

        for source in sources {
            if let Some(filename) = source.file_name() {
                let dest_path = destination.join(filename);

                // Check if destination already exists
                if dest_path.exists() {
                    // Determine conflict kind
                    let kind = if source == &dest_path {
                        // Same file - source and destination are identical
                        ConflictKind::SameFile
                    } else if dest_path.is_dir() {
                        ConflictKind::DirectoryExists
                    } else {
                        ConflictKind::FileExists
                    };

                    return Some(Conflict::new(
                        source.clone(),
                        dest_path,
                        kind,
                    ));
                }
            }
        }
        None
    }

    /// Get the appropriate paste destination based on current selection.
    /// - If selected item is a directory → return that directory
    /// - If selected item is a file → return the parent directory
    /// - If nothing selected → return view_root
    fn get_paste_destination(&self) -> PathBuf {
        // Try to get the currently selected item
        if let Some(selected_path) = self.get_selected_path() {
            // Check if the selected item is a directory
            if selected_path.is_dir() {
                return selected_path;
            } else {
                // It's a file, use its parent directory
                if let Some(parent) = selected_path.parent() {
                    return parent.to_path_buf();
                }
            }
        }
        // Default to view_root if nothing selected
        self.view_root.clone()
    }

    /// Adapt copy result channel to ScanResult channel.
    fn adapt_copy_rx(mut rx: mpsc::Receiver<CopyResult>) -> mpsc::Receiver<ScanResult> {
        let (tx, result_rx) = mpsc::channel(100);
        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let scan_result = match result {
                    CopyResult::Progress(p) => ScanResult::OperationProgress(p),
                    CopyResult::Conflict(c) => ScanResult::OperationConflict(c),
                    CopyResult::Complete(c) => ScanResult::OperationComplete {
                        operation_type: c.operation_type,
                        succeeded: c.succeeded,
                        failed: c.failed,
                        bytes_processed: c.bytes_processed,
                    },
                };
                if tx.send(scan_result).await.is_err() {
                    break;
                }
            }
        });
        result_rx
    }

    /// Adapt move result channel to ScanResult channel.
    fn adapt_move_rx(mut rx: mpsc::Receiver<MoveResult>) -> mpsc::Receiver<ScanResult> {
        let (tx, result_rx) = mpsc::channel(100);
        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let scan_result = match result {
                    MoveResult::Progress(p) => ScanResult::OperationProgress(p),
                    MoveResult::Conflict(c) => ScanResult::OperationConflict(c),
                    MoveResult::Complete(c) => ScanResult::OperationComplete {
                        operation_type: c.operation_type,
                        succeeded: c.succeeded,
                        failed: c.failed,
                        bytes_processed: c.bytes_processed,
                    },
                };
                if tx.send(scan_result).await.is_err() {
                    break;
                }
            }
        });
        result_rx
    }

    /// Handle key press during conflict resolution mode.
    fn handle_conflict_key(&mut self, event: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        use gravityfile_ops::ConflictResolution;

        // Check if this is a SameFile conflict - only Skip and Abort are valid
        let is_same_file = self.pending_conflict
            .as_ref()
            .map(|c| matches!(c.kind, gravityfile_ops::ConflictKind::SameFile))
            .unwrap_or(false);

        let resolution = match event.code {
            // Skip
            KeyCode::Char('s') => Some(ConflictResolution::Skip),
            // Skip All (not valid for same file)
            KeyCode::Char('S') if !is_same_file => Some(ConflictResolution::SkipAll),
            // Overwrite (not valid for same file)
            KeyCode::Char('o') if !is_same_file => Some(ConflictResolution::Overwrite),
            // Overwrite All (not valid for same file)
            KeyCode::Char('O') if !is_same_file => Some(ConflictResolution::OverwriteAll),
            // Rename (auto-rename)
            KeyCode::Char('r') => Some(ConflictResolution::AutoRename),
            // Abort/Cancel
            KeyCode::Esc => Some(ConflictResolution::Abort),
            // Enter also acts as skip for same-file conflicts
            KeyCode::Enter if is_same_file => Some(ConflictResolution::Skip),
            // For SourceIsAncestor, Enter also acknowledges
            KeyCode::Enter => {
                if let Some(conflict) = &self.pending_conflict {
                    if matches!(conflict.kind, gravityfile_ops::ConflictKind::SourceIsAncestor) {
                        Some(ConflictResolution::Abort)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(res) = resolution {
            // Clear the pending conflict
            self.pending_conflict = None;

            // Take the pending operation
            let pending = self.pending_operation.take();

            // If aborting or skipping, cancel the operation and return to normal mode
            if matches!(res, ConflictResolution::Abort | ConflictResolution::Skip | ConflictResolution::SkipAll) {
                self.mode = AppMode::Normal;
                self.scan_rx = None;
                self.operation_progress = None;
                // Keep clipboard for Abort (user might want to try elsewhere)
                // Clear for Skip only if it was a cut operation that we're fully canceling
                return;
            }

            // Resume the pending operation with the chosen resolution
            if let Some(pending_op) = pending {
                match pending_op {
                    PendingOperation::Paste { sources, destination: _, mode } => {
                        // Temporarily set clipboard to resume the paste
                        let old_clipboard = self.clipboard.clone();
                        self.clipboard.paths = sources;
                        self.clipboard.mode = mode;

                        // Execute paste with the chosen resolution
                        self.paste_with_resolution(Some(res));

                        // For Copy mode, restore clipboard after paste starts
                        // For Cut mode, the paste_with_resolution already clears it
                        if mode == ClipboardMode::Copy {
                            self.clipboard = old_clipboard;
                        }
                    }
                }
            } else {
                // No pending operation, just return to normal
                self.mode = AppMode::Normal;
                self.scan_rx = None;
                self.operation_progress = None;
            }
        }
    }

    /// Start rename operation for current selection.
    fn start_rename(&mut self) {
        if let Some(path) = self.get_selected_path() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut input = InputState::with_initial(&name);
            input.context_path = Some(path);
            self.input_state = Some(input);
            self.mode = AppMode::Renaming;
        }
    }

    /// Start create file operation.
    fn start_create_file(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::CreatingFile;
    }

    /// Start create directory operation.
    fn start_create_directory(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::CreatingDirectory;
    }

    /// Start take operation (create directory and cd into it).
    fn start_take(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::Taking;
    }

    /// Execute undo of the last operation.
    fn execute_undo(&mut self) {
        // TODO: Implement undo
        // For now, just show a message that undo is not yet implemented
        self.operation_message = Some((false, "Undo not yet implemented".to_string()));
    }

    /// Handle key events in input modes (Renaming, CreatingFile, CreatingDirectory).
    fn handle_input_event(&mut self, key: crossterm::event::KeyEvent) {
        let current_mode = self.mode;

        if let Some(ref mut input) = self.input_state {
            match input.handle_key(key) {
                InputResult::Continue => {}
                InputResult::Cancel => {
                    self.input_state = None;
                    self.mode = AppMode::Normal;
                }
                InputResult::Submit(value) => {
                    let context_path = input.context_path.clone();
                    self.input_state = None;

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
                        _ => {}
                    }

                    self.mode = AppMode::Normal;
                }
            }
        } else {
            // No input state, just cancel
            self.mode = AppMode::Normal;
        }
    }

    /// Execute rename operation.
    fn execute_rename(&mut self, source: std::path::PathBuf, new_name: String) {
        use gravityfile_ops::start_rename;

        // Validate name
        if new_name.is_empty() || new_name.contains('/') || new_name.contains('\\') {
            self.operation_message = Some((false, "Invalid file name".to_string()));
            return;
        }

        let rx = start_rename(source.clone(), new_name.clone());

        // Adapt the rename result receiver
        let adapted_rx = Self::adapt_rename_rx(rx);
        self.scan_rx = Some(adapted_rx);
        self.mode = AppMode::Normal;

        // Show pending message
        self.operation_message = Some((true, format!("Renaming to {}...", new_name)));
    }

    /// Adapt rename receiver to ScanResult.
    fn adapt_rename_rx(
        mut rx: mpsc::Receiver<gravityfile_ops::RenameResult>,
    ) -> mpsc::Receiver<ScanResult> {
        let (tx, adapted_rx) = mpsc::channel(100);

        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let scan_result = match result {
                    gravityfile_ops::RenameResult::Progress(progress) => {
                        ScanResult::OperationProgress(progress)
                    }
                    gravityfile_ops::RenameResult::Complete(complete) => {
                        ScanResult::OperationComplete {
                            operation_type: complete.operation_type,
                            succeeded: complete.succeeded,
                            failed: complete.failed,
                            bytes_processed: complete.bytes_processed,
                        }
                    }
                };
                if tx.send(scan_result).await.is_err() {
                    break;
                }
            }
        });

        adapted_rx
    }

    /// Execute create file operation.
    fn execute_create_file(&mut self, parent: std::path::PathBuf, name: String) {
        use gravityfile_ops::start_create_file;

        // Validate name
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            self.operation_message = Some((false, "Invalid file name".to_string()));
            return;
        }

        let path = parent.join(&name);
        let rx = start_create_file(path.clone());

        let adapted_rx = Self::adapt_create_rx(rx);
        self.scan_rx = Some(adapted_rx);

        self.operation_message = Some((true, format!("Creating file {}...", name)));
    }

    /// Execute create directory operation.
    fn execute_create_directory(&mut self, parent: std::path::PathBuf, name: String) {
        use gravityfile_ops::start_create_directory;

        // Validate name
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            self.operation_message = Some((false, "Invalid directory name".to_string()));
            return;
        }

        let path = parent.join(&name);
        let rx = start_create_directory(path.clone());

        let adapted_rx = Self::adapt_create_rx(rx);
        self.scan_rx = Some(adapted_rx);

        self.operation_message = Some((true, format!("Creating directory {}...", name)));
    }

    /// Execute take operation (create directory and navigate into it).
    fn execute_take(&mut self, parent: std::path::PathBuf, name: String) {
        // Validate name
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            self.operation_message = Some((false, "Invalid directory name".to_string()));
            return;
        }

        let path = parent.join(&name);

        // Check if already exists
        if path.exists() {
            if path.is_dir() {
                // Directory exists - just navigate into it
                self.operation_message = Some((true, format!("Directory {} already exists, navigating...", name)));
            } else {
                // File exists with same name
                self.operation_message = Some((false, format!("A file named {} already exists", name)));
                return;
            }
        } else {
            // Create the directory synchronously (it's a simple operation)
            if let Err(e) = std::fs::create_dir(&path) {
                self.operation_message = Some((false, format!("Failed to create directory: {}", e)));
                return;
            }
            self.operation_message = Some((true, format!("Created and entered {}", name)));
        }

        // Navigate into the new directory
        // Save current state to history
        let saved_expanded = self.tree_state.expanded.clone();
        let saved_selected = match self.layout_mode {
            LayoutMode::Tree => self.tree_state.selected,
            LayoutMode::Miller => self.miller_state.selected,
        };
        self.view_history.push((self.view_root.clone(), saved_selected, saved_expanded));

        // Update view root to the new directory
        self.view_root = path;

        // Reset state for new directory
        match self.layout_mode {
            LayoutMode::Tree => {
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
            }
            LayoutMode::Miller => {
                self.miller_state.reset();
            }
        }

        // Refresh to show new directory in tree
        self.start_scan();
    }

    /// Adapt create result receiver to ScanResult.
    fn adapt_create_rx(
        mut rx: mpsc::Receiver<gravityfile_ops::CreateResult>,
    ) -> mpsc::Receiver<ScanResult> {
        let (tx, adapted_rx) = mpsc::channel(100);

        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let scan_result = match result {
                    gravityfile_ops::CreateResult::Progress(progress) => {
                        ScanResult::OperationProgress(progress)
                    }
                    gravityfile_ops::CreateResult::Complete(complete) => {
                        ScanResult::OperationComplete {
                            operation_type: complete.operation_type,
                            succeeded: complete.succeeded,
                            failed: complete.failed,
                            bytes_processed: complete.bytes_processed,
                        }
                    }
                };
                if tx.send(scan_result).await.is_err() {
                    break;
                }
            }
        });

        adapted_rx
    }

    // Navigation methods
    fn move_up(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.tree_state.move_up(1),
                LayoutMode::Miller => self.miller_state.move_up(1),
            },
            View::Duplicates => {
                self.selected_dup_group = self.selected_dup_group.saturating_sub(1);
            }
            View::Age => {
                self.selected_stale_dir = self.selected_stale_dir.saturating_sub(1);
            }
            View::Errors => {
                self.selected_warning = self.selected_warning.saturating_sub(1);
            }
        }
    }

    fn move_down(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.move_down(1, self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.miller_state.move_down(1, self.cached_miller_len);
                }
            },
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_dup_group = (self.selected_dup_group + 1).min(max);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_stale_dir = (self.selected_stale_dir + 1).min(max);
                }
            }
            View::Errors => {
                let max = self.warnings.len().saturating_sub(1);
                self.selected_warning = (self.selected_warning + 1).min(max);
            }
        }
    }

    fn page_up(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.tree_state.move_up(PAGE_SIZE),
                LayoutMode::Miller => self.miller_state.move_up(PAGE_SIZE),
            },
            View::Duplicates => {
                self.selected_dup_group = self.selected_dup_group.saturating_sub(PAGE_SIZE);
            }
            View::Age => {
                self.selected_stale_dir = self.selected_stale_dir.saturating_sub(PAGE_SIZE);
            }
            View::Errors => {
                self.selected_warning = self.selected_warning.saturating_sub(PAGE_SIZE);
            }
        }
    }

    fn page_down(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.move_down(PAGE_SIZE, self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.miller_state.move_down(PAGE_SIZE, self.cached_miller_len);
                }
            },
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_dup_group = (self.selected_dup_group + PAGE_SIZE).min(max);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_stale_dir = (self.selected_stale_dir + PAGE_SIZE).min(max);
                }
            }
            View::Errors => {
                let max = self.warnings.len().saturating_sub(1);
                self.selected_warning = (self.selected_warning + PAGE_SIZE).min(max);
            }
        }
    }

    fn jump_to_top(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.tree_state.jump_to_top(),
                LayoutMode::Miller => self.miller_state.jump_to_top(),
            },
            View::Duplicates => self.selected_dup_group = 0,
            View::Age => self.selected_stale_dir = 0,
            View::Errors => self.selected_warning = 0,
        }
    }

    fn jump_to_bottom(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.jump_to_bottom(self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.miller_state.jump_to_bottom(self.cached_miller_len);
                }
            },
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    self.selected_dup_group = filtered.len().saturating_sub(1);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    self.selected_stale_dir = filtered.len().saturating_sub(1);
                }
            }
            View::Errors => {
                self.selected_warning = self.warnings.len().saturating_sub(1);
            }
        }
    }

    fn expand_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn collapse_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.collapse(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn toggle_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.toggle_expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn get_selected_info(&self) -> Option<SelectedInfo> {
        let tree = self.tree.as_ref()?;
        let (view_node, view_path) = self.get_view_root_node()?;

        match self.layout_mode {
            LayoutMode::Tree => {
                let items = TreeView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
                let item = items.get(self.tree_state.selected)?;
                let node = Self::find_node_at_path(&tree.root, &item.path, &tree.root_path)?;

                Some(SelectedInfo {
                    name: item.node.name.clone(),
                    path: item.path.clone(),
                    size: node.size,
                    file_count: node.file_count(),
                    dir_count: node.dir_count(),
                    modified: node.timestamps.modified,
                    is_dir: node.is_dir(),
                })
            }
            LayoutMode::Miller => {
                let child = view_node.children.get(self.miller_state.selected)?;
                let path = self.view_root.join(&*child.name);

                Some(SelectedInfo {
                    name: child.name.to_string().into(),
                    path,
                    size: child.size,
                    file_count: child.file_count(),
                    dir_count: child.dir_count(),
                    modified: child.timestamps.modified,
                    is_dir: child.is_dir(),
                })
            }
        }
    }

    /// Drill into the currently selected directory.
    fn drill_into_selected(&mut self) {
        let Some((view_node, view_path)) = self.get_view_root_node() else {
            return;
        };

        let items = TreeView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
        let Some(item) = items.get(self.tree_state.selected) else {
            return;
        };

        if !matches!(
            item.node.kind,
            crate::ui::VisibleNodeKind::Directory { .. }
        ) {
            return;
        }

        if item.path == self.view_root {
            self.tree_state.expand(&item.path);
            self.update_cached_tree_len();
            return;
        }

        let saved_expanded = self.tree_state.expanded.clone();
        let saved_selected = self.tree_state.selected;
        self.view_history
            .push((self.view_root.clone(), saved_selected, saved_expanded));

        self.view_root = item.path.clone();

        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
    }

    /// Drill into the selected directory in Miller columns mode.
    fn drill_into_miller_selected(&mut self) {
        let Some((view_node, _)) = self.get_view_root_node() else {
            return;
        };

        let Some(selected_child) = view_node.children.get(self.miller_state.selected) else {
            return;
        };

        // Only drill into directories
        if !selected_child.is_dir() {
            return;
        }

        let target_path = self.view_root.join(&*selected_child.name);

        // Save current state to history
        let saved_expanded = self.tree_state.expanded.clone();
        let saved_selected = self.miller_state.selected;
        self.view_history
            .push((self.view_root.clone(), saved_selected, saved_expanded));

        // Update view root
        self.view_root = target_path;
        self.miller_state.reset();
        self.update_cached_miller_len();
    }

    /// Navigate back up to the previous view root.
    fn navigate_back(&mut self) {
        if let Some((prev_root, saved_selected, saved_expanded)) = self.view_history.pop() {
            self.view_root = prev_root;
            self.tree_state.expanded = saved_expanded;

            // Restore selection based on layout mode
            match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.selected = saved_selected;
                    self.tree_state.offset = 0;
                    self.update_cached_tree_len();
                }
                LayoutMode::Miller => {
                    self.miller_state.selected = saved_selected;
                    self.miller_state.offset = 0;
                    self.update_cached_miller_len();
                }
            }
        } else if self.view_root != self.path {
            if let Some(parent) = self.view_root.parent() {
                if parent.starts_with(&self.path) || parent == self.path {
                    let dir_we_drilled_into = self.view_root.clone();
                    self.tree_state.collapse(&dir_we_drilled_into);

                    self.view_root = parent.to_path_buf();
                    self.tree_state.expand(&self.view_root);

                    match self.layout_mode {
                        LayoutMode::Tree => {
                            self.tree_state.selected = 0;
                            self.tree_state.offset = 0;
                            self.update_cached_tree_len();
                        }
                        LayoutMode::Miller => {
                            // Try to select the directory we just came from
                            if let Some((node, _)) = self.get_view_root_node() {
                                let dir_name = dir_we_drilled_into
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("");
                                let idx = node
                                    .children
                                    .iter()
                                    .position(|c| c.name.as_str() == dir_name)
                                    .unwrap_or(0);
                                self.miller_state.selected = idx;
                            } else {
                                self.miller_state.selected = 0;
                            }
                            self.miller_state.offset = 0;
                            self.update_cached_miller_len();
                        }
                    }
                }
            }
        }
    }

    /// Find a node by path in the tree.
    fn find_node_at_path<'a>(
        node: &'a gravityfile_core::FileNode,
        target_path: &PathBuf,
        current_path: &PathBuf,
    ) -> Option<&'a gravityfile_core::FileNode> {
        if current_path == target_path {
            return Some(node);
        }
        for child in &node.children {
            let child_path = current_path.join(&*child.name);
            if let Some(found) = Self::find_node_at_path(child, target_path, &child_path) {
                return Some(found);
            }
        }
        None
    }

    /// Get the current view root node.
    fn get_view_root_node(&self) -> Option<(&gravityfile_core::FileNode, PathBuf)> {
        let tree = self.tree.as_ref()?;
        let node = Self::find_node_at_path(&tree.root, &self.view_root, &tree.root_path)?;
        Some((node, self.view_root.clone()))
    }

    /// Get the parent node of the current view root (for Miller columns).
    fn get_parent_node(&self) -> Option<&gravityfile_core::FileNode> {
        let tree = self.tree.as_ref()?;
        let parent_path = self.view_root.parent()?.to_path_buf();
        // Only return parent if it's within or at the scan root
        if parent_path.starts_with(&tree.root_path) || parent_path == tree.root_path {
            Self::find_node_at_path(&tree.root, &parent_path, &tree.root_path)
        } else {
            None
        }
    }

    /// Get the current directory name for highlighting in parent column.
    fn get_current_dir_name(&self) -> Option<String> {
        self.view_root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }

    /// Handle raw key input for command mode.
    fn handle_command_input(&mut self, key: crossterm::event::KeyEvent) {
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

    /// Execute a command from the command palette.
    fn execute_command(&mut self, cmd: &str) {
        match parse_command(cmd) {
            CommandAction::None => {}
            CommandAction::Quit => {
                self.mode = AppMode::Quit;
            }
            CommandAction::Refresh => {
                self.marked.clear();
                self.start_scan();
            }
            CommandAction::NavigateTo(target) => {
                self.cd_to_path(&target);
            }
            CommandAction::GoToRoot => {
                self.go_to_root();
            }
            CommandAction::NavigateBack => {
                self.navigate_back();
            }
            CommandAction::ShowHelp => {
                self.mode = AppMode::Help;
            }
            CommandAction::SwitchView(view) => {
                self.view = view;
            }
            CommandAction::ClearMarks => {
                self.marked.clear();
            }
            CommandAction::ToggleDetails => {
                self.show_details = !self.show_details;
            }
            CommandAction::SetTheme(theme_cmd) => match theme_cmd {
                ThemeCommand::Dark => self.theme = Theme::dark(),
                ThemeCommand::Light => self.theme = Theme::light(),
                ThemeCommand::Toggle => self.theme = self.theme.toggle(),
            },
            CommandAction::SetLayout(layout_cmd) => {
                match layout_cmd {
                    LayoutCommand::Tree => {
                        if self.layout_mode != LayoutMode::Tree {
                            self.layout_mode = LayoutMode::Tree;
                            self.update_cached_tree_len();
                        }
                    }
                    LayoutCommand::Miller => {
                        if self.layout_mode != LayoutMode::Miller {
                            self.layout_mode = LayoutMode::Miller;
                            self.miller_state.reset();
                            self.update_cached_miller_len();
                        }
                    }
                    LayoutCommand::Toggle => {
                        self.layout_mode = self.layout_mode.toggle();
                        if self.layout_mode == LayoutMode::Miller {
                            self.miller_state.reset();
                            self.update_cached_miller_len();
                        } else {
                            self.update_cached_tree_len();
                        }
                    }
                }
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
                    self.execute_create_file(self.view_root.clone(), name);
                } else {
                    // Enter create file mode
                    self.start_create_file();
                }
            }
            CommandAction::CreateDirectory(name) => {
                if let Some(name) = name {
                    // Direct create with provided name
                    self.execute_create_directory(self.view_root.clone(), name);
                } else {
                    // Enter create directory mode
                    self.start_create_directory();
                }
            }
            CommandAction::Take(name) => {
                if let Some(name) = name {
                    // Direct take with provided name
                    self.execute_take(self.view_root.clone(), name);
                } else {
                    // Enter take mode
                    self.start_take();
                }
            }
            CommandAction::Undo => {
                self.execute_undo();
            }
        }
    }

    /// Go to the scan root.
    fn go_to_root(&mut self) {
        self.view_root = self.path.clone();
        self.view_history.clear();
        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
    }

    /// Change directory to a path.
    fn cd_to_path(&mut self, target: &str) {
        if target == "/" || target == "~" {
            self.go_to_root();
            return;
        }
        if target == ".." {
            self.navigate_back();
            return;
        }

        let Some(tree) = &self.tree else {
            return;
        };

        let target_path = if target.starts_with('/') {
            let relative = target.trim_start_matches('/');
            self.path.join(relative)
        } else {
            self.view_root.join(target)
        };

        if let Some(node) = Self::find_node_at_path(&tree.root, &target_path, &tree.root_path) {
            if node.is_dir() {
                let saved_expanded = self.tree_state.expanded.clone();
                let saved_selected = self.tree_state.selected;
                self.view_history
                    .push((self.view_root.clone(), saved_selected, saved_expanded));

                self.view_root = target_path;
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
                self.update_cached_tree_len();
            }
        }
    }

    /// Check if a path is marked for deletion.
    pub fn is_marked(&self, path: &PathBuf) -> bool {
        self.marked.contains(path)
    }

    /// Get duplicate groups filtered to current view_root.
    fn get_filtered_duplicates(
        &self,
    ) -> Option<(Vec<&gravityfile_analyze::DuplicateGroup>, u64)> {
        let dups = self.duplicates.as_ref()?;

        if self.view_root == self.path {
            let total: u64 = dups.groups.iter().map(|g| g.wasted_bytes).sum();
            return Some((dups.groups.iter().collect(), total));
        }

        let filtered: Vec<&gravityfile_analyze::DuplicateGroup> = dups
            .groups
            .iter()
            .filter(|g| g.paths.iter().any(|p| p.starts_with(&self.view_root)))
            .collect();

        let total_wasted: u64 = filtered
            .iter()
            .map(|g| {
                let paths_in_view = g
                    .paths
                    .iter()
                    .filter(|p| p.starts_with(&self.view_root))
                    .count();
                if paths_in_view > 1 {
                    g.size * (paths_in_view as u64 - 1)
                } else {
                    0
                }
            })
            .sum();

        Some((filtered, total_wasted))
    }

    /// Get stale directories filtered to current view_root.
    fn get_filtered_stale_dirs(&self) -> Option<Vec<&gravityfile_analyze::StaleDirectory>> {
        let age = self.age_report.as_ref()?;

        if self.view_root == self.path {
            return Some(age.stale_directories.iter().collect());
        }

        let filtered: Vec<&gravityfile_analyze::StaleDirectory> = age
            .stale_directories
            .iter()
            .filter(|d| d.path.starts_with(&self.view_root))
            .collect();

        Some(filtered)
    }
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let ctx = RenderContext {
            mode: self.mode,
            view: self.view,
            scan_view: self.scan_view,
            theme: &self.theme,
            path: &self.path,
            view_root: &self.view_root,
            show_details: self.show_details,
            tree: self.tree.as_ref(),
            tree_state: &self.tree_state,
            layout_mode: self.layout_mode,
            miller_state: &self.miller_state,
            scan_progress: self.scan_progress.as_ref(),
            deletion_progress: self.deletion_progress.as_ref(),
            duplicates: self.duplicates.as_ref(),
            age_report: self.age_report.as_ref(),
            warnings: &self.warnings,
            selected_dup_group: self.selected_dup_group,
            selected_stale_dir: self.selected_stale_dir,
            selected_warning: self.selected_warning,
            marked: &self.marked,
            deletion_message: self.deletion_message.as_ref(),
            operation_message: self.operation_message.as_ref(),
            error: self.error.as_deref(),
            command_input: self.command_input.buffer(),
            command_cursor: self.command_input.cursor(),
            input_state: self.input_state.as_ref(),
            operation_progress: self.operation_progress.as_ref(),
            pending_conflict: self.pending_conflict.as_ref(),
            clipboard: &self.clipboard,
            get_path_size: Box::new(|p| self.get_path_size(p)),
            get_selected_info: self.get_selected_info(),
            get_view_root_node: self.get_view_root_node(),
            get_parent_node: self.get_parent_node(),
            current_dir_name: self.get_current_dir_name(),
            get_filtered_duplicates: self.get_filtered_duplicates(),
            get_filtered_stale_dirs: self.get_filtered_stale_dirs(),
        };

        render_app(&ctx, area, buf);
    }
}
