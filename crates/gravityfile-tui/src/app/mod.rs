//! Main application state and logic.

mod commands;
mod constants;
mod deletion;
pub mod input;
mod navigation;
mod render;
pub mod state;
mod scanning;

use std::collections::{HashMap, HashSet};
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
use crate::preview::PreviewState;
use crate::search::SearchState;
use crate::theme::Theme;
use crate::ui::{TreeState, TreeView};
use crate::TuiConfig;

use self::commands::{parse_command, CommandAction, CommandInput, CommandKeyResult, LayoutCommand, SortCommand, ThemeCommand};
use self::constants::{PAGE_SIZE, TICK_INTERVAL_MS};
use self::input::{InputResult, InputState};
use self::render::{render_app, RenderContext};
use self::state::{
    AppMode, ClipboardMode, ClipboardState, DeletionProgress, DuplicatesViewState, LayoutMode,
    PendingOperation, ScanResult, SelectedInfo, SettingsState, SortMode, TabManager, UserSettings, View,
};

/// Application result type.
pub type AppResult<T> = color_eyre::Result<T>;

/// Data collected for synchronizing selection between layout modes.
struct LayoutSyncData {
    /// Current view root path.
    view_path: PathBuf,
    /// Index in miller view (when switching from tree to miller).
    miller_index: Option<usize>,
    /// Index in tree view (when switching from miller to tree).
    tree_index: Option<usize>,
    /// New view root if we need to change it to show the selected item.
    new_view_root: Option<PathBuf>,
}

/// Main application state.
pub struct App {
    /// Path being analyzed (scan root).
    path: PathBuf,
    /// Current view root for drill-down navigation (can be different from scan root).
    view_root: PathBuf,
    /// Navigation history stack for going back up (path, selected_index, expanded_set).
    view_history: Vec<(PathBuf, usize, HashSet<PathBuf>)>,
    /// Forward navigation history stack (for returning after going back).
    forward_history: Vec<(PathBuf, usize, HashSet<PathBuf>)>,
    /// Current mode.
    mode: AppMode,
    /// Current view (normal mode).
    view: View,
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
    /// Duplicates view state (expanded groups, selected files).
    duplicates_state: DuplicatesViewState,
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
    /// Whether a full recursive scan has been completed (vs just quick_list).
    has_full_scan: bool,
    /// Flag indicating UI needs redraw.
    needs_redraw: bool,
    /// Command palette input state.
    command_input: CommandInput,
    /// Current sort mode for file listings.
    sort_mode: SortMode,
    /// Fuzzy search state.
    search_state: SearchState,
    /// Directory tab manager.
    tab_manager: TabManager,
    /// File preview state.
    preview_state: PreviewState,
    /// Whether to auto-scan on startup.
    scan_on_startup: bool,
    /// User settings (persistent configuration).
    user_settings: UserSettings,
    /// Settings modal state.
    settings_state: Option<SettingsState>,
    /// Cached parent tree for Miller columns when at tree root.
    /// This allows showing the parent column even when navigated beyond the original scan root.
    cached_parent_tree: Option<FileTree>,
    /// Pending command that requires terminal suspension (e.g., opening files in editor).
    pending_suspend_command: Option<std::process::Command>,
    /// Cache of scanned directories by path.
    /// This preserves scan data when navigating to parent directories and allows
    /// restoring it when navigating back.
    scanned_cache: HashMap<PathBuf, FileTree>,
}

impl App {
    /// Create a new application with default config.
    /// Immediately loads a quick directory listing for instant display.
    pub fn new(path: PathBuf) -> Self {
        Self::with_config(path, TuiConfig::default())
    }

    /// Create a new application with custom config.
    /// Immediately loads a quick directory listing for instant display,
    /// and optionally starts a full recursive scan based on config.
    pub fn with_config(path: PathBuf, config: TuiConfig) -> Self {
        // Load user settings from disk
        let user_settings = UserSettings::load();

        // Load quick tree immediately for instant display
        let quick_tree = gravityfile_scan::quick_list(&path).ok();

        // Determine scan_on_startup: CLI flag overrides user settings
        // CLI flag (-S) is explicit; if not provided, use user settings
        let scan_on_startup = if config.scan_on_startup {
            true // CLI flag was explicitly set
        } else {
            user_settings.scan_on_startup
        };

        let mut app = Self {
            path: path.clone(),
            view_root: path.clone(),
            view_history: Vec::new(),
            forward_history: Vec::new(),
            mode: AppMode::default(),
            view: View::default(),
            theme: Theme::dark(),
            tree: quick_tree,
            tree_state: TreeState::new(path.clone()),
            cached_tree_len: 0,
            duplicates: None,
            age_report: None,
            duplicates_state: DuplicatesViewState::new(),
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
            has_full_scan: false,
            needs_redraw: true,
            command_input: CommandInput::new(),
            sort_mode: SortMode::default(),
            search_state: SearchState::new(),
            tab_manager: TabManager::with_initial_tab(path, 10),
            preview_state: PreviewState::new(),
            scan_on_startup,
            user_settings,
            settings_state: None,
            cached_parent_tree: None,
            pending_suspend_command: None,
            scanned_cache: HashMap::new(),
        };

        // Update cached lengths for immediate navigation
        app.update_cached_tree_len();
        app.update_cached_miller_len();
        // Cache parent for Miller columns display
        app.update_cached_parent();

        app
    }

    /// Run the application with async event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> AppResult<()> {
        // Start scan if configured to do so
        if self.scan_on_startup {
            self.start_scan();
        }
        // Otherwise quick list is already loaded in new() for immediate display

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
                            } else if self.mode == AppMode::Search {
                                self.handle_search_input(key_event);
                            } else if matches!(self.mode, AppMode::Renaming | AppMode::CreatingFile | AppMode::CreatingDirectory | AppMode::Taking | AppMode::GoingTo) {
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
                                } else if self.mode == AppMode::Search {
                                    self.handle_search_input(key_event);
                                } else if matches!(self.mode, AppMode::Renaming | AppMode::CreatingFile | AppMode::CreatingDirectory | AppMode::Taking | AppMode::GoingTo) {
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

            // Handle pending suspend command (opening files in external apps)
            if let Some(mut cmd) = self.pending_suspend_command.take() {
                // Restore terminal before running external command
                ratatui::restore();

                // Run the command and wait for it to complete
                let _ = cmd.status();

                // Reinitialize terminal
                terminal = ratatui::init();
                self.needs_redraw = true;
            }
        }

        Ok(())
    }

    /// Start a background scan of the current view_root directory.
    /// Note: Scanning runs in the background while the app remains in Normal mode.
    /// The UI will detect scanning state via `scan_progress.is_some()`.
    /// If we have a quick tree, it remains visible during scanning.
    fn start_scan(&mut self) {
        // Update the scan root to the current view_root
        self.path = self.view_root.clone();

        self.scan_progress = Some(ScanProgress::new());
        // Don't clear tree - keep quick listing visible during scan
        // self.tree = None;
        self.duplicates = None;
        self.age_report = None;
        self.warnings.clear();
        self.selected_warning = 0;
        // Don't reset cached lengths - keep them for navigation
        // self.cached_tree_len = 0;

        self.scan_rx = Some(scanning::start_scan(self.view_root.clone()));
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
            ScanResult::PartialTree(tree) => {
                // Update tree with partial results during scanning
                // This allows the UI to show progressive updates as directories are scanned
                let root_path = tree.root_path.clone();
                self.tree = Some(tree);

                // Preserve selection position if possible
                if self.tree_state.expanded.is_empty() {
                    self.tree_state = TreeState::new(root_path.clone());
                }
                self.tree_state.expand(&root_path);
                self.update_cached_tree_len();
                self.update_cached_miller_len();

                // Clamp selection to valid range
                if self.tree_state.selected >= self.cached_tree_len && self.cached_tree_len > 0 {
                    self.tree_state.selected = self.cached_tree_len - 1;
                }
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

                // Cache the scanned tree for later restoration
                self.scanned_cache.insert(root_path.clone(), tree.clone());

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
                self.update_cached_miller_len();

                // Update cached parent tree for Miller columns display
                self.update_cached_parent();

                // Clamp selection to valid range
                if self.tree_state.selected >= self.cached_tree_len && self.cached_tree_len > 0 {
                    self.tree_state.selected = self.cached_tree_len - 1;
                }
                self.mode = AppMode::Normal;
                self.error = None;
                self.scan_progress = None;
                self.has_full_scan = true;

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

    /// Synchronize selection state when toggling between Tree and Miller layouts.
    /// Maintains the same selected item across layout switches.
    fn sync_layout_selection(&mut self) {
        // Collect all data needed before mutating self
        let sync_data = self.collect_layout_sync_data();

        let Some(data) = sync_data else {
            // No view node, just toggle and reset
            self.layout_mode = self.layout_mode.toggle();
            if self.layout_mode == LayoutMode::Miller {
                self.miller_state.reset();
                self.update_cached_miller_len();
                self.update_preview();
            } else {
                self.update_cached_tree_len();
                self.preview_state.content = crate::preview::PreviewContent::Empty;
            }
            return;
        };

        match self.layout_mode {
            LayoutMode::Tree => {
                // Switching Tree → Miller
                self.layout_mode = LayoutMode::Miller;

                // If we need to change view_root to show the selected item
                if let Some(new_view_root) = data.new_view_root {
                    self.view_root = new_view_root;
                    self.tree_state.expand(&self.view_root);
                }

                if let Some(miller_idx) = data.miller_index {
                    self.miller_state.selected = miller_idx;
                    self.miller_state.offset = 0;
                    self.update_cached_miller_len();
                    self.miller_state.ensure_visible(20);
                    self.update_preview();
                } else {
                    // Fallback: reset miller state
                    self.miller_state.reset();
                    self.update_cached_miller_len();
                    self.update_preview();
                }
            }
            LayoutMode::Miller => {
                // Switching Miller → Tree
                self.layout_mode = LayoutMode::Tree;
                self.preview_state.content = crate::preview::PreviewContent::Empty;

                // Ensure the view_root is expanded in tree
                self.tree_state.expand(&data.view_path);

                if let Some(tree_idx) = data.tree_index {
                    self.tree_state.selected = tree_idx;
                    self.tree_state.offset = 0;
                    self.tree_state.ensure_visible(20);
                }
                self.update_cached_tree_len();
            }
        }
    }

    /// Collect data needed for layout synchronization without holding borrows.
    fn collect_layout_sync_data(&self) -> Option<LayoutSyncData> {
        let (view_node, view_path) = self.get_view_root_node()?;

        match self.layout_mode {
            LayoutMode::Tree => {
                // Get the currently selected path in tree view
                let items = TreeView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard)
                    .flatten(&self.tree_state);

                let selected_path = items.get(self.tree_state.selected).map(|item| item.path.clone());

                let Some(selected) = selected_path else {
                    return Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index: None,
                        tree_index: None,
                        new_view_root: None,
                    });
                };

                // If we're on the root itself, preserve current miller selection
                if selected == view_path {
                    let miller_index = if self.miller_state.selected < view_node.children.len() {
                        Some(self.miller_state.selected)
                    } else if view_node.children.is_empty() {
                        None
                    } else {
                        Some(0)
                    };
                    return Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index,
                        tree_index: None,
                        new_view_root: None,
                    });
                }

                // Check if selected is a direct child of view_root
                let is_direct_child = selected.parent() == Some(view_path.as_path());

                if is_direct_child {
                    // Direct child - find it in miller
                    let name = selected.file_name()?.to_string_lossy();
                    let miller_index = view_node.children.iter().position(|c| c.name.as_str() == name);
                    Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index,
                        tree_index: None,
                        new_view_root: None,
                    })
                } else {
                    // Nested item - need to change view_root to the parent of selected
                    // so that Miller can display the selected item
                    let new_view_root = selected.parent()?.to_path_buf();

                    // Find the selected item's name to locate it in the new view
                    let selected_name = selected.file_name()?.to_string_lossy().to_string();

                    // We need to find the node at new_view_root to get its children
                    let tree = self.tree.as_ref()?;
                    let new_view_node = Self::find_node_at_path(&tree.root, &new_view_root, &tree.root_path)?;
                    let miller_index = new_view_node.children.iter().position(|c| c.name.as_str() == selected_name);

                    Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index,
                        tree_index: None,
                        new_view_root: Some(new_view_root),
                    })
                }
            }
            LayoutMode::Miller => {
                // Get the target path from miller selection
                let target_path = view_node
                    .children
                    .get(self.miller_state.selected)
                    .map(|c| view_path.join(&*c.name));

                // Use existing tree_state expansion to find the index
                // First ensure view_path is expanded
                let mut tree_state_for_search = self.tree_state.clone();
                tree_state_for_search.expand(&view_path);

                let items = TreeView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard)
                    .flatten(&tree_state_for_search);

                let tree_index = target_path.as_ref().and_then(|target| {
                    items.iter().position(|item| &item.path == target)
                });

                Some(LayoutSyncData {
                    view_path: view_path.clone(),
                    miller_index: None,
                    tree_index,
                    new_view_root: None,
                })
            }
        }
    }

    /// Update the file preview for the currently selected item in Miller mode.
    fn update_preview(&mut self) {
        // Only update preview in Miller mode for non-directory items
        if self.layout_mode != LayoutMode::Miller {
            self.preview_state.content = crate::preview::PreviewContent::Empty;
            return;
        }

        let Some((view_node, _)) = self.get_view_root_node() else {
            self.preview_state.content = crate::preview::PreviewContent::Empty;
            return;
        };

        let Some(selected_child) = view_node.children.get(self.miller_state.selected) else {
            self.preview_state.content = crate::preview::PreviewContent::Empty;
            return;
        };

        // Only show file preview for non-directories
        if selected_child.is_dir() {
            // For directories, we show child list directly via miller columns
            self.preview_state.content = crate::preview::PreviewContent::Empty;
            return;
        }

        // Get the full path to the selected file
        let file_path = self.view_root.join(&*selected_child.name);

        // Update preview if path changed
        self.preview_state.update(Some(&file_path));
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
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            AppMode::Settings => {
                self.handle_settings_input(action);
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

            KeyAction::NextView => {
                self.view = self.view.next();
            }
            KeyAction::PrevView => {
                self.view = self.view.prev();
            }

            // Directory tabs
            KeyAction::NewDirTab => {
                // Open new tab with current view_root
                self.tab_manager.new_tab(self.view_root.clone());
            }
            KeyAction::CloseDirTab => {
                self.tab_manager.close_active_tab();
            }
            KeyAction::NextDirTab => {
                // Only switch tabs if there are multiple
                if self.tab_manager.len() > 1 {
                    self.tab_manager.next_tab();
                    self.sync_from_active_tab();
                }
            }
            KeyAction::PrevDirTab => {
                // Only switch tabs if there are multiple
                if self.tab_manager.len() > 1 {
                    self.tab_manager.prev_tab();
                    self.sync_from_active_tab();
                }
            }
            KeyAction::DirTab(n) => {
                // Only switch tabs if there are multiple
                if self.tab_manager.len() > 1 {
                    self.tab_manager.switch_to_number(n as usize);
                    self.sync_from_active_tab();
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
                } else if self.view == View::Duplicates {
                    // l expands group in duplicates view
                    if !self.duplicates_state.is_expanded(self.duplicates_state.selected_group) {
                        self.duplicates_state.toggle_expand();
                    }
                }
            }
            KeyAction::MoveLeft | KeyAction::Collapse => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.collapse_selected(),
                        LayoutMode::Miller => self.navigate_back(),
                    }
                } else if self.view == View::Duplicates {
                    // h collapses expanded group in duplicates view
                    if self.duplicates_state.is_expanded(self.duplicates_state.selected_group) {
                        self.duplicates_state.toggle_expand();
                    }
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
                if self.marked.is_empty() {
                    if self.view == View::Duplicates {
                        // In duplicates view, special handling
                        // Collect paths first to avoid borrow issues
                        let paths_to_mark: Vec<PathBuf> = if let Some((filtered, _)) = self.get_filtered_duplicates() {
                            let is_header = !self.duplicates_state.is_expanded(self.duplicates_state.selected_group)
                                || self.duplicates_state.selected_item(self.duplicates_state.selected_group) == 0;

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
                            self.marked.insert(path);
                        }
                    } else if self.view == View::Errors {
                        // In Errors view, get the selected warning's path
                        if let Some(warning) = self.warnings.get(self.selected_warning) {
                            // Only allow deleting broken symlinks, not permission errors etc.
                            if warning.kind == gravityfile_core::WarningKind::BrokenSymlink {
                                self.marked.insert(warning.path.clone());
                            }
                        }
                    } else if let Some(path) = self.get_selected_path() {
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
            KeyAction::GoTo => {
                self.start_goto();
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

            KeyAction::DrillDown | KeyAction::OpenFile => {
                if self.view == View::Explorer {
                    // Check if selected item is a file or directory
                    let is_file = self.is_selected_file();
                    if is_file {
                        self.open_selected_file();
                    } else {
                        match self.layout_mode {
                            LayoutMode::Tree => self.drill_into_selected(),
                            LayoutMode::Miller => self.drill_into_miller_selected(),
                        }
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
                self.sync_layout_selection();
            }
            KeyAction::CyclePreviewMode => {
                self.preview_state.cycle_mode();
            }
            KeyAction::OpenSettings => {
                self.settings_state = Some(SettingsState::new(self.user_settings.clone()));
                self.mode = AppMode::Settings;
            }
            KeyAction::Refresh => {
                self.marked.clear();
                self.start_scan();
            }
            KeyAction::Sort => {
                self.cycle_sort_mode();
            }
            KeyAction::ReverseSort => {
                self.reverse_sort_mode();
            }
            KeyAction::Search => {
                self.start_search();
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
                // Collect data first to avoid borrow issues
                let (is_on_header, group_paths, selected_file) = if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let is_on_header = !self.duplicates_state.is_expanded(self.duplicates_state.selected_group)
                        || self.duplicates_state.selected_item(self.duplicates_state.selected_group) == 0;

                    let group_paths: Vec<PathBuf> = filtered
                        .get(self.duplicates_state.selected_group)
                        .map(|g| g.paths.clone())
                        .unwrap_or_default();

                    let selected_file = if !is_on_header {
                        self.duplicates_state.selected_file_path(&filtered).cloned()
                    } else {
                        None
                    };

                    (is_on_header, group_paths, selected_file)
                } else {
                    return;
                };

                if is_on_header {
                    // Header selected - toggle ALL files in the group
                    let any_marked = group_paths.iter().any(|p| self.marked.contains(p));

                    if any_marked {
                        // Unmark all files in the group
                        for path in &group_paths {
                            self.marked.remove(path);
                        }
                    } else {
                        // Mark all files in the group
                        for path in group_paths {
                            if path != self.path {
                                self.marked.insert(path);
                            }
                        }
                    }
                    return; // Early return since we handled it here
                } else {
                    // Specific file selected within expanded group
                    selected_file
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
            View::Errors => {
                // Only allow marking broken symlinks for deletion
                if let Some(warning) = self.warnings.get(self.selected_warning) {
                    if warning.kind == gravityfile_core::WarningKind::BrokenSymlink {
                        Some(warning.path.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
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

    /// Start go to directory operation.
    fn start_goto(&mut self) {
        // Pre-fill with current view root for convenience
        let current_path = self.view_root.to_string_lossy().to_string();
        let mut input = InputState::with_initial(&current_path);
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::GoingTo;
    }

    /// Execute go to directory.
    fn execute_goto(&mut self, path_str: &str) {
        let path = if path_str.starts_with('~') {
            // Expand tilde
            if let Some(home) = dirs::home_dir() {
                home.join(&path_str[1..].trim_start_matches('/'))
            } else {
                std::path::PathBuf::from(path_str)
            }
        } else if path_str.starts_with('/') {
            std::path::PathBuf::from(path_str)
        } else {
            // Relative path from current view root
            self.view_root.join(path_str)
        };

        // Canonicalize to resolve .. and .
        let path = path.canonicalize().unwrap_or(path);

        if path.is_dir() {
            // Check if target path is within our already-scanned tree
            let within_scanned_tree = if let Some(tree) = &self.tree {
                path.starts_with(&tree.root_path) || tree.root_path.starts_with(&path)
            } else {
                false
            };

            if within_scanned_tree {
                // Navigate within the existing tree - no rescan needed
                self.view_root = path.clone();
                self.view_history.clear();
                self.forward_history.clear();
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
                self.miller_state.reset();
                self.update_cached_tree_len();
                self.update_cached_miller_len();
                self.preview_state.content = crate::preview::PreviewContent::Empty;

                // Update the active tab view_root but keep the tree
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.view_root = self.view_root.clone();
                }
            } else {
                // Going to a new location - use quick_list for immediate display
                // Don't auto-scan; user can press R to scan if needed
                self.path = path.clone();
                self.view_root = path.clone();
                self.view_history.clear();
                self.forward_history.clear();
                self.tree_state = TreeState::new(self.path.clone());
                self.miller_state.reset();
                self.marked.clear();
                self.clipboard.clear();
                self.duplicates = None;
                self.age_report = None;
                self.warnings.clear();
                self.preview_state.content = crate::preview::PreviewContent::Empty;
                self.scan_progress = None;

                // Load quick tree for immediate display
                self.tree = gravityfile_scan::quick_list(&path).ok();
                self.update_cached_tree_len();
                self.update_cached_miller_len();

                // Update the active tab
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.path = self.path.clone();
                    tab.view_root = self.view_root.clone();
                    tab.label = self.path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| self.path.to_string_lossy().to_string());
                    tab.history.clear();
                }

                // Don't auto-start scan - user can press R when ready
            }
        } else {
            self.error = Some(format!("Not a directory: {}", path.display()));
        }
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
                        AppMode::GoingTo => {
                            self.execute_goto(&value);
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

        self.sync_to_active_tab();

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
                LayoutMode::Miller => {
                    self.miller_state.move_up(1);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Get counts first to avoid borrow issues
                let (group_count, file_counts) = if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let counts: Vec<usize> = filtered.iter().map(|g| g.paths.len()).collect();
                    (filtered.len(), counts)
                } else {
                    (0, Vec::new())
                };
                if group_count > 0 {
                    self.duplicates_state.move_up(group_count, |idx| {
                        file_counts.get(idx).copied().unwrap_or(0)
                    });
                }
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
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Get counts first to avoid borrow issues
                let (group_count, file_counts) = if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let counts: Vec<usize> = filtered.iter().map(|g| g.paths.len()).collect();
                    (filtered.len(), counts)
                } else {
                    (0, Vec::new())
                };
                if group_count > 0 {
                    self.duplicates_state.move_down(group_count, |idx| {
                        file_counts.get(idx).copied().unwrap_or(0)
                    });
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
                LayoutMode::Miller => {
                    self.miller_state.move_up(PAGE_SIZE);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Page up - move multiple groups up
                self.duplicates_state.selected_group =
                    self.duplicates_state.selected_group.saturating_sub(PAGE_SIZE);
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
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Page down - move multiple groups down
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let max = filtered.len().saturating_sub(1);
                    self.duplicates_state.selected_group =
                        (self.duplicates_state.selected_group + PAGE_SIZE).min(max);
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
                LayoutMode::Miller => {
                    self.miller_state.jump_to_top();
                    self.update_preview();
                }
            },
            View::Duplicates => self.duplicates_state.reset(),
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
                    self.update_preview();
                }
            },
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    self.duplicates_state.selected_group = filtered.len().saturating_sub(1);
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
                // Lazy load directory contents if directory has no children
                if matches!(item.node.kind, crate::ui::VisibleNodeKind::Directory { .. }) {
                    let needs_lazy_load = if let Some(tree) = &self.tree {
                        if let Some(node) = Self::find_node_at_path(&tree.root, &item.path, &tree.root_path) {
                            node.children.is_empty()
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if needs_lazy_load {
                        self.lazy_load_directory(&item.path);
                    }
                }
                self.tree_state.expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    /// Collapse the selected item, or navigate back if already collapsed/not a directory.
    /// This provides vim-style navigation where 'h' goes up when there's nothing to collapse.
    fn collapse_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                // Check if this is an expanded directory
                let is_expanded_dir = matches!(
                    item.node.kind,
                    crate::ui::VisibleNodeKind::Directory { expanded: true }
                );

                if is_expanded_dir {
                    // Collapse the directory
                    self.tree_state.collapse(&item.path);
                    self.update_cached_tree_len();
                } else {
                    // Not expanded or not a directory - navigate back to parent
                    self.navigate_back();
                }
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

    /// Check if the currently selected item is a file (not a directory).
    fn is_selected_file(&self) -> bool {
        match self.layout_mode {
            LayoutMode::Tree => {
                if let Some((view_node, view_path)) = self.get_view_root_node() {
                    let items = TreeView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard)
                        .flatten(&self.tree_state);
                    if let Some(item) = items.get(self.tree_state.selected) {
                        return !matches!(item.node.kind, crate::ui::VisibleNodeKind::Directory { .. });
                    }
                }
                false
            }
            LayoutMode::Miller => {
                if let Some((view_node, _)) = self.get_view_root_node() {
                    if let Some(child) = view_node.children.get(self.miller_state.selected) {
                        return !child.is_dir();
                    }
                }
                false
            }
        }
    }

    /// Get the path of the currently selected item.
    fn get_selected_path(&self) -> Option<PathBuf> {
        match self.layout_mode {
            LayoutMode::Tree => {
                if let Some((view_node, view_path)) = self.get_view_root_node() {
                    let items = TreeView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard)
                        .flatten(&self.tree_state);
                    return items.get(self.tree_state.selected).map(|item| item.path.clone());
                }
                None
            }
            LayoutMode::Miller => {
                if let Some((view_node, _)) = self.get_view_root_node() {
                    if let Some(child) = view_node.children.get(self.miller_state.selected) {
                        return Some(self.view_root.join(&*child.name));
                    }
                }
                None
            }
        }
    }

    /// Open the currently selected file with the configured opener.
    fn open_selected_file(&mut self) {
        let Some(path) = self.get_selected_path() else {
            return;
        };

        // Get opener configuration
        let openers = &self.user_settings.openers;
        let editor_config = &self.user_settings.editor;

        match crate::opener::open_file(&path, openers, editor_config) {
            crate::opener::OpenResult::Opened => {
                // File opened with system command (non-blocking)
            }
            crate::opener::OpenResult::NeedsSuspend(cmd) => {
                // Need to suspend terminal and run command
                self.suspend_and_run(cmd);
            }
            crate::opener::OpenResult::NotSupported(msg) => {
                self.error = Some(msg);
            }
            crate::opener::OpenResult::Error(msg) => {
                self.error = Some(msg);
            }
        }
    }

    /// Suspend the terminal, run a command, and restore.
    fn suspend_and_run(&mut self, cmd: std::process::Command) {
        // We need to restore terminal before running the command
        // and reinitialize after. This is handled by setting a flag
        // that the main loop will process.
        self.pending_suspend_command = Some(cmd);
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

        // Check if directory needs lazy loading (has no children loaded yet)
        let needs_lazy_load = if let Some(tree) = &self.tree {
            if let Some(node) = Self::find_node_at_path(&tree.root, &item.path, &tree.root_path) {
                node.children.is_empty()
            } else {
                true
            }
        } else {
            true
        };

        // Lazy load directory contents if needed
        if needs_lazy_load {
            self.lazy_load_directory(&item.path);
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

        let target_path = item.path.clone();

        // Check if forward history has this path - if so, restore that state
        if let Some(forward_idx) = self.forward_history.iter().rposition(|(p, _, _)| p == &target_path) {
            let (_, fwd_selected, fwd_expanded) = self.forward_history.remove(forward_idx);
            // Clear any forward history entries after this one
            self.forward_history.truncate(forward_idx);

            self.view_root = target_path;
            self.tree_state.selected = fwd_selected;
            self.tree_state.offset = 0;
            self.tree_state.expanded = fwd_expanded;
            self.tree_state.expand(&self.view_root);
            self.update_cached_tree_len();
            self.sync_to_active_tab();
            return;
        }

        // Clear forward history - user is navigating to a new location
        self.forward_history.clear();

        self.view_root = target_path;

        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
        self.sync_to_active_tab();
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

        // Lazy loading: if directory has no children (from quick_list), load them now
        if selected_child.children.is_empty() {
            self.lazy_load_directory(&target_path);
        }

        // Save current state to history
        let saved_expanded = self.tree_state.expanded.clone();
        let saved_selected = self.miller_state.selected;
        self.view_history
            .push((self.view_root.clone(), saved_selected, saved_expanded));

        // Check if forward history has this path - if so, restore that state
        if let Some(forward_idx) = self.forward_history.iter().rposition(|(p, _, _)| p == &target_path) {
            let (_, fwd_selected, fwd_expanded) = self.forward_history.remove(forward_idx);
            // Clear any forward history entries after this one
            self.forward_history.truncate(forward_idx);

            self.view_root = target_path;
            self.miller_state.selected = fwd_selected;
            self.miller_state.offset = 0;
            self.tree_state.expanded = fwd_expanded;
            self.update_cached_miller_len();
            self.miller_state.ensure_visible(20);
            self.update_preview();
            self.sync_to_active_tab();
            return;
        }

        // Clear forward history - user is navigating to a new location
        self.forward_history.clear();

        // Update view root
        self.view_root = target_path;
        self.miller_state.reset();
        self.update_cached_miller_len();
        self.update_preview();
        self.sync_to_active_tab();
    }

    /// Lazy load a directory's children using quick_list.
    /// This updates the tree in-place with the new children.
    fn lazy_load_directory(&mut self, path: &PathBuf) {
        // First check if we have cached scan data for this directory
        if let Some(cached_tree) = self.scanned_cache.get(path).cloned() {
            // Use cached scan data - it has full size/structure information
            if let Some(tree) = &mut self.tree {
                if let Some(node) = Self::find_node_mut(&mut tree.root, path, &tree.root_path) {
                    // Replace with the cached node (preserves all scan data)
                    *node = cached_tree.root;
                }
            }
            return;
        }

        // Quick load directory contents for immediate display
        // When the full scan completes, it will replace this with accurate data
        if let Ok(quick_tree) = gravityfile_scan::quick_list(path) {
            // Find the node in our tree and update its children
            if let Some(tree) = &mut self.tree {
                if let Some(node) = Self::find_node_mut(&mut tree.root, path, &tree.root_path) {
                    node.children = quick_tree.root.children;
                }
            }
        }
    }

    /// Find a mutable reference to a node by path.
    fn find_node_mut<'a>(
        node: &'a mut gravityfile_core::FileNode,
        target: &PathBuf,
        current_path: &PathBuf,
    ) -> Option<&'a mut gravityfile_core::FileNode> {
        if current_path == target {
            return Some(node);
        }

        for child in &mut node.children {
            let child_path = current_path.join(&*child.name);
            if target.starts_with(&child_path) {
                if let Some(found) = Self::find_node_mut(child, target, &child_path) {
                    return Some(found);
                }
            }
        }

        None
    }

    /// Navigate back up to the previous view root.
    /// Supports navigation beyond the scan root, all the way to filesystem root.
    fn navigate_back(&mut self) {
        // Prepare current state for forward history (will be pushed if navigation succeeds)
        let current_selected = match self.layout_mode {
            LayoutMode::Tree => self.tree_state.selected,
            LayoutMode::Miller => self.miller_state.selected,
        };
        let current_state = (
            self.view_root.clone(),
            current_selected,
            self.tree_state.expanded.clone(),
        );

        // First, try to use view history if available
        if let Some((prev_root, saved_selected, saved_expanded)) = self.view_history.pop() {
            // Navigation will succeed - save current state to forward history
            self.forward_history.push(current_state);
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
            self.sync_to_active_tab();
            return;
        }

        // No history - try to navigate to parent
        let Some(parent) = self.view_root.parent() else {
            return; // Already at root
        };

        let parent = parent.to_path_buf();
        let dir_we_came_from = self.view_root.clone();

        // Navigation will succeed - save current state to forward history
        self.forward_history.push(current_state);

        // Check if parent is within our current tree
        let parent_in_tree = parent.starts_with(&self.path) || parent == self.path;

        if parent_in_tree {
            // Navigate within existing tree
            self.tree_state.collapse(&dir_we_came_from);
            self.view_root = parent;
            self.tree_state.expand(&self.view_root);
        } else {
            // Navigate beyond scan root - need to extend our tree
            self.navigate_to_parent_beyond_scan_root(&parent, &dir_we_came_from);
            return;
        }

        // Select the directory we just came from
        self.select_child_by_name(&dir_we_came_from);
        self.sync_to_active_tab();
    }

    /// Navigate to a parent directory that's outside the current scan root.
    /// This extends the tree by lazy-loading the parent.
    fn navigate_to_parent_beyond_scan_root(&mut self, parent: &PathBuf, dir_we_came_from: &PathBuf) {
        // Cache current tree before navigating away (if we have scan data)
        if let Some(tree) = &self.tree {
            if self.has_full_scan {
                self.scanned_cache.insert(tree.root_path.clone(), tree.clone());
            }
        }

        // Load the parent directory contents
        let Ok(mut parent_tree) = gravityfile_scan::quick_list(parent) else {
            return; // Can't access parent directory
        };

        // Merge any cached scans into the parent tree's children
        self.merge_cached_scans_into_tree(&mut parent_tree.root, parent);

        // Create a new tree rooted at the parent
        self.path = parent.clone();
        self.tree = Some(parent_tree);
        self.view_root = parent.clone();
        self.view_history.clear();
        // Note: don't clear forward_history here - we want to allow navigating back

        // The parent tree is quick-listed, not fully scanned
        self.has_full_scan = false;

        // Reset tree state for the new root
        self.tree_state = TreeState::new(parent.clone());

        // Update cached parent for Miller columns display
        self.update_cached_parent();

        // Select the directory we came from
        self.select_child_by_name(dir_we_came_from);
        self.sync_to_active_tab();
    }

    /// Merge cached scan data into a tree node's children.
    /// For each child directory in the node, if we have cached scan data,
    /// replace the placeholder child with the fully scanned version.
    fn merge_cached_scans_into_tree(&self, node: &mut gravityfile_core::FileNode, node_path: &PathBuf) {
        for child in &mut node.children {
            if child.is_dir() {
                let child_path = node_path.join(&*child.name);
                if let Some(cached_tree) = self.scanned_cache.get(&child_path) {
                    // Replace this child with the cached scan data
                    *child = cached_tree.root.clone();
                }
            }
        }
    }

    /// Select a child entry by matching its name.
    fn select_child_by_name(&mut self, target_path: &PathBuf) {
        let dir_name = target_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        match self.layout_mode {
            LayoutMode::Tree => {
                // In tree view, find the index of the target in flattened list
                if let Some((node, root_path)) = self.get_view_root_node() {
                    let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                        .flatten(&self.tree_state);
                    if let Some(idx) = items.iter().position(|item| {
                        item.path.file_name().and_then(|n| n.to_str()) == Some(dir_name)
                    }) {
                        self.tree_state.selected = idx;
                    } else {
                        self.tree_state.selected = 0;
                    }
                } else {
                    self.tree_state.selected = 0;
                }
                self.tree_state.offset = 0;
                self.update_cached_tree_len();
            }
            LayoutMode::Miller => {
                // In miller view, find the index in children
                if let Some((node, _)) = self.get_view_root_node() {
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

        // First, check if parent is within the main tree
        if parent_path.starts_with(&tree.root_path) || parent_path == tree.root_path {
            return Self::find_node_at_path(&tree.root, &parent_path, &tree.root_path);
        }

        // If we're at the tree root and have a cached parent, use that
        if self.view_root == tree.root_path {
            if let Some(cached) = &self.cached_parent_tree {
                return Some(&cached.root);
            }
        }

        None
    }

    /// Update the cached parent tree for Miller columns display.
    /// Called when navigating to ensure the parent column can be shown.
    fn update_cached_parent(&mut self) {
        let Some(tree) = &self.tree else { return };

        // Only need to cache parent if we're at or near the tree root
        if self.view_root == tree.root_path {
            if let Some(parent_path) = self.view_root.parent() {
                let parent_path_buf = parent_path.to_path_buf();
                // Load the parent directory
                if let Ok(mut parent_tree) = gravityfile_scan::quick_list(&parent_path_buf) {
                    // Merge any cached scans into the parent tree's children
                    // This ensures previously scanned directories show their data
                    self.merge_cached_scans_into_tree(&mut parent_tree.root, &parent_path_buf);
                    self.cached_parent_tree = Some(parent_tree);
                }
            }
        } else {
            // Don't need cached parent when navigating within the tree
            self.cached_parent_tree = None;
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
                            // Use sync to preserve selection when switching to tree
                            self.sync_layout_selection();
                        }
                    }
                    LayoutCommand::Miller => {
                        if self.layout_mode != LayoutMode::Miller {
                            // Use sync to preserve selection when switching to miller
                            self.sync_layout_selection();
                        }
                    }
                    LayoutCommand::Toggle => {
                        self.sync_layout_selection();
                    }
                }
            }
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
        self.forward_history.clear();
        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
        self.sync_to_active_tab();
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
                self.sync_to_active_tab();
            }
        }
    }

    /// Check if a path is marked for deletion.
    pub fn is_marked(&self, path: &PathBuf) -> bool {
        self.marked.contains(path)
    }

    /// Cycle to the next sort mode and apply sorting.
    fn cycle_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.apply_sort();
    }

    /// Reverse the current sort direction.
    fn reverse_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.reverse();
        self.apply_sort();
    }

    /// Sync current tab state to the active tab.
    fn sync_to_active_tab(&mut self) {
        if let Some(tab) = self.tab_manager.active_tab_mut() {
            tab.view_root = self.view_root.clone();
            tab.label = self.view_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.view_root.to_string_lossy().to_string());
        }
    }

    /// Sync app state from the active tab.
    fn sync_from_active_tab(&mut self) {
        if let Some(tab) = self.tab_manager.active_tab() {
            // Only sync if view_root is different
            if tab.view_root != self.view_root {
                self.view_root = tab.view_root.clone();
                // Reset tree state for the new view
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
                self.miller_state.reset();
                match self.layout_mode {
                    LayoutMode::Tree => self.update_cached_tree_len(),
                    LayoutMode::Miller => self.update_cached_miller_len(),
                }
            }
        }
    }

    /// Start fuzzy search mode.
    fn start_search(&mut self) {
        // Populate search paths from the current tree
        self.update_search_paths();
        self.search_state.activate();
        self.mode = AppMode::Search;
    }

    /// Update search paths from the file tree.
    fn update_search_paths(&mut self) {
        let mut paths = Vec::new();

        if let Some(tree) = &self.tree {
            Self::collect_paths_recursive(
                &tree.root,
                &tree.root_path,
                &self.view_root,
                &mut paths,
            );
        }

        self.search_state.update_paths(paths);
    }

    /// Recursively collect paths from the file tree.
    fn collect_paths_recursive(
        node: &gravityfile_core::FileNode,
        current_path: &PathBuf,
        view_root: &PathBuf,
        paths: &mut Vec<(PathBuf, String, bool)>,
    ) {
        // Only include paths under view_root
        if !current_path.starts_with(view_root) && current_path != view_root {
            return;
        }

        // Compute relative display path from view_root
        let display = if current_path == view_root {
            ".".to_string()
        } else {
            current_path
                .strip_prefix(view_root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| node.name.to_string())
        };

        paths.push((current_path.clone(), display, node.is_dir()));

        // Recurse into children
        for child in &node.children {
            let child_path = current_path.join(&*child.name);
            Self::collect_paths_recursive(child, &child_path, view_root, paths);
        }
    }

    /// Handle key input during search mode.
    fn handle_search_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        match (key.code, key.modifiers) {
            // Cancel search
            (KeyCode::Esc, _) => {
                self.search_state.deactivate();
                self.mode = AppMode::Normal;
            }
            // Execute search (navigate to selected)
            (KeyCode::Enter, _) => {
                if let Some(result) = self.search_state.selected_result() {
                    let target_path = result.path.clone();
                    self.navigate_to_search_result(&target_path);
                }
                self.search_state.deactivate();
                self.mode = AppMode::Normal;
            }
            // Navigation in results
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.search_state.move_up();
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.search_state.move_down();
            }
            // Cycle search mode
            (KeyCode::Tab, _) => {
                self.search_state.cycle_mode();
            }
            // Cursor movement
            (KeyCode::Left, _) => {
                self.search_state.move_cursor_left();
            }
            (KeyCode::Right, _) => {
                self.search_state.move_cursor_right();
            }
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.search_state.move_cursor_start();
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.search_state.move_cursor_end();
            }
            // Delete
            (KeyCode::Backspace, _) => {
                self.search_state.delete_char_before();
            }
            (KeyCode::Delete, _) => {
                self.search_state.delete_char_at();
            }
            // Clear line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.search_state.clear_query();
            }
            // Type character
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.search_state.insert_char(c);
            }
            _ => {}
        }
    }

    /// Navigate to a search result path.
    fn navigate_to_search_result(&mut self, target: &PathBuf) {
        // If it's a directory, drill into it
        // If it's a file, navigate to its parent and select it
        if target.is_dir() {
            // Save current state
            let saved_expanded = self.tree_state.expanded.clone();
            let saved_selected = match self.layout_mode {
                LayoutMode::Tree => self.tree_state.selected,
                LayoutMode::Miller => self.miller_state.selected,
            };
            self.view_history.push((self.view_root.clone(), saved_selected, saved_expanded));

            // Navigate to directory
            self.view_root = target.clone();
            self.tree_state.selected = 0;
            self.tree_state.offset = 0;
            self.tree_state.expand(&self.view_root);
            self.miller_state.reset();

            match self.layout_mode {
                LayoutMode::Tree => self.update_cached_tree_len(),
                LayoutMode::Miller => self.update_cached_miller_len(),
            }
            self.sync_to_active_tab();
        } else {
            // It's a file - navigate to parent and select it
            if let Some(parent) = target.parent() {
                // Save current state
                let saved_expanded = self.tree_state.expanded.clone();
                let saved_selected = match self.layout_mode {
                    LayoutMode::Tree => self.tree_state.selected,
                    LayoutMode::Miller => self.miller_state.selected,
                };
                self.view_history.push((self.view_root.clone(), saved_selected, saved_expanded));

                // Navigate to parent
                self.view_root = parent.to_path_buf();
                self.tree_state.expand(&self.view_root);

                // Get file name before borrowing for position search
                let file_name = target.file_name().and_then(|n| n.to_str()).map(|s| s.to_string());
                let target_clone = target.clone();

                match self.layout_mode {
                    LayoutMode::Tree => {
                        // For tree view, we need to find the index in flattened list
                        self.tree_state.selected = 0;
                        self.update_cached_tree_len();

                        // Find position in flattened tree
                        if let Some((node, _)) = self.get_view_root_node() {
                            let items = TreeView::new(node, &self.view_root, &self.theme, &self.marked, &self.clipboard)
                                .flatten(&self.tree_state);
                            if let Some(pos) = items.iter().position(|i| i.path == target_clone) {
                                self.tree_state.selected = pos;
                            }
                        }
                    }
                    LayoutMode::Miller => {
                        // Find the index of the file in the parent's children
                        if let (Some(file_name), Some((node, _))) = (&file_name, self.get_view_root_node()) {
                            let idx = node
                                .children
                                .iter()
                                .position(|c| c.name.as_str() == file_name)
                                .unwrap_or(0);
                            self.miller_state.selected = idx;
                        }
                        self.update_cached_miller_len();
                    }
                }
                self.sync_to_active_tab();
            }
        }
    }

    /// Handle key input during settings mode.
    fn handle_settings_input(&mut self, action: KeyAction) {
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
            KeyAction::Sort => {
                // 's' saves settings
                if let Some(ref state) = self.settings_state {
                    if state.dirty {
                        // Save settings to disk
                        self.user_settings = state.settings.clone();
                        if let Err(e) = self.user_settings.save() {
                            self.error = Some(format!("Failed to save settings: {}", e));
                        } else {
                            // Apply settings immediately where applicable
                            self.scan_on_startup = self.user_settings.scan_on_startup;
                        }
                    }
                }
                self.settings_state = None;
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
    }

    /// Apply the current sort mode to the file tree.
    fn apply_sort(&mut self) {
        if let Some(ref mut tree) = self.tree {
            Self::sort_node_recursive(&mut tree.root, self.sort_mode);
            // Update cached lengths after sorting
            match self.layout_mode {
                LayoutMode::Tree => self.update_cached_tree_len(),
                LayoutMode::Miller => self.update_cached_miller_len(),
            }
        }
    }

    /// Recursively sort a node and all its children.
    fn sort_node_recursive(node: &mut gravityfile_core::FileNode, mode: SortMode) {
        // Sort children based on mode
        node.children.sort_by(|a, b| {
            match mode {
                SortMode::SizeDescending => b.size.cmp(&a.size),
                SortMode::SizeAscending => a.size.cmp(&b.size),
                SortMode::NameAscending => a.name.cmp(&b.name),
                SortMode::NameDescending => b.name.cmp(&a.name),
                SortMode::ModifiedDescending => b.timestamps.modified.cmp(&a.timestamps.modified),
                SortMode::ModifiedAscending => a.timestamps.modified.cmp(&b.timestamps.modified),
                SortMode::CountDescending => {
                    let a_count = a.children.len();
                    let b_count = b.children.len();
                    b_count.cmp(&a_count)
                }
                SortMode::CountAscending => {
                    let a_count = a.children.len();
                    let b_count = b.children.len();
                    a_count.cmp(&b_count)
                }
            }
        });

        // Recursively sort children
        for child in &mut node.children {
            Self::sort_node_recursive(child, mode);
        }
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
            duplicates_state: &self.duplicates_state,
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
            sort_mode: self.sort_mode,
            search_state: &self.search_state,
            tab_manager: &self.tab_manager,
            preview_content: &self.preview_state.content,
            preview_mode: self.preview_state.mode,
            has_full_scan: self.has_full_scan,
            settings_state: self.settings_state.as_ref(),
        };

        render_app(&ctx, area, buf);
    }
}
