//! Application state types and enums.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, FromRepr, IntoEnumIterator};

use gravityfile_analyze::{AgeReport, DuplicateReport};
use gravityfile_core::FileTree;
use gravityfile_ops::{Conflict, OperationProgress, OperationType};
use gravityfile_scan::ScanProgress;

/// Application mode representing the current UI state.
/// Note: Scanning is NOT a mode - scanning happens in the background
/// while the user can still interact with the UI in Normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppMode {
    #[default]
    Normal,
    Help,
    /// Fuzzy search mode for quick navigation.
    Search,
    /// Confirming deletion of marked items.
    ConfirmDelete,
    /// Deletion in progress.
    Deleting,
    /// Copy operation in progress.
    Copying,
    /// Move operation in progress.
    Moving,
    /// Renaming a file or directory (text input mode).
    Renaming,
    /// Creating a new file (text input mode).
    CreatingFile,
    /// Creating a new directory (text input mode).
    CreatingDirectory,
    /// Taking (create directory and cd into it) - text input mode.
    Taking,
    /// Going to a directory (path input mode).
    GoingTo,
    /// Waiting for conflict resolution.
    ConflictResolution,
    /// Command palette input mode (vim-style :command).
    Command,
    /// Settings modal.
    Settings,
    Quit,
}

/// Layout mode for the explorer view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutMode {
    /// Tree view (default, current behavior).
    #[default]
    Tree,
    /// Miller columns (ranger-style three-pane).
    Miller,
}

impl LayoutMode {
    /// Toggle between layout modes.
    pub fn toggle(self) -> Self {
        match self {
            Self::Tree => Self::Miller,
            Self::Miller => Self::Tree,
        }
    }
}

/// Clipboard mode determines paste behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardMode {
    /// Clipboard is empty.
    #[default]
    Empty,
    /// Items were yanked (copy).
    Copy,
    /// Items were cut (move).
    Cut,
}

/// Clipboard state for file operations.
#[derive(Debug, Clone, Default)]
pub struct ClipboardState {
    /// Paths currently in the clipboard.
    pub paths: Vec<PathBuf>,
    /// The clipboard mode (copy or cut).
    pub mode: ClipboardMode,
    /// The root directory where items were copied/cut from.
    pub source_root: Option<PathBuf>,
}

impl ClipboardState {
    /// Yank (copy) paths to the clipboard.
    pub fn yank(&mut self, paths: impl IntoIterator<Item = PathBuf>, source_root: PathBuf) {
        self.paths = paths.into_iter().collect();
        self.mode = ClipboardMode::Copy;
        self.source_root = Some(source_root);
    }

    /// Cut (move) paths to the clipboard.
    pub fn cut(&mut self, paths: impl IntoIterator<Item = PathBuf>, source_root: PathBuf) {
        self.paths = paths.into_iter().collect();
        self.mode = ClipboardMode::Cut;
        self.source_root = Some(source_root);
    }

    /// Clear the clipboard.
    pub fn clear(&mut self) {
        self.paths.clear();
        self.mode = ClipboardMode::Empty;
        self.source_root = None;
    }

    /// Check if the clipboard is empty.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Get the number of items in the clipboard.
    pub fn len(&self) -> usize {
        self.paths.len()
    }
}

/// Sort mode for file listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, EnumIter, FromRepr)]
pub enum SortMode {
    /// Size descending (largest first) - default for disk usage analyzer.
    #[default]
    #[strum(to_string = "Size ↓")]
    SizeDescending,
    /// Size ascending (smallest first).
    #[strum(to_string = "Size ↑")]
    SizeAscending,
    /// Name ascending (A-Z).
    #[strum(to_string = "Name ↓")]
    NameAscending,
    /// Name descending (Z-A).
    #[strum(to_string = "Name ↑")]
    NameDescending,
    /// Modified date descending (newest first).
    #[strum(to_string = "Date ↓")]
    ModifiedDescending,
    /// Modified date ascending (oldest first).
    #[strum(to_string = "Date ↑")]
    ModifiedAscending,
    /// Child count descending (most children first).
    #[strum(to_string = "Count ↓")]
    CountDescending,
    /// Child count ascending (fewest children first).
    #[strum(to_string = "Count ↑")]
    CountAscending,
}

impl SortMode {
    /// Cycle to the next sort mode.
    pub fn next(self) -> Self {
        let current = self as usize;
        let next = (current + 1) % Self::iter().count();
        Self::from_repr(next).unwrap_or_default()
    }

    /// Reverse the current sort direction.
    pub fn reverse(self) -> Self {
        match self {
            Self::SizeDescending => Self::SizeAscending,
            Self::SizeAscending => Self::SizeDescending,
            Self::NameAscending => Self::NameDescending,
            Self::NameDescending => Self::NameAscending,
            Self::ModifiedDescending => Self::ModifiedAscending,
            Self::ModifiedAscending => Self::ModifiedDescending,
            Self::CountDescending => Self::CountAscending,
            Self::CountAscending => Self::CountDescending,
        }
    }

    /// Get a short label for display in the status bar.
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::SizeDescending => "SZ↓",
            Self::SizeAscending => "SZ↑",
            Self::NameAscending => "NM↓",
            Self::NameDescending => "NM↑",
            Self::ModifiedDescending => "DT↓",
            Self::ModifiedAscending => "DT↑",
            Self::CountDescending => "CT↓",
            Self::CountAscending => "CT↑",
        }
    }
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

impl View {
    /// Move to next view (cyclic).
    pub fn next(self) -> Self {
        let current = self as usize;
        let next = (current + 1) % Self::iter().count();
        Self::from_repr(next).unwrap_or_default()
    }

    /// Move to previous view (cyclic).
    pub fn prev(self) -> Self {
        let current = self as usize;
        let count = Self::iter().count();
        let prev = (current + count - 1) % count;
        Self::from_repr(prev).unwrap_or_default()
    }
}


/// Progress during deletion operation.
#[derive(Debug, Clone)]
pub struct DeletionProgress {
    /// Total items to delete.
    pub total: usize,
    /// Items deleted so far.
    pub deleted: usize,
    /// Items that failed to delete.
    pub failed: usize,
    /// Bytes freed so far.
    pub bytes_freed: u64,
    /// Current item being deleted.
    pub current: Option<PathBuf>,
}

impl DeletionProgress {
    /// Create new deletion progress.
    pub fn new(total: usize) -> Self {
        Self {
            total,
            deleted: 0,
            failed: 0,
            bytes_freed: 0,
            current: None,
        }
    }

    /// Get completion percentage.
    pub fn percentage(&self) -> u16 {
        if self.total > 0 {
            ((self.deleted + self.failed) as f64 / self.total as f64 * 100.0) as u16
        } else {
            0
        }
    }
}

/// Result from a background scan operation.
pub enum ScanResult {
    Progress(ScanProgress),
    #[allow(dead_code)] // For future real-time warning streaming
    Warning(gravityfile_core::ScanWarning),
    /// Partial tree snapshot for real-time updates during scanning.
    PartialTree(FileTree),
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
    /// Progress update during file operations (copy/move/etc).
    OperationProgress(OperationProgress),
    /// A conflict was encountered during file operation.
    OperationConflict(Conflict),
    /// File operation completed.
    OperationComplete {
        operation_type: OperationType,
        succeeded: usize,
        failed: usize,
        bytes_processed: u64,
    },
}

/// Information about the currently selected item.
#[derive(Debug, Clone)]
pub struct SelectedInfo {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub modified: std::time::SystemTime,
    pub is_dir: bool,
}

/// A pending file operation waiting for conflict resolution.
#[derive(Debug, Clone)]
pub enum PendingOperation {
    /// Paste operation (copy or move).
    Paste {
        sources: Vec<PathBuf>,
        destination: PathBuf,
        mode: ClipboardMode,
    },
}

/// State for the duplicates view with expandable groups.
#[derive(Debug, Clone, Default)]
pub struct DuplicatesViewState {
    /// Index of the selected group.
    pub selected_group: usize,
    /// Set of expanded group indices.
    pub expanded_groups: std::collections::HashSet<usize>,
    /// When a group is expanded, the selected file index within that group (0 = group header).
    /// Key is group index, value is selected item (0 = header, 1..=n = files).
    pub selected_in_group: std::collections::HashMap<usize, usize>,
    /// Scroll offset for the view.
    pub scroll_offset: usize,
}

impl DuplicatesViewState {
    /// Create a new duplicates view state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle expansion of the selected group.
    pub fn toggle_expand(&mut self) {
        if self.expanded_groups.contains(&self.selected_group) {
            self.expanded_groups.remove(&self.selected_group);
            self.selected_in_group.remove(&self.selected_group);
        } else {
            self.expanded_groups.insert(self.selected_group);
            self.selected_in_group.insert(self.selected_group, 0);
        }
    }

    /// Check if a group is expanded.
    pub fn is_expanded(&self, group_index: usize) -> bool {
        self.expanded_groups.contains(&group_index)
    }

    /// Get the selected item within a group (0 = header, 1+ = file index).
    pub fn selected_item(&self, group_index: usize) -> usize {
        self.selected_in_group.get(&group_index).copied().unwrap_or(0)
    }

    /// Move selection up.
    pub fn move_up(&mut self, _group_count: usize, get_file_count: impl Fn(usize) -> usize) {
        if self.is_expanded(self.selected_group) {
            let selected = self.selected_item(self.selected_group);
            if selected > 0 {
                // Move within group
                self.selected_in_group.insert(self.selected_group, selected - 1);
                return;
            }
        }
        // Move to previous group
        if self.selected_group > 0 {
            self.selected_group -= 1;
            // If previous group is expanded, select its last item
            if self.is_expanded(self.selected_group) {
                let file_count = get_file_count(self.selected_group);
                self.selected_in_group.insert(self.selected_group, file_count);
            }
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self, group_count: usize, get_file_count: impl Fn(usize) -> usize) {
        if self.is_expanded(self.selected_group) {
            let file_count = get_file_count(self.selected_group);
            let selected = self.selected_item(self.selected_group);
            if selected < file_count {
                // Move within group
                self.selected_in_group.insert(self.selected_group, selected + 1);
                return;
            }
        }
        // Move to next group
        if self.selected_group < group_count.saturating_sub(1) {
            self.selected_group += 1;
            // Start at header of next group
            if self.is_expanded(self.selected_group) {
                self.selected_in_group.insert(self.selected_group, 0);
            }
        }
    }

    /// Reset the state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Get the currently selected file path, if any.
    pub fn selected_file_path<'a>(
        &self,
        groups: &'a [&gravityfile_analyze::DuplicateGroup],
    ) -> Option<&'a PathBuf> {
        let group = groups.get(self.selected_group)?;
        let selected = self.selected_item(self.selected_group);
        if selected == 0 || !self.is_expanded(self.selected_group) {
            None // Header selected, not a specific file
        } else {
            group.paths.get(selected - 1)
        }
    }

    /// Get all file paths in the selected group (for bulk operations).
    pub fn selected_group_paths<'a>(
        &self,
        groups: &'a [&gravityfile_analyze::DuplicateGroup],
    ) -> Option<&'a [PathBuf]> {
        groups.get(self.selected_group).map(|g| g.paths.as_slice())
    }
}

/// A single tab representing an independent directory exploration context.
#[derive(Debug, Clone)]
pub struct Tab {
    /// Unique identifier for this tab.
    pub id: usize,
    /// Display label for the tab (typically the directory name).
    pub label: String,
    /// Root path being explored in this tab.
    pub path: PathBuf,
    /// Current view root (may be different from path after drilling down).
    pub view_root: PathBuf,
    /// Navigation history for back navigation.
    pub history: Vec<(PathBuf, usize, std::collections::HashSet<PathBuf>)>,
}

impl Tab {
    /// Create a new tab for a given path.
    pub fn new(id: usize, path: PathBuf) -> Self {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        Self {
            id,
            label,
            path: path.clone(),
            view_root: path,
            history: Vec::new(),
        }
    }

    /// Get a short label for display (truncated if too long).
    pub fn short_label(&self, max_len: usize) -> String {
        if self.label.len() <= max_len {
            self.label.clone()
        } else {
            format!("{}…", &self.label[..max_len.saturating_sub(1)])
        }
    }
}

/// Manager for multiple tabs.
#[derive(Debug)]
pub struct TabManager {
    /// All open tabs.
    tabs: Vec<Tab>,
    /// Index of the currently active tab.
    active: usize,
    /// Next tab ID to assign.
    next_id: usize,
    /// Maximum number of tabs allowed.
    max_tabs: usize,
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new(10)
    }
}

impl TabManager {
    /// Create a new tab manager with a maximum tab limit.
    pub fn new(max_tabs: usize) -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            max_tabs,
        }
    }

    /// Create a tab manager with an initial tab.
    pub fn with_initial_tab(path: PathBuf, max_tabs: usize) -> Self {
        let mut manager = Self::new(max_tabs);
        manager.new_tab(path);
        manager
    }

    /// Get the number of open tabs.
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Check if there are no tabs.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Get the active tab index.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Get a reference to the active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Get a mutable reference to the active tab.
    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// Get all tabs.
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Create a new tab with the given path.
    /// Returns true if the tab was created, false if at max capacity.
    pub fn new_tab(&mut self, path: PathBuf) -> bool {
        if self.tabs.len() >= self.max_tabs {
            return false;
        }

        let tab = Tab::new(self.next_id, path);
        self.next_id += 1;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        true
    }

    /// Close the active tab.
    /// Returns true if a tab was closed, false if it's the last tab.
    pub fn close_active_tab(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return false;
        }

        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        true
    }

    /// Close a tab by index.
    pub fn close_tab(&mut self, index: usize) -> bool {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return false;
        }

        self.tabs.remove(index);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if self.active > index {
            self.active -= 1;
        }
        true
    }

    /// Switch to the next tab.
    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    /// Switch to the previous tab.
    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                self.active - 1
            };
        }
    }

    /// Switch to a specific tab by index.
    pub fn switch_to(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    /// Switch to a tab by number (1-indexed, for Alt+1 through Alt+9).
    pub fn switch_to_number(&mut self, number: usize) {
        if number > 0 && number <= self.tabs.len() {
            self.active = number - 1;
        }
    }

    /// Duplicate the active tab (open same directory in new tab).
    pub fn duplicate_active_tab(&mut self) -> bool {
        if let Some(tab) = self.active_tab() {
            let path = tab.view_root.clone();
            self.new_tab(path)
        } else {
            false
        }
    }
}

/// Persistent user settings stored in config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    /// Whether to automatically start scanning on startup.
    pub scan_on_startup: bool,
    /// Show hidden files by default.
    pub show_hidden: bool,
    /// Default layout mode.
    pub default_layout: String,
    /// File opener configuration.
    pub openers: FileOpeners,
    /// Editor configuration for opensesame.
    #[serde(default)]
    pub editor: opensesame::EditorConfig,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            scan_on_startup: true,  // Enabled by default
            show_hidden: false,
            default_layout: "tree".to_string(),
            openers: FileOpeners::default(),
            editor: opensesame::EditorConfig::default(),
        }
    }
}

/// Configuration for file openers by extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FileOpeners {
    /// Opener for markdown files (.md).
    pub md: String,
    /// Opener for text/code files (uses $EDITOR via opensesame).
    /// Set to "editor" to use opensesame, or a specific command.
    pub text: String,
    /// Opener for other files (system default).
    pub default: String,
}

impl Default for FileOpeners {
    fn default() -> Self {
        Self {
            // Use treemd for markdown if available, otherwise system open
            md: "treemd".to_string(),
            // Use $EDITOR via opensesame for text files
            text: "editor".to_string(),
            // Use system open command for other files
            default: "open".to_string(),
        }
    }
}

impl UserSettings {
    /// Get the config file path.
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("gravityfile").join("settings.toml"))
    }

    /// Load settings from disk, or return defaults.
    pub fn load() -> Self {
        Self::config_path()
            .and_then(|path| std::fs::read_to_string(&path).ok())
            .and_then(|content| toml::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Save settings to disk.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "No config directory")
        })?;

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        std::fs::write(&path, content)
    }
}

/// State for the settings modal.
#[derive(Debug, Clone, Default)]
pub struct SettingsState {
    /// Currently selected setting index.
    pub selected: usize,
    /// Temporary copy of settings being edited.
    pub settings: UserSettings,
    /// Whether settings have been modified.
    pub dirty: bool,
}

impl SettingsState {
    /// Create a new settings state with current settings.
    pub fn new(settings: UserSettings) -> Self {
        Self {
            selected: 0,
            settings,
            dirty: false,
        }
    }

    /// Number of settings items.
    pub fn item_count(&self) -> usize {
        3 // scan_on_startup, show_hidden, default_layout
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        if self.selected < self.item_count() - 1 {
            self.selected += 1;
        }
    }

    /// Toggle the currently selected boolean setting.
    pub fn toggle_selected(&mut self) {
        match self.selected {
            0 => {
                self.settings.scan_on_startup = !self.settings.scan_on_startup;
                self.dirty = true;
            }
            1 => {
                self.settings.show_hidden = !self.settings.show_hidden;
                self.dirty = true;
            }
            2 => {
                // Cycle layout mode
                self.settings.default_layout = match self.settings.default_layout.as_str() {
                    "tree" => "miller".to_string(),
                    "miller" => "tree".to_string(),
                    _ => "tree".to_string(),
                };
                self.dirty = true;
            }
            _ => {}
        }
    }
}
