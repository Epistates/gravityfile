//! Main application state and logic.

mod commands;
mod constants;
mod deletion;
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
use gravityfile_scan::ScanProgress;

use crate::event::KeyAction;
use crate::theme::Theme;
use crate::ui::{TreeState, TreeView};

use self::commands::{parse_command, CommandAction, CommandInput, CommandKeyResult, ThemeCommand};
use self::constants::{PAGE_SIZE, TICK_INTERVAL_MS};
use self::render::{render_app, RenderContext};
use self::state::{AppMode, DeletionProgress, ScanResult, ScanView, SelectedInfo, View};

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
            marked_for_deletion: HashSet::new(),
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
                self.tree_state = TreeState::new(root_path);
                self.update_cached_tree_len();
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
        }
    }

    /// Update cached tree item count.
    fn update_cached_tree_len(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
            self.cached_tree_len = items.len();
        } else {
            self.cached_tree_len = 0;
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
                    KeyAction::ToggleHelp | KeyAction::Quit => {
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
            _ => {}
        }

        match action {
            KeyAction::Quit | KeyAction::ForceQuit => {
                self.mode = AppMode::Quit;
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

            KeyAction::Delete => {
                self.toggle_mark_for_deletion();
            }
            KeyAction::Confirm => {
                if !self.marked_for_deletion.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                } else if self.view == View::Explorer {
                    self.toggle_selected();
                }
            }
            KeyAction::ClearMarks => {
                self.marked_for_deletion.clear();
            }

            KeyAction::DrillDown => {
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
                    let items =
                        TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
                    items.get(self.tree_state.selected).map(|i| i.path.clone())
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

    // Navigation methods
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
            View::Explorer => self.tree_state.move_up(PAGE_SIZE),
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
            View::Explorer => {
                self.tree_state.move_down(PAGE_SIZE, self.cached_tree_len);
            }
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
            let items = TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn collapse_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.collapse(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn toggle_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme).flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.toggle_expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn get_selected_info(&self) -> Option<SelectedInfo> {
        let tree = self.tree.as_ref()?;
        let (view_node, view_path) = self.get_view_root_node()?;
        let items = TreeView::new(view_node, &view_path, &self.theme).flatten(&self.tree_state);
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

    /// Drill into the currently selected directory.
    fn drill_into_selected(&mut self) {
        let Some((view_node, view_path)) = self.get_view_root_node() else {
            return;
        };

        let items = TreeView::new(view_node, &view_path, &self.theme).flatten(&self.tree_state);
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

    /// Navigate back up to the previous view root.
    fn navigate_back(&mut self) {
        if let Some((prev_root, saved_selected, saved_expanded)) = self.view_history.pop() {
            self.view_root = prev_root;
            self.tree_state.expanded = saved_expanded;
            self.tree_state.selected = saved_selected;
            self.tree_state.offset = 0;
            self.update_cached_tree_len();
        } else if self.view_root != self.path {
            if let Some(parent) = self.view_root.parent() {
                if parent.starts_with(&self.path) || parent == self.path {
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

    /// Get the current view root node.
    fn get_view_root_node(&self) -> Option<(&gravityfile_core::FileNode, PathBuf)> {
        let tree = self.tree.as_ref()?;
        let node = Self::find_node_at_path(&tree.root, &self.view_root, &tree.root_path)?;
        Some((node, self.view_root.clone()))
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
                self.marked_for_deletion.clear();
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
                self.marked_for_deletion.clear();
            }
            CommandAction::ToggleDetails => {
                self.show_details = !self.show_details;
            }
            CommandAction::SetTheme(theme_cmd) => match theme_cmd {
                ThemeCommand::Dark => self.theme = Theme::dark(),
                ThemeCommand::Light => self.theme = Theme::light(),
                ThemeCommand::Toggle => self.theme = self.theme.toggle(),
            },
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
        self.marked_for_deletion.contains(path)
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
            scan_progress: self.scan_progress.as_ref(),
            deletion_progress: self.deletion_progress.as_ref(),
            duplicates: self.duplicates.as_ref(),
            age_report: self.age_report.as_ref(),
            warnings: &self.warnings,
            selected_dup_group: self.selected_dup_group,
            selected_stale_dir: self.selected_stale_dir,
            selected_warning: self.selected_warning,
            marked_for_deletion: &self.marked_for_deletion,
            deletion_message: self.deletion_message.as_ref(),
            error: self.error.as_deref(),
            command_input: self.command_input.buffer(),
            command_cursor: self.command_input.cursor(),
            get_path_size: Box::new(|p| self.get_path_size(p)),
            get_selected_info: self.get_selected_info(),
            get_view_root_node: self.get_view_root_node(),
            get_filtered_duplicates: self.get_filtered_duplicates(),
            get_filtered_stale_dirs: self.get_filtered_stale_dirs(),
        };

        render_app(&ctx, area, buf);
    }
}
