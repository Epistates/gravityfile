//! Main application state and logic.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs, Widget};
use ratatui::{DefaultTerminal, Frame};
use strum::{Display, EnumIter, FromRepr, IntoEnumIterator};
use tokio::sync::mpsc;

use gravityfile_analyze::{
    AgeAnalyzer, AgeConfig, AgeReport, DuplicateConfig, DuplicateFinder, DuplicateReport,
};
use gravityfile_core::FileTree;
use gravityfile_scan::{JwalkScanner, ScanConfig, ScanProgress};

use crate::event::KeyAction;
use crate::theme::Theme;
use crate::ui::{format_relative_time, format_size, AppLayout, HelpOverlay, TreeState, TreeView};

/// Application result type.
pub type AppResult<T> = color_eyre::Result<T>;

/// Application mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppMode {
    #[default]
    Normal,
    Scanning,
    Help,
    /// Confirming deletion of marked items.
    ConfirmDelete,
    /// Deletion in progress.
    Deleting,
    /// Command palette input mode (vim-style :command).
    Command,
    Quit,
}

/// Active view/tab during normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumIter, FromRepr)]
pub enum View {
    #[default]
    Explorer,
    Duplicates,
    Age,
    Errors,
}

/// Active view/tab during scanning mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanView {
    #[default]
    Progress,
    Errors,
}

impl ScanView {
    fn next(self) -> Self {
        match self {
            ScanView::Progress => ScanView::Errors,
            ScanView::Errors => ScanView::Progress,
        }
    }

    fn prev(self) -> Self {
        self.next() // Only 2 options, so same as next
    }
}

impl View {
    fn next(self) -> Self {
        let current = self as usize;
        let next = (current + 1) % Self::iter().count();
        Self::from_repr(next).unwrap_or_default()
    }

    fn prev(self) -> Self {
        let current = self as usize;
        let count = Self::iter().count();
        let prev = (current + count - 1) % count;
        Self::from_repr(prev).unwrap_or_default()
    }
}

/// Progress during deletion operation.
#[derive(Debug, Clone)]
struct DeletionProgress {
    /// Total items to delete.
    total: usize,
    /// Items deleted so far.
    deleted: usize,
    /// Items that failed to delete.
    failed: usize,
    /// Bytes freed so far.
    bytes_freed: u64,
    /// Current item being deleted.
    current: Option<PathBuf>,
}

/// Result from a background scan operation.
enum ScanResult {
    Progress(ScanProgress),
    #[allow(dead_code)] // For future real-time warning streaming
    Warning(gravityfile_core::ScanWarning),
    ScanComplete(Result<FileTree, gravityfile_scan::ScanError>),
    AnalysisComplete {
        duplicates: DuplicateReport,
        age_report: AgeReport,
    },
    /// Progress update during deletion.
    DeletionProgress(DeletionProgress),
    /// Deletion completed.
    DeletionComplete {
        deleted: usize,
        failed: usize,
        bytes_freed: u64,
    },
}

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
    /// Paths marked for deletion.
    marked_for_deletion: HashSet<PathBuf>,
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
    /// Command palette input buffer.
    command_input: String,
    /// Command palette cursor position.
    command_cursor: usize,
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
            marked_for_deletion: HashSet::new(),
            deletion_message: None,
            deletion_progress: None,
            scan_progress: None,
            scan_rx: None,
            warnings: Vec::new(),
            selected_warning: 0,
            analyzing: false,
            needs_redraw: true,
            command_input: String::new(),
            command_cursor: 0,
        }
    }

    /// Run the application with async event loop.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> AppResult<()> {
        // Start initial scan
        self.start_scan();

        // Use a longer interval - only for catching background updates
        let period = Duration::from_millis(50);
        let mut interval = tokio::time::interval(period);
        let mut events = EventStream::new();

        // Main async event loop using tokio::select!
        while self.mode != AppMode::Quit {
            // Only render when needed
            if self.needs_redraw {
                terminal.draw(|frame| self.render(frame))?;
                self.needs_redraw = false;
            }

            tokio::select! {
                biased; // Prioritize in order listed

                // Handle terminal events first - drain all pending events before rendering
                Some(Ok(event)) = events.next() => {
                    if let Event::Key(key_event) = event {
                        // Only process key press events, ignore releases and repeats for navigation
                        if key_event.kind == crossterm::event::KeyEventKind::Press {
                            // Command mode uses raw key input
                            if self.mode == AppMode::Command {
                                self.handle_command_input(key_event);
                            } else {
                                let action = KeyAction::from_key_event(key_event);
                                self.handle_action(action);
                            }
                        }
                    }

                    // Drain any additional pending events (prevents input lag)
                    while crossterm::event::poll(Duration::ZERO)? {
                        if let Ok(Event::Key(key_event)) = crossterm::event::read() {
                            if key_event.kind == crossterm::event::KeyEventKind::Press {
                                if self.mode == AppMode::Command {
                                    self.handle_command_input(key_event);
                                } else {
                                    let action = KeyAction::from_key_event(key_event);
                                    self.handle_action(action);
                                }
                                // Exit early if quitting
                                if self.mode == AppMode::Quit {
                                    break;
                                }
                            }
                        }
                    }
                    self.needs_redraw = true;
                }

                // Handle scan progress/completion
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

                // Periodic tick for background updates (scan progress)
                _ = interval.tick() => {
                    // Just tick - scan results will set needs_redraw
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

        let (tx, rx) = mpsc::channel(100);
        self.scan_rx = Some(rx);

        let path = self.path.clone();

        // Spawn background scan task
        tokio::spawn(async move {
            let config = ScanConfig::new(&path);
            let scanner = JwalkScanner::new();
            let mut progress_rx = scanner.subscribe();

            // Spawn task to forward progress updates
            let tx_progress = tx.clone();
            let progress_task = tokio::spawn(async move {
                while let Ok(progress) = progress_rx.recv().await {
                    if tx_progress.send(ScanResult::Progress(progress)).await.is_err() {
                        break;
                    }
                }
            });

            // Run scan in blocking task (jwalk uses rayon internally)
            let result = tokio::task::spawn_blocking(move || scanner.scan(&config))
                .await
                .unwrap_or_else(|e| Err(gravityfile_scan::ScanError::Other {
                    message: e.to_string(),
                }));

            // Cancel progress task and send final result
            progress_task.abort();
            let _ = tx.send(ScanResult::ScanComplete(result)).await;
        });
    }

    /// Handle a scan result from the background task.
    fn handle_scan_result(&mut self, result: ScanResult) {
        match result {
            ScanResult::Progress(progress) => {
                self.scan_progress = Some(progress);
            }
            ScanResult::Warning(warning) => {
                // Real-time warning collection during scan
                self.warnings.push(warning);
            }
            ScanResult::ScanComplete(Ok(tree)) => {
                let root_path = tree.root_path.clone();

                // Merge any final warnings from tree (in case some weren't streamed)
                for warning in &tree.warnings {
                    if !self.warnings.iter().any(|w| w.path == warning.path) {
                        self.warnings.push(warning.clone());
                    }
                }

                // Clone tree for analysis before storing
                let tree_for_analysis = tree.clone();

                self.tree = Some(tree);
                self.tree_state = TreeState::new(root_path);
                self.update_cached_tree_len();
                self.mode = AppMode::Normal;
                self.error = None;
                self.scan_progress = None;

                // Start background analysis (after storing tree)
                self.analyzing = true;
                self.start_analysis(tree_for_analysis);
            }
            ScanResult::ScanComplete(Err(e)) => {
                self.error = Some(e.to_string());
                self.mode = AppMode::Normal;
                self.scan_progress = None;
                self.scan_rx = None;
                self.analyzing = false;
            }
            ScanResult::AnalysisComplete { duplicates, age_report } => {
                self.duplicates = Some(duplicates);
                self.age_report = Some(age_report);
                self.analyzing = false;
                self.scan_rx = None;
            }
            ScanResult::DeletionProgress(progress) => {
                self.deletion_progress = Some(progress);
            }
            ScanResult::DeletionComplete { deleted, failed, bytes_freed } => {
                self.deletion_progress = None;
                self.scan_rx = None;

                let msg = if failed == 0 {
                    format!(
                        "Deleted {} items, freed {}",
                        deleted,
                        format_size(bytes_freed)
                    )
                } else {
                    format!(
                        "Deleted {}, failed {} (freed {})",
                        deleted,
                        failed,
                        format_size(bytes_freed)
                    )
                };

                self.deletion_message = Some((failed == 0, msg));
                self.mode = AppMode::Normal;

                // Refresh scan after deletion
                self.start_scan();
            }
        }
    }

    /// Update cached tree item count.
    fn update_cached_tree_len(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme)
                .flatten(&self.tree_state);
            self.cached_tree_len = items.len();
        } else {
            self.cached_tree_len = 0;
        }
    }

    /// Start background analysis of the tree.
    fn start_analysis(&mut self, tree: FileTree) {
        // Create a new channel for analysis results
        let (tx, rx) = mpsc::channel(10);
        self.scan_rx = Some(rx);

        tokio::spawn(async move {
            // Run analysis in blocking task
            let result = tokio::task::spawn_blocking(move || {
                let dup_config = DuplicateConfig::builder()
                    .min_size(1024u64)
                    .max_groups(100usize)
                    .build()
                    .unwrap();
                let finder = DuplicateFinder::with_config(dup_config);
                let duplicates = finder.find_duplicates(&tree);

                let age_config = AgeConfig::default();
                let analyzer = AgeAnalyzer::with_config(age_config);
                let age_report = analyzer.analyze(&tree);

                (duplicates, age_report)
            })
            .await;

            if let Ok((duplicates, age_report)) = result {
                let _ = tx.send(ScanResult::AnalysisComplete { duplicates, age_report }).await;
            }
        });
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
                    KeyAction::ToggleHelp | KeyAction::Quit => {
                        // Return to scanning mode if scan is still in progress
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
                    KeyAction::Confirm => {
                        // execute_deletion sets mode to Deleting
                        self.execute_deletion();
                    }
                    KeyAction::Quit | KeyAction::Cancel => {
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            AppMode::Deleting => {
                // During deletion, ignore all input - deletion cannot be cancelled
                // The UI will update with progress automatically
                return;
            }
            AppMode::Command => {
                // Command mode is handled separately via raw key events
                // This branch handles mapped actions that might come through
                match action {
                    KeyAction::Quit | KeyAction::Cancel => {
                        self.command_input.clear();
                        self.command_cursor = 0;
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            _ => {}
        }

        match action {
            KeyAction::Quit | KeyAction::ForceQuit => {
                self.mode = AppMode::Quit;
            }

            // View switching - different behavior during scan
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

            // Navigation based on current view
            KeyAction::MoveUp => self.move_up(),
            KeyAction::MoveDown => self.move_down(),
            KeyAction::PageUp => self.page_up(),
            KeyAction::PageDown => self.page_down(),
            KeyAction::JumpToTop => self.jump_to_top(),
            KeyAction::JumpToBottom => self.jump_to_bottom(),

            KeyAction::MoveRight | KeyAction::Expand => {
                if self.view == View::Explorer {
                    self.expand_selected();
                }
            }
            KeyAction::MoveLeft | KeyAction::Collapse => {
                if self.view == View::Explorer {
                    self.collapse_selected();
                }
            }
            KeyAction::ToggleExpand => {
                if self.view == View::Explorer {
                    self.toggle_selected();
                }
            }

            // Deletion
            KeyAction::Delete => {
                self.toggle_mark_for_deletion();
            }
            KeyAction::Confirm => {
                // Enter: if items are marked, show confirmation; otherwise toggle expand in explorer
                if !self.marked_for_deletion.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                } else if self.view == View::Explorer {
                    self.toggle_selected();
                }
            }
            KeyAction::ClearMarks => {
                self.marked_for_deletion.clear();
            }

            // Directory drill-down navigation
            KeyAction::DrillDown => {
                // If items are marked for deletion, Enter shows confirmation instead of drilling
                if !self.marked_for_deletion.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                } else if self.view == View::Explorer {
                    self.drill_into_selected();
                }
            }
            KeyAction::NavigateBack => {
                if self.view == View::Explorer {
                    self.navigate_back();
                }
            }

            // Command palette
            KeyAction::CommandMode => {
                self.command_input.clear();
                self.command_cursor = 0;
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
            KeyAction::Refresh => {
                self.marked_for_deletion.clear();
                self.start_scan();
            }

            _ => {}
        }
    }

    /// Toggle marking the currently selected item for deletion.
    fn toggle_mark_for_deletion(&mut self) {
        let path = match self.view {
            View::Explorer => {
                if let Some((node, root_path)) = self.get_view_root_node() {
                    let items = TreeView::new(node, &root_path, &self.theme)
                        .flatten(&self.tree_state);
                    items.get(self.tree_state.selected).map(|i| i.path.clone())
                } else {
                    None
                }
            }
            View::Duplicates => {
                // Use filtered duplicates for current view
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let selected = self.selected_dup_group.min(filtered.len().saturating_sub(1));
                    filtered.get(selected).and_then(|g| {
                        // Get first path that's within the current view
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
                // Use filtered stale directories for current view
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let selected = self.selected_stale_dir.min(filtered.len().saturating_sub(1));
                    filtered.get(selected).map(|d| d.path.clone())
                } else {
                    None
                }
            }
            View::Errors => {
                // Errors view doesn't support marking for deletion
                None
            }
        };

        if let Some(path) = path {
            // Don't allow marking the root path
            if path != self.path {
                if self.marked_for_deletion.contains(&path) {
                    self.marked_for_deletion.remove(&path);
                } else {
                    self.marked_for_deletion.insert(path);
                }
            }
        }
    }

    /// Execute deletion of all marked items asynchronously.
    fn execute_deletion(&mut self) {
        // Collect paths and sizes before clearing
        let paths_with_sizes: Vec<(PathBuf, u64)> = self
            .marked_for_deletion
            .iter()
            .map(|p| (p.clone(), self.get_path_size(p).unwrap_or(0)))
            .collect();

        let total = paths_with_sizes.len();
        if total == 0 {
            return;
        }

        self.marked_for_deletion.clear();
        self.mode = AppMode::Deleting;
        self.deletion_progress = Some(DeletionProgress {
            total,
            deleted: 0,
            failed: 0,
            bytes_freed: 0,
            current: paths_with_sizes.first().map(|(p, _)| p.clone()),
        });

        // Create channel for deletion progress
        let (tx, rx) = mpsc::channel(100);
        self.scan_rx = Some(rx);

        // Spawn background deletion task
        tokio::spawn(async move {
            let mut deleted = 0;
            let mut failed = 0;
            let mut bytes_freed: u64 = 0;

            for (i, (path, size)) in paths_with_sizes.iter().enumerate() {
                // Send progress update
                let _ = tx
                    .send(ScanResult::DeletionProgress(DeletionProgress {
                        total,
                        deleted,
                        failed,
                        bytes_freed,
                        current: Some(path.clone()),
                    }))
                    .await;

                // Perform deletion in blocking task to not block the async runtime
                let path_clone = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    if path_clone.is_dir() {
                        fs::remove_dir_all(&path_clone)
                    } else {
                        fs::remove_file(&path_clone)
                    }
                })
                .await;

                match result {
                    Ok(Ok(())) => {
                        deleted += 1;
                        bytes_freed += size;
                    }
                    _ => {
                        failed += 1;
                    }
                }

                // Send updated progress after each deletion
                if i < total - 1 {
                    let _ = tx
                        .send(ScanResult::DeletionProgress(DeletionProgress {
                            total,
                            deleted,
                            failed,
                            bytes_freed,
                            current: paths_with_sizes.get(i + 1).map(|(p, _)| p.clone()),
                        }))
                        .await;
                }
            }

            // Send completion
            let _ = tx
                .send(ScanResult::DeletionComplete {
                    deleted,
                    failed,
                    bytes_freed,
                })
                .await;
        });
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

    fn move_up(&mut self) {
        match self.view {
            View::Explorer => self.tree_state.move_up(1),
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
            View::Explorer => {
                self.tree_state.move_down(1, self.cached_tree_len);
            }
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
            View::Explorer => self.tree_state.move_up(10),
            View::Duplicates => {
                self.selected_dup_group = self.selected_dup_group.saturating_sub(10);
            }
            View::Age => {
                self.selected_stale_dir = self.selected_stale_dir.saturating_sub(10);
            }
            View::Errors => {
                self.selected_warning = self.selected_warning.saturating_sub(10);
            }
        }
    }

    fn page_down(&mut self) {
        match self.view {
            View::Explorer => {
                self.tree_state.move_down(10, self.cached_tree_len);
            }
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_dup_group = (self.selected_dup_group + 10).min(max);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_stale_dir = (self.selected_stale_dir + 10).min(max);
                }
            }
            View::Errors => {
                let max = self.warnings.len().saturating_sub(1);
                self.selected_warning = (self.selected_warning + 10).min(max);
            }
        }
    }

    fn jump_to_top(&mut self) {
        match self.view {
            View::Explorer => self.tree_state.jump_to_top(),
            View::Duplicates => self.selected_dup_group = 0,
            View::Age => self.selected_stale_dir = 0,
            View::Errors => self.selected_warning = 0,
        }
    }

    fn jump_to_bottom(&mut self) {
        match self.view {
            View::Explorer => {
                self.tree_state.jump_to_bottom(self.cached_tree_len);
            }
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
            let items =
                TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn collapse_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items =
                TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.collapse(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn toggle_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items =
                TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.toggle_expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn get_selected_info(&self) -> Option<SelectedInfo> {
        let tree = self.tree.as_ref()?;
        let (view_node, view_path) = self.get_view_root_node()?;
        let items =
            TreeView::new(view_node, &view_path, &self.theme).flatten(&self.tree_state);
        let item = items.get(self.tree_state.selected)?;

        // Find node in full tree for complete info
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

    /// Drill into the currently selected directory.
    fn drill_into_selected(&mut self) {
        let Some((view_node, view_path)) = self.get_view_root_node() else { return };

        // Get the selected item
        let items = TreeView::new(view_node, &view_path, &self.theme)
            .flatten(&self.tree_state);
        let Some(item) = items.get(self.tree_state.selected) else { return };

        // Only drill into directories
        if !matches!(item.node.kind, crate::ui::VisibleNodeKind::Directory { .. }) {
            return;
        }

        // Don't drill into if already at view root (can use expand instead)
        if item.path == self.view_root {
            // Just expand instead
            self.tree_state.expand(&item.path);
            self.update_cached_tree_len();
            return;
        }

        // Save the entire state before drilling: view_root, selected index, and all expanded paths
        let saved_expanded = self.tree_state.expanded.clone();
        let saved_selected = self.tree_state.selected;
        self.view_history.push((self.view_root.clone(), saved_selected, saved_expanded));

        // Set new view root
        self.view_root = item.path.clone();

        // Reset selection and expand the new root
        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
    }

    /// Navigate back up to the previous view root.
    fn navigate_back(&mut self) {
        // If we have history, restore the entire saved state
        if let Some((prev_root, saved_selected, saved_expanded)) = self.view_history.pop() {
            // Restore view root
            self.view_root = prev_root;

            // Restore the entire expanded set (this preserves exactly what was open/closed)
            self.tree_state.expanded = saved_expanded;

            // Restore selection position
            self.tree_state.selected = saved_selected;
            self.tree_state.offset = 0;
            self.update_cached_tree_len();
        } else if self.view_root != self.path {
            // No history but view_root differs from scan root - go to parent
            if let Some(parent) = self.view_root.parent() {
                // Check if parent is within or equal to scan root
                if parent.starts_with(&self.path) || parent == self.path {
                    // Collapse the current directory when backing out
                    let dir_we_drilled_into = self.view_root.clone();
                    self.tree_state.collapse(&dir_we_drilled_into);

                    self.view_root = parent.to_path_buf();
                    self.tree_state.expand(&self.view_root);
                    self.tree_state.selected = 0;
                    self.tree_state.offset = 0;
                    self.update_cached_tree_len();
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

    /// Get the current view root node (for drill-down rendering).
    fn get_view_root_node(&self) -> Option<(&gravityfile_core::FileNode, PathBuf)> {
        let tree = self.tree.as_ref()?;
        let node = Self::find_node_at_path(&tree.root, &self.view_root, &tree.root_path)?;
        Some((node, self.view_root.clone()))
    }

    /// Handle raw key input for command mode.
    fn handle_command_input(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            // Execute command on Enter
            (KeyCode::Enter, _) => {
                let cmd = self.command_input.clone();
                self.command_input.clear();
                self.command_cursor = 0;
                self.mode = AppMode::Normal;
                self.execute_command(&cmd);
            }
            // Cancel on Escape
            (KeyCode::Esc, _) => {
                self.command_input.clear();
                self.command_cursor = 0;
                self.mode = AppMode::Normal;
            }
            // Delete char before cursor
            (KeyCode::Backspace, _) => {
                if self.command_cursor > 0 {
                    self.command_cursor -= 1;
                    self.command_input.remove(self.command_cursor);
                } else if self.command_input.is_empty() {
                    // Exit command mode if input is empty and backspace pressed
                    self.mode = AppMode::Normal;
                }
            }
            // Delete char at cursor
            (KeyCode::Delete, _) => {
                if self.command_cursor < self.command_input.len() {
                    self.command_input.remove(self.command_cursor);
                }
            }
            // Move cursor
            (KeyCode::Left, _) => {
                self.command_cursor = self.command_cursor.saturating_sub(1);
            }
            (KeyCode::Right, _) => {
                self.command_cursor = (self.command_cursor + 1).min(self.command_input.len());
            }
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.command_cursor = 0;
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.command_cursor = self.command_input.len();
            }
            // Clear line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.command_input.clear();
                self.command_cursor = 0;
            }
            // Type character
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.command_input.insert(self.command_cursor, c);
                self.command_cursor += 1;
            }
            _ => {}
        }
    }

    /// Execute a command from the command palette.
    fn execute_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd.trim().split_whitespace().collect();
        if parts.is_empty() {
            return;
        }

        match parts[0] {
            // Quit commands
            "q" | "quit" | "exit" => {
                self.mode = AppMode::Quit;
            }
            // Refresh/rescan
            "r" | "refresh" | "rescan" => {
                self.marked_for_deletion.clear();
                self.start_scan();
            }
            // Change directory (cd path)
            "cd" => {
                if parts.len() > 1 {
                    let target = parts[1..].join(" ");
                    self.cd_to_path(&target);
                } else {
                    // cd with no args goes to scan root
                    self.go_to_root();
                }
            }
            // Go to root
            "root" | "top" => {
                self.go_to_root();
            }
            // Go back
            "back" | "up" | ".." => {
                self.navigate_back();
            }
            // Help
            "help" | "?" => {
                self.mode = AppMode::Help;
            }
            // View switching
            "explorer" | "e" | "tree" => {
                self.view = View::Explorer;
            }
            "duplicates" | "dups" | "d" => {
                self.view = View::Duplicates;
            }
            "age" | "a" => {
                self.view = View::Age;
            }
            "errors" | "err" => {
                self.view = View::Errors;
            }
            // Clear marks
            "clear" | "unmark" => {
                self.marked_for_deletion.clear();
            }
            // Toggle details
            "details" | "info" | "i" => {
                self.show_details = !self.show_details;
            }
            // Theme commands
            "theme" | "t" => {
                if parts.len() > 1 {
                    match parts[1] {
                        "dark" => self.theme = Theme::dark(),
                        "light" => self.theme = Theme::light(),
                        "toggle" => self.theme = self.theme.toggle(),
                        _ => {}
                    }
                } else {
                    // Toggle by default
                    self.theme = self.theme.toggle();
                }
            }
            "dark" => {
                self.theme = Theme::dark();
            }
            "light" => {
                self.theme = Theme::light();
            }
            _ => {
                // Unknown command - could show error message
            }
        }
    }

    /// Go to the scan root (resetting navigation history).
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
        // Handle special navigation commands first (don't need tree)
        if target == "/" || target == "~" {
            self.go_to_root();
            return;
        }
        if target == ".." {
            self.navigate_back();
            return;
        }

        // For other paths, we need the tree to verify the target exists
        let Some(tree) = &self.tree else { return };

        let target_path = if target.starts_with('/') {
            // Absolute path (but constrained to scan root)
            let relative = target.trim_start_matches('/');
            self.path.join(relative)
        } else {
            // Relative path from current view_root
            self.view_root.join(target)
        };

        // Verify the target exists in the tree and is a directory
        if let Some(node) = Self::find_node_at_path(&tree.root, &target_path, &tree.root_path) {
            if node.is_dir() {
                // Save current state before navigating
                let saved_expanded = self.tree_state.expanded.clone();
                let saved_selected = self.tree_state.selected;
                self.view_history.push((self.view_root.clone(), saved_selected, saved_expanded));

                self.view_root = target_path;
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
                self.update_cached_tree_len();
            }
        }
    }
}

struct SelectedInfo {
    name: String,
    path: PathBuf,
    size: u64,
    file_count: u64,
    dir_count: u64,
    modified: std::time::SystemTime,
    is_dir: bool,
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Fill entire area with theme background color
        let base_style = Style::default().bg(self.theme.background).fg(self.theme.foreground);
        buf.set_style(area, base_style);

        // Layout: header, tabs, content, footer
        let [header, tabs_area, content, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .areas(area);

        // Render header
        self.render_header(header, buf);

        // Render tabs
        self.render_tabs(tabs_area, buf);

        // Render content based on mode and view
        // Show scan content if scanning is in progress (even if in Help mode overlay)
        let is_scanning = self.scan_progress.is_some() && self.tree.is_none();

        if is_scanning {
            match self.scan_view {
                ScanView::Progress => self.render_scanning(content, buf),
                ScanView::Errors => self.render_errors(content, buf),
            }
        } else {
            match self.view {
                View::Explorer => self.render_explorer(content, buf),
                View::Duplicates => self.render_duplicates(content, buf),
                View::Age => self.render_age(content, buf),
                View::Errors => self.render_errors(content, buf),
            }
        }

        // Render footer
        self.render_footer(footer, buf);

        // Render overlays
        match self.mode {
            AppMode::Help => {
                HelpOverlay::new(&self.theme).render(area, buf);
            }
            AppMode::ConfirmDelete => {
                self.render_delete_confirmation(area, buf);
            }
            AppMode::Deleting => {
                self.render_deletion_progress(area, buf);
            }
            AppMode::Command => {
                self.render_command_palette(footer, buf);
            }
            _ => {}
        }
    }
}

impl App {
    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let title = Span::styled(
            " gravityfile ",
            self.theme.title.add_modifier(Modifier::BOLD),
        );

        let stats = if let Some(tree) = &self.tree {
            format!(
                " {} in {} files, {} dirs ",
                format_size(tree.stats.total_size),
                tree.stats.total_files,
                tree.stats.total_dirs
            )
        } else {
            String::new()
        };

        let stats_span = Span::styled(stats, self.theme.header);

        // Show marked items count or deletion message
        let status = if let Some((success, msg)) = &self.deletion_message {
            let color = if *success {
                self.theme.success
            } else {
                self.theme.warning
            };
            Span::styled(format!(" {} ", msg), Style::default().fg(color))
        } else if !self.marked_for_deletion.is_empty() {
            let total_size: u64 = self
                .marked_for_deletion
                .iter()
                .filter_map(|p| self.get_path_size(p))
                .sum();
            Span::styled(
                format!(
                    " {} marked ({}) [Enter to delete] ",
                    self.marked_for_deletion.len(),
                    format_size(total_size)
                ),
                Style::default().fg(self.theme.warning),
            )
        } else {
            Span::raw("")
        };

        let line = Line::from(vec![title, Span::raw(" "), stats_span, status]);

        Paragraph::new(line)
            .style(self.theme.header)
            .render(area, buf);
    }

    fn render_delete_confirmation(&self, area: Rect, buf: &mut Buffer) {
        // Calculate popup area
        let popup_width = 60.min(area.width.saturating_sub(4));
        let popup_height = (self.marked_for_deletion.len() as u16 + 8).min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the popup area
        Clear.render(popup_area, buf);

        // Draw border
        let block = Block::default()
            .title(" Confirm Deletion ")
            .title_style(Style::default().fg(self.theme.error).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.error));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // Calculate total size
        let total_size: u64 = self
            .marked_for_deletion
            .iter()
            .filter_map(|p| self.get_path_size(p))
            .sum();

        // Build content
        let mut lines = vec![
            Line::styled(
                format!(
                    "Delete {} items ({})? This cannot be undone!",
                    self.marked_for_deletion.len(),
                    format_size(total_size)
                ),
                Style::default().fg(self.theme.warning).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
        ];

        // List items (limited)
        let max_items = (inner.height as usize).saturating_sub(5);
        for path in self.marked_for_deletion.iter().take(max_items) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            let prefix = if path.is_dir() { "📁 " } else { "📄 " };
            lines.push(Line::raw(format!("  {}{}", prefix, name)));
        }

        if self.marked_for_deletion.len() > max_items {
            lines.push(Line::styled(
                format!("  ... and {} more", self.marked_for_deletion.len() - max_items),
                Style::default().fg(self.theme.muted),
            ));
        }

        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(" y/Enter ", self.theme.help_key),
            Span::raw("Confirm  "),
            Span::styled(" n/Esc ", self.theme.help_key),
            Span::raw("Cancel"),
        ]));

        Paragraph::new(lines).render(inner, buf);
    }

    fn render_deletion_progress(&self, area: Rect, buf: &mut Buffer) {
        // Calculate popup area
        let popup_width = 50.min(area.width.saturating_sub(4));
        let popup_height = 10.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the popup area
        Clear.render(popup_area, buf);

        // Draw border
        let block = Block::default()
            .title(" Deleting... ")
            .title_style(Style::default().fg(self.theme.warning).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.warning));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let mut lines = vec![];

        if let Some(progress) = &self.deletion_progress {
            // Progress bar
            let pct = if progress.total > 0 {
                ((progress.deleted + progress.failed) as f64 / progress.total as f64 * 100.0) as u16
            } else {
                0
            };
            let bar_width = (inner.width as usize).saturating_sub(10);
            let filled = (pct as usize * bar_width) / 100;
            let empty = bar_width.saturating_sub(filled);

            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::raw("  ["),
                Span::styled("█".repeat(filled), Style::default().fg(self.theme.info)),
                Span::styled("░".repeat(empty), Style::default().fg(self.theme.muted)),
                Span::raw(format!("] {}%", pct)),
            ]));

            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("  Progress: ", self.theme.help_desc),
                Span::raw(format!(
                    "{}/{} items",
                    progress.deleted + progress.failed,
                    progress.total
                )),
            ]));

            lines.push(Line::from(vec![
                Span::styled("  Freed:    ", self.theme.help_desc),
                Span::raw(format_size(progress.bytes_freed)),
            ]));

            if progress.failed > 0 {
                lines.push(Line::from(vec![
                    Span::styled("  Failed:   ", Style::default().fg(self.theme.error)),
                    Span::styled(
                        progress.failed.to_string(),
                        Style::default().fg(self.theme.error),
                    ),
                ]));
            }

            // Current item being deleted
            if let Some(current) = &progress.current {
                let name = current
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| current.display().to_string());
                let max_len = (inner.width as usize).saturating_sub(4);
                let display_name = if name.len() > max_len {
                    format!("...{}", &name[name.len().saturating_sub(max_len - 3)..])
                } else {
                    name
                };
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("  {}", display_name),
                    Style::default().fg(self.theme.muted),
                ));
            }
        } else {
            lines.push(Line::raw("  Preparing deletion..."));
        }

        Paragraph::new(lines).render(inner, buf);
    }

    fn render_tabs(&self, area: Rect, buf: &mut Buffer) {
        // Show scan tabs if scanning is in progress (even if in Help mode)
        let is_scanning = self.scan_progress.is_some() && self.tree.is_none();

        if is_scanning {
            // During scan, show only Progress and Errors tabs
            // Use scan_progress.errors_count since warnings aren't populated until scan completes
            let error_count = self.scan_progress
                .as_ref()
                .map(|p| p.errors_count)
                .unwrap_or(0);
            let titles = vec![
                " Progress ".to_string(),
                if error_count > 0 {
                    format!(" Errors ({}) ", error_count)
                } else {
                    " Errors ".to_string()
                },
            ];

            let tabs = Tabs::new(titles)
                .select(self.scan_view as usize)
                .style(self.theme.footer)
                .highlight_style(self.theme.selected);

            tabs.render(area, buf);
        } else {
            // Normal mode - show all tabs
            let titles: Vec<String> = View::iter().map(|v| format!(" {} ", v)).collect();

            let tabs = Tabs::new(titles)
                .select(self.view as usize)
                .style(self.theme.footer)
                .highlight_style(self.theme.selected);

            tabs.render(area, buf);
        }
    }

    fn render_scanning(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(format!(" Scanning {} ", self.path.display()))
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines = vec![
            Line::raw(""),
            Line::styled(
                "  Scanning directory...",
                Style::default().fg(self.theme.info).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
        ];

        if let Some(progress) = &self.scan_progress {
            lines.push(Line::from(vec![
                Span::styled("  Files: ", self.theme.help_desc),
                Span::raw(progress.files_scanned.to_string()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Dirs:  ", self.theme.help_desc),
                Span::raw(progress.dirs_scanned.to_string()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Size:  ", self.theme.help_desc),
                Span::raw(format_size(progress.bytes_scanned)),
            ]));

            if progress.errors_count > 0 {
                lines.push(Line::from(vec![
                    Span::styled("  Errors: ", Style::default().fg(self.theme.warning)),
                    Span::styled(
                        progress.errors_count.to_string(),
                        Style::default().fg(self.theme.warning),
                    ),
                ]));
            }

            // Show current path (truncated)
            let current = progress.current_path.display().to_string();
            let max_width = inner.width.saturating_sub(4) as usize;
            let display_path = if current.len() > max_width {
                format!("...{}", &current[current.len().saturating_sub(max_width - 3)..])
            } else {
                current
            };
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("  {}", display_path),
                Style::default().fg(self.theme.muted),
            ));
        }

        Paragraph::new(lines).render(inner, buf);
    }

    fn render_explorer(&self, area: Rect, buf: &mut Buffer) {
        let layout = AppLayout::new(area, self.show_details);

        if let Some((view_node, view_path)) = self.get_view_root_node() {
            // Build title showing navigation context
            let title = if self.view_root != self.path {
                // Show breadcrumb-style path when drilled in
                let relative = self.view_root.strip_prefix(&self.path)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| self.view_root.display().to_string());
                format!(" {} (← Backspace) ", relative)
            } else {
                format!(" {} ", view_path.display())
            };

            let tree_view = TreeView::new(view_node, &view_path, &self.theme).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.theme.border)
                    .title(title)
                    .title_style(self.theme.title),
            );

            let mut tree_state = self.tree_state.clone();
            ratatui::widgets::StatefulWidget::render(tree_view, layout.main, buf, &mut tree_state);
        } else if let Some(error) = &self.error {
            let error_block = Block::default()
                .borders(Borders::ALL)
                .border_style(self.theme.border)
                .title(" Error ")
                .title_style(Style::default().fg(self.theme.error));

            let error_text = Paragraph::new(error.as_str())
                .block(error_block)
                .style(Style::default().fg(self.theme.error));

            error_text.render(layout.main, buf);
        }

        if let Some(details_area) = layout.details {
            self.render_details(details_area, buf);
        }
    }

    fn render_duplicates(&self, area: Rect, buf: &mut Buffer) {
        // Build title showing context when drilled in
        let title = if self.view_root != self.path {
            let relative = self.view_root
                .strip_prefix(&self.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| self.view_root.display().to_string());
            format!(" Duplicates in {} ", relative)
        } else {
            " Duplicates ".to_string()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if let Some((filtered_groups, total_wasted)) = self.get_filtered_duplicates() {
            if filtered_groups.is_empty() {
                let msg = Paragraph::new("No duplicate files found in this directory.")
                    .style(Style::default().fg(self.theme.muted));
                msg.render(inner, buf);
                return;
            }

            // Header with filtered stats
            let header = format!(
                " {} groups, {} wasted",
                filtered_groups.len(),
                format_size(total_wasted)
            );
            let header_line = Line::styled(header, self.theme.title);
            let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
            Paragraph::new(header_line).render(header_area, buf);

            // List of groups
            let list_area = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
            let visible_height = list_area.height as usize;

            // Clamp selection to filtered list
            let selected = self.selected_dup_group.min(filtered_groups.len().saturating_sub(1));

            // Calculate scroll offset
            let scroll_offset = if selected >= visible_height {
                selected - visible_height + 1
            } else {
                0
            };

            for (i, group) in filtered_groups.iter().enumerate().skip(scroll_offset).take(visible_height) {
                let y = list_area.y + (i - scroll_offset) as u16;
                let is_selected = i == selected;

                // Count how many files are in the current view
                let files_in_view = group
                    .paths
                    .iter()
                    .filter(|p| p.starts_with(&self.view_root))
                    .count();
                let total_files = group.count();

                // Show file count with context if some are outside view
                let file_info = if files_in_view < total_files {
                    format!("{}/{} files", files_in_view, total_files)
                } else {
                    format!("{} files", total_files)
                };

                let line = format!(
                    " {}, {} each ({} wasted)",
                    file_info,
                    format_size(group.size),
                    format_size(group.wasted_bytes)
                );

                let style = if is_selected {
                    self.theme.selected
                } else {
                    Style::default()
                };

                let line = Line::styled(line, style);
                let line_area = Rect::new(list_area.x, y, list_area.width, 1);
                Paragraph::new(line).render(line_area, buf);
            }
        } else {
            let msg = Paragraph::new("Analyzing duplicates...")
                .style(Style::default().fg(self.theme.muted));
            msg.render(inner, buf);
        }
    }

    fn render_age(&self, area: Rect, buf: &mut Buffer) {
        // Build title showing context when drilled in
        let title = if self.view_root != self.path {
            let relative = self.view_root
                .strip_prefix(&self.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| self.view_root.display().to_string());
            format!(" Age Analysis - {} ", relative)
        } else {
            " Age Analysis ".to_string()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if let Some(age) = &self.age_report {
            // Age distribution chart (shows full scan data)
            let max_size = age.buckets.iter().map(|b| b.total_size).max().unwrap_or(1);
            let chart_height = age.buckets.len().min(inner.height as usize / 2);

            // Show note if drilled in that buckets are for full scan
            let bucket_start_y = if self.view_root != self.path {
                let note = Line::styled(
                    " Distribution (full scan):",
                    Style::default().fg(self.theme.muted),
                );
                let note_area = Rect::new(inner.x, inner.y, inner.width, 1);
                Paragraph::new(note).render(note_area, buf);
                inner.y + 1
            } else {
                inner.y
            };

            for (i, bucket) in age.buckets.iter().enumerate().take(chart_height) {
                let y = bucket_start_y + i as u16;
                if y >= inner.y + inner.height {
                    break;
                }
                let bar_width = if max_size > 0 {
                    ((bucket.total_size as f64 / max_size as f64) * 20.0) as usize
                } else {
                    0
                };

                let bar = "█".repeat(bar_width);
                let line = format!(
                    " {:<12} {:>10} {:>8} files  {}",
                    bucket.name,
                    format_size(bucket.total_size),
                    bucket.file_count,
                    bar
                );

                let line_area = Rect::new(inner.x, y, inner.width, 1);
                Paragraph::new(line).render(line_area, buf);
            }

            // Get filtered stale directories
            let filtered_stale = self.get_filtered_stale_dirs();
            let stale_dirs: Vec<&gravityfile_analyze::StaleDirectory> = filtered_stale
                .unwrap_or_default();

            // Stale directories header
            let stale_y = bucket_start_y + chart_height as u16 + 1;
            if stale_y < inner.y + inner.height {
                let total_stale_size: u64 = stale_dirs.iter().map(|d| d.size).sum();
                let stale_header = if stale_dirs.is_empty() {
                    " No stale directories found.".to_string()
                } else {
                    format!(
                        " Stale Directories ({}, {} total)",
                        stale_dirs.len(),
                        format_size(total_stale_size)
                    )
                };
                let header_area = Rect::new(inner.x, stale_y, inner.width, 1);
                Paragraph::new(Line::styled(stale_header, self.theme.title)).render(header_area, buf);

                // List stale directories
                let list_y = stale_y + 1;
                let list_height = (inner.y + inner.height).saturating_sub(list_y) as usize;

                // Clamp selection to filtered list
                let selected = self.selected_stale_dir.min(stale_dirs.len().saturating_sub(1));

                for (i, dir) in stale_dirs.iter().enumerate().take(list_height) {
                    let y = list_y + i as u16;
                    let is_selected = i == selected;

                    let line = format!(
                        "   {} ({}, {} old)",
                        dir.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                        format_size(dir.size),
                        gravityfile_analyze::format_age(dir.newest_file_age)
                    );

                    let style = if is_selected {
                        self.theme.selected
                    } else {
                        Style::default()
                    };

                    let line_area = Rect::new(inner.x, y, inner.width, 1);
                    Paragraph::new(Line::styled(line, style)).render(line_area, buf);
                }
            }
        } else {
            let msg = Paragraph::new("Analyzing file ages...")
                .style(Style::default().fg(self.theme.muted));
            msg.render(inner, buf);
        }
    }

    fn render_errors(&self, area: Rect, buf: &mut Buffer) {
        let title = if self.mode == AppMode::Scanning {
            // During scan, use error count from progress since warnings aren't populated yet
            let count = self.scan_progress
                .as_ref()
                .map(|p| p.errors_count)
                .unwrap_or(0);
            format!(" Errors & Warnings ({}) ", count)
        } else if !self.warnings.is_empty() {
            format!(" Scan Errors & Warnings ({}) ", self.warnings.len())
        } else {
            " Scan Errors & Warnings ".to_string()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if self.warnings.is_empty() {
            // During scanning, warnings aren't available yet - they come with the tree
            let lines = if self.mode == AppMode::Scanning {
                let error_count = self.scan_progress
                    .as_ref()
                    .map(|p| p.errors_count)
                    .unwrap_or(0);
                if error_count > 0 {
                    vec![
                        Line::raw(""),
                        Line::styled(
                            format!("  {} errors encountered during scan", error_count),
                            Style::default().fg(self.theme.warning),
                        ),
                        Line::raw(""),
                        Line::styled(
                            "  Error details will be available after scan completes.",
                            Style::default().fg(self.theme.muted),
                        ),
                    ]
                } else {
                    vec![Line::styled(
                        "No errors yet...",
                        Style::default().fg(self.theme.muted),
                    )]
                }
            } else {
                vec![Line::styled(
                    "No errors or warnings during scan.",
                    Style::default().fg(self.theme.muted),
                )]
            };
            Paragraph::new(lines).render(inner, buf);
            return;
        }

        // Each warning takes 2 lines: kind + path on first, message on second
        let lines_per_item = 2;
        let visible_items = inner.height as usize / lines_per_item;

        // Calculate scroll offset
        let scroll_offset = if self.selected_warning >= visible_items {
            self.selected_warning - visible_items + 1
        } else {
            0
        };

        for (i, warning) in self.warnings.iter().enumerate().skip(scroll_offset).take(visible_items) {
            let base_y = inner.y + ((i - scroll_offset) * lines_per_item) as u16;
            let is_selected = i == self.selected_warning;

            // Kind label with icon
            let (icon, kind_label) = match warning.kind {
                gravityfile_core::WarningKind::PermissionDenied => ("🔒", "Permission Denied"),
                gravityfile_core::WarningKind::BrokenSymlink => ("🔗", "Broken Symlink"),
                gravityfile_core::WarningKind::ReadError => ("⚠", "Read Error"),
                gravityfile_core::WarningKind::MetadataError => ("📋", "Metadata Error"),
                gravityfile_core::WarningKind::CrossFilesystem => ("💾", "Cross Filesystem"),
            };

            // First line: icon + kind + path
            let path_str = warning.path.display().to_string();
            let prefix = format!(" {} {} ", icon, kind_label);
            let available_width = (inner.width as usize).saturating_sub(prefix.len() + 1);
            let display_path = if path_str.len() > available_width {
                format!("...{}", &path_str[path_str.len().saturating_sub(available_width - 3)..])
            } else {
                path_str
            };

            let style = if is_selected {
                self.theme.selected
            } else {
                Style::default().fg(self.theme.warning)
            };

            let line1 = Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(display_path, style),
            ]);
            let line1_area = Rect::new(inner.x, base_y, inner.width, 1);
            Paragraph::new(line1).render(line1_area, buf);

            // Second line: message (indented, muted unless selected)
            if base_y + 1 < inner.y + inner.height {
                let msg_style = if is_selected {
                    self.theme.selected
                } else {
                    Style::default().fg(self.theme.muted)
                };

                // Truncate message if too long
                let max_msg_len = (inner.width as usize).saturating_sub(4);
                let msg = if warning.message.len() > max_msg_len {
                    format!("{}...", &warning.message[..max_msg_len.saturating_sub(3)])
                } else {
                    warning.message.clone()
                };

                let line2 = Line::styled(format!("    {}", msg), msg_style);
                let line2_area = Rect::new(inner.x, base_y + 1, inner.width, 1);
                Paragraph::new(line2).render(line2_area, buf);
            }
        }
    }

    fn render_details(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(" Details ")
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if let Some(info) = self.get_selected_info() {
            let mut lines = vec![
                Line::from(Span::styled(
                    &info.name,
                    self.theme.title.add_modifier(Modifier::BOLD),
                )),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("Size: ", self.theme.help_desc),
                    Span::raw(format_size(info.size)),
                ]),
            ];

            if info.is_dir {
                lines.push(Line::from(vec![
                    Span::styled("Files: ", self.theme.help_desc),
                    Span::raw(info.file_count.to_string()),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("Dirs: ", self.theme.help_desc),
                    Span::raw(info.dir_count.to_string()),
                ]));
            }

            lines.push(Line::from(vec![
                Span::styled("Modified: ", self.theme.help_desc),
                Span::raw(format_relative_time(info.modified)),
            ]));

            lines.push(Line::raw(""));
            lines.push(Line::styled("Path:", self.theme.help_desc));

            let path_str = info.path.display().to_string();
            let max_width = inner.width.saturating_sub(2) as usize;
            for chunk in path_str
                .chars()
                .collect::<Vec<_>>()
                .chunks(max_width)
                .map(|c| c.iter().collect::<String>())
            {
                lines.push(Line::raw(chunk));
            }

            Paragraph::new(lines).render(inner, buf);
        }
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let mut keys: Vec<(&str, &str)> = match self.view {
            View::Explorer => {
                let mut v = vec![
                    ("j/k", "Nav"),
                    ("h/l", "Fold"),
                    ("Enter", "Drill"),
                ];
                // Show Backspace hint when drilled in
                if self.view_root != self.path {
                    v.push(("⌫", "Back"));
                }
                v.push(("d", "Mark"));
                v
            }
            View::Duplicates | View::Age => vec![
                ("j/k", "Nav"),
                ("d", "Mark"),
            ],
            View::Errors => vec![
                ("j/k", "Nav"),
            ],
        };

        // Add deletion-related keys if items are marked
        if !self.marked_for_deletion.is_empty() {
            keys.push(("x", "Clear"));
            keys.push(("y", "Delete"));
        }

        keys.extend([(":", "Cmd"), ("Tab", "View"), ("?", "Help"), ("q", "Quit")]);

        let spans: Vec<Span> = keys
            .iter()
            .flat_map(|(key, desc)| {
                vec![
                    Span::styled(format!(" {key} "), self.theme.help_key),
                    Span::styled(format!("{desc} "), self.theme.help_desc),
                ]
            })
            .collect();

        let line = Line::from(spans);

        Paragraph::new(line)
            .style(self.theme.footer)
            .render(area, buf);
    }

    /// Render the command palette input line.
    fn render_command_palette(&self, area: Rect, buf: &mut Buffer) {
        // Clear the area first
        Clear.render(area, buf);

        // Build the command line display
        let prompt = ":";
        let input = &self.command_input;

        // Calculate cursor position for display
        let cursor_pos = self.command_cursor;

        // Build the line with cursor indication
        let mut spans = vec![
            Span::styled(prompt, Style::default().fg(self.theme.info).add_modifier(Modifier::BOLD)),
        ];

        if input.is_empty() {
            // Show placeholder
            spans.push(Span::styled(
                "type command (q, cd, help...)",
                Style::default().fg(self.theme.muted),
            ));
        } else {
            // Show input with cursor
            let (before, after) = input.split_at(cursor_pos.min(input.len()));
            spans.push(Span::raw(before));

            if !after.is_empty() {
                // Show cursor under next char
                let (cursor_char, rest) = after.split_at(1);
                spans.push(Span::styled(
                    cursor_char,
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
                spans.push(Span::raw(rest));
            } else {
                // Cursor at end - show block cursor
                spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
            }
        }

        let line = Line::from(spans);
        Paragraph::new(line)
            .style(self.theme.footer)
            .render(area, buf);
    }

    /// Check if a path is marked for deletion.
    pub fn is_marked(&self, path: &PathBuf) -> bool {
        self.marked_for_deletion.contains(path)
    }

    /// Get duplicate groups filtered to current view_root.
    /// Returns (filtered_groups, total_wasted_in_view).
    fn get_filtered_duplicates(&self) -> Option<(Vec<&gravityfile_analyze::DuplicateGroup>, u64)> {
        let dups = self.duplicates.as_ref()?;

        // If at scan root, no filtering needed
        if self.view_root == self.path {
            let total: u64 = dups.groups.iter().map(|g| g.wasted_bytes).sum();
            return Some((dups.groups.iter().collect(), total));
        }

        // Filter groups to those with at least one path in current view
        let filtered: Vec<&gravityfile_analyze::DuplicateGroup> = dups
            .groups
            .iter()
            .filter(|g| g.paths.iter().any(|p| p.starts_with(&self.view_root)))
            .collect();

        // Calculate wasted bytes only counting paths in view
        let total_wasted: u64 = filtered
            .iter()
            .map(|g| {
                let paths_in_view = g.paths.iter().filter(|p| p.starts_with(&self.view_root)).count();
                if paths_in_view > 1 {
                    // Wasted = size * (count_in_view - 1)
                    g.size * (paths_in_view as u64 - 1)
                } else {
                    // Only one copy in this view - no waste shown here
                    // (the duplicate is outside the current view)
                    0
                }
            })
            .sum();

        Some((filtered, total_wasted))
    }

    /// Get stale directories filtered to current view_root.
    fn get_filtered_stale_dirs(&self) -> Option<Vec<&gravityfile_analyze::StaleDirectory>> {
        let age = self.age_report.as_ref()?;

        // If at scan root, no filtering needed
        if self.view_root == self.path {
            return Some(age.stale_directories.iter().collect());
        }

        // Filter to directories within current view
        let filtered: Vec<&gravityfile_analyze::StaleDirectory> = age
            .stale_directories
            .iter()
            .filter(|d| d.path.starts_with(&self.view_root))
            .collect();

        Some(filtered)
    }
}
