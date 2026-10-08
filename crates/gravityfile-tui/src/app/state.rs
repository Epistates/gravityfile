//! Application state types and enums.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, FromRepr, IntoEnumIterator};

use gravityfile_analyze::{AgeReport, DuplicateReport};
use gravityfile_core::FileTree;
use gravityfile_ops::{ConflictRequest, OperationProgress, OperationType};
use gravityfile_scan::ScanProgress;

use crate::ui::{MillerState, TreeState, TreemapState};

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
    /// Undo is running, including while cancellation is being completed.
    Undoing,
    /// A single-entry creation or rename is awaiting its worker result.
    UpdatingEntry,
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
    /// Setting a bookmark (waiting for key input).
    SettingBookmark,
    /// Jumping to a bookmark (waiting for key input).
    JumpingToBookmark,
    /// Viewing the bookmarks list modal.
    BookmarkList,
    /// Confirming bulk rename changes.
    ConfirmBulkRename,
    /// A bulk rename is running to completion or rollback.
    BulkRenaming,
    /// Visual selection mode (vim-style range selection).
    Visual,
    Quit,
}

/// Layout mode for the explorer view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

    /// The name used in settings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Miller => "miller",
        }
    }
}

/// State for visual selection mode (vim-style range selection).
#[derive(Debug, Clone, Default)]
pub struct VisualState {
    /// Anchor index where visual selection started.
    pub anchor: usize,
    /// Current cursor index (selection extends from anchor to cursor).
    pub cursor: usize,
}

impl VisualState {
    /// Create a new visual state starting at the given index.
    pub fn new(start_index: usize) -> Self {
        Self {
            anchor: start_index,
            cursor: start_index,
        }
    }

    /// Get the selection range as (start, end) inclusive.
    pub fn selection_range(&self) -> (usize, usize) {
        let start = self.anchor.min(self.cursor);
        let end = self.anchor.max(self.cursor);
        (start, end)
    }

    /// Move the cursor up.
    pub fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move the cursor down with a maximum bound.
    pub fn move_down(&mut self, max: usize) {
        if max > 0 && self.cursor < max - 1 {
            self.cursor += 1;
        }
    }

    /// Move the cursor up by a page.
    pub fn page_up(&mut self, page_size: usize) {
        self.cursor = self.cursor.saturating_sub(page_size);
    }

    /// Move the cursor down by a page with a maximum bound.
    pub fn page_down(&mut self, page_size: usize, max: usize) {
        if max > 0 {
            self.cursor = (self.cursor + page_size).min(max - 1);
        }
    }

    /// Jump cursor to the top.
    pub fn jump_to_top(&mut self) {
        self.cursor = 0;
    }

    /// Jump cursor to the bottom.
    pub fn jump_to_bottom(&mut self, max: usize) {
        if max > 0 {
            self.cursor = max - 1;
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
}

/// Sort mode for file listings.
///
/// Arrow convention: ↑ = ascending (A→Z, small→large), ↓ = descending (Z→A, large→small).
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
    #[strum(to_string = "Name ↑")]
    NameAscending,
    /// Name descending (Z-A).
    #[strum(to_string = "Name ↓")]
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
    /// ↑ = ascending (A→Z, small→large), ↓ = descending (Z→A, large→small).
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::SizeDescending => "SZ↓",
            Self::SizeAscending => "SZ↑",
            Self::NameAscending => "NM↑",
            Self::NameDescending => "NM↓",
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
    Treemap,
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
    #[cfg(test)]
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
    ScanComplete(Result<FileTree, gravityfile_scan::ScanError>),
    AnalysisComplete {
        duplicates: DuplicateReport,
        age_report: AgeReport,
        /// Per-group root count and validated path ordering, prepared off-thread.
        duplicate_scope: (PathBuf, Vec<(usize, bool)>),
    },
    AnalysisFailed(String),
    AnalysisCancelled,
    /// Progress update during deletion.
    DeletionProgress(DeletionProgress),
    /// Deletion completed.
    DeletionComplete {
        mode: gravityfile_ops::DeleteMode,
        /// Paths that were removed.
        deleted: Vec<PathBuf>,
        failed: usize,
        bytes_freed: u64,
        errors: Vec<gravityfile_ops::OperationError>,
    },
    /// Progress update during file operations (copy/move/etc).
    OperationProgress(OperationProgress),
    /// A conflict was encountered during file operation.
    OperationConflict(ConflictRequest),
    /// Create-and-enter completed: true means this task created the directory.
    TakeComplete {
        path: PathBuf,
        result: Result<bool, String>,
    },
    /// Bulk rename completed or failed after rollback attempts.
    BulkRenameComplete(Result<Vec<(std::path::PathBuf, std::path::PathBuf)>, String>),
    /// File operation completed.
    OperationComplete {
        cancelled: bool,
        operation_type: OperationType,
        succeeded: usize,
        failed: usize,
        skipped: usize,
        bytes_processed: u64,
        errors: Vec<gravityfile_ops::OperationError>,
        /// Information for the undo log.
        undo_info: Option<gravityfile_ops::UndoableOperation>,
        /// Items an undo could not reverse, to push back for a retry.
        undo_remaining: Option<gravityfile_ops::UndoEntry>,
        /// Paths whose contents may have changed, to refresh in the tree.
        touched: Vec<PathBuf>,
    },
}

/// The state of one changed path, scanned again.
pub struct Refreshed {
    pub path: PathBuf,
    /// `None` when the path no longer exists.
    pub entry: Result<Option<gravityfile_scan::ScannedEntry>, String>,
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
        self.selected_in_group
            .get(&group_index)
            .copied()
            .unwrap_or(0)
    }

    /// Move selection up.
    pub fn move_up(&mut self, _group_count: usize, get_file_count: impl Fn(usize) -> usize) {
        if self.is_expanded(self.selected_group) {
            let selected = self.selected_item(self.selected_group);
            if selected > 0 {
                // Move within group
                self.selected_in_group
                    .insert(self.selected_group, selected - 1);
                return;
            }
        }
        // Move to previous group
        if self.selected_group > 0 {
            self.selected_group -= 1;
            // If previous group is expanded, select its last item
            if self.is_expanded(self.selected_group) {
                let file_count = get_file_count(self.selected_group);
                self.selected_in_group
                    .insert(self.selected_group, file_count);
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
                self.selected_in_group
                    .insert(self.selected_group, selected + 1);
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
}

/// A place in a tab's back/forward history: the directory viewed, the
/// selected row and the expanded directories.
pub type Visit = (PathBuf, usize, HashSet<PathBuf>);

/// What one tab is looking at and how it got there. The active tab's
/// navigation is owned by the app; [`TabManager`] keeps the others.
#[derive(Debug, Clone, Default)]
pub struct Navigation {
    /// The directory being viewed.
    pub view_root: PathBuf,
    /// Selection, scroll and expanded directories in the tree layout.
    pub tree_state: TreeState,
    /// Selection and scroll in the Miller layout.
    pub miller_state: MillerState,
    /// Selection in the treemap.
    pub treemap_state: TreemapState,
    /// Earlier places, most recent last.
    pub back: Vec<Visit>,
    /// Places left by going back, most recent last.
    pub forward: Vec<Visit>,
}

impl Navigation {
    /// Start viewing `view_root` with it expanded and no history.
    pub fn new(view_root: PathBuf) -> Self {
        Self {
            tree_state: TreeState::new(view_root.clone()),
            view_root,
            ..Self::default()
        }
    }

    /// The tab label: the viewed directory's name.
    pub fn label(&self) -> String {
        self.view_root
            .file_name()
            .unwrap_or(self.view_root.as_os_str())
            .to_string_lossy()
            .into_owned()
    }
}

/// The open directory tabs. The active tab's [`Navigation`] is held by the
/// app and swapped back in here when another tab becomes active, so each tab
/// keeps its own place, selection and history.
#[derive(Debug)]
pub struct TabManager {
    /// One entry per tab; the active tab's entry is a placeholder.
    stashed: Vec<Navigation>,
    /// Index of the active tab.
    active: usize,
    /// Maximum number of tabs.
    max_tabs: usize,
}

impl TabManager {
    /// One tab, the active one, whose navigation the caller holds.
    pub fn new(max_tabs: usize) -> Self {
        Self {
            stashed: vec![Navigation::default()],
            active: 0,
            max_tabs,
        }
    }

    /// The number of open tabs.
    pub fn len(&self) -> usize {
        self.stashed.len()
    }

    /// The active tab's index.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Every tab's label, in order, taking the active one from `current`.
    pub fn labels(&self, current: &Navigation) -> Vec<String> {
        (0..self.stashed.len())
            .map(|index| {
                if index == self.active {
                    current.label()
                } else {
                    self.stashed[index].label()
                }
            })
            .collect()
    }

    /// Open a tab showing `navigation` and make it active. Returns false at
    /// the tab limit.
    pub fn open(&mut self, current: &mut Navigation, navigation: Navigation) -> bool {
        if self.stashed.len() >= self.max_tabs {
            return false;
        }
        self.stashed.push(navigation);
        self.switch(current, self.stashed.len() - 1)
    }

    /// Close the active tab and activate its neighbour. Returns false for the
    /// last tab.
    pub fn close_active(&mut self, current: &mut Navigation) -> bool {
        if self.stashed.len() <= 1 {
            return false;
        }
        self.stashed.remove(self.active);
        self.active = self.active.min(self.stashed.len() - 1);
        std::mem::swap(current, &mut self.stashed[self.active]);
        true
    }

    /// Make tab `index` active. Returns whether the active tab changed.
    pub fn switch(&mut self, current: &mut Navigation, index: usize) -> bool {
        if index == self.active || index >= self.stashed.len() {
            return false;
        }
        std::mem::swap(current, &mut self.stashed[self.active]);
        self.active = index;
        std::mem::swap(current, &mut self.stashed[self.active]);
        true
    }

    /// Activate the next tab, wrapping around.
    pub fn next(&mut self, current: &mut Navigation) -> bool {
        self.switch(current, (self.active + 1) % self.stashed.len())
    }

    /// Activate the previous tab, wrapping around.
    pub fn prev(&mut self, current: &mut Navigation) -> bool {
        let count = self.stashed.len();
        self.switch(current, (self.active + count - 1) % count)
    }
}

/// Directory bookmarks for quick navigation.
/// Bookmarks are single-character keys mapped to directory paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bookmarks {
    /// Map of bookmark keys to directory paths.
    pub marks: HashMap<char, PathBuf>,
}

impl Bookmarks {
    /// Create bookmarks with sensible defaults.
    pub fn with_defaults() -> Self {
        use gravityfile_core::paths;
        let mut marks = HashMap::new();
        marks.insert('r', PathBuf::from("/"));
        if let Some(config) = paths::config_root() {
            marks.insert('c', config);
        }
        if let Some(home) = paths::home() {
            for (key, name) in [('d', "Downloads"), ('o', "Documents")] {
                let directory = home.join(name);
                if directory.is_dir() {
                    marks.insert(key, directory);
                }
            }
            marks.insert('h', home);
        }
        Self { marks }
    }

    /// Set a bookmark.
    pub fn set(&mut self, key: char, path: PathBuf) {
        self.marks.insert(key, path);
    }

    /// Get a bookmark.
    pub fn get(&self, key: char) -> Option<&PathBuf> {
        self.marks.get(&key)
    }

    /// Remove a bookmark.
    pub fn remove(&mut self, key: char) -> Option<PathBuf> {
        self.marks.remove(&key)
    }

    /// Get all bookmarks sorted by key.
    pub fn sorted_entries(&self) -> Vec<(char, &PathBuf)> {
        let mut entries: Vec<_> = self.marks.iter().map(|(k, v)| (*k, v)).collect();
        entries.sort_by_key(|(k, _)| *k);
        entries
    }

    /// Check if a bookmark key is valid (alphanumeric).
    pub fn is_valid_key(key: char) -> bool {
        key.is_ascii_alphanumeric()
    }
}

/// State for the bookmark list modal.
#[derive(Debug, Clone, Default)]
pub struct BookmarkListState {
    /// Currently selected bookmark index.
    pub selected: usize,
    /// Cached sorted entries for display.
    entries_count: usize,
}

impl BookmarkListState {
    /// Create a new bookmark list state.
    pub fn new(bookmarks: &Bookmarks) -> Self {
        Self {
            selected: 0,
            entries_count: bookmarks.marks.len(),
        }
    }

    /// Update the entries count.
    pub fn update_count(&mut self, bookmarks: &Bookmarks) {
        self.entries_count = bookmarks.marks.len();
        if self.selected >= self.entries_count && self.entries_count > 0 {
            self.selected = self.entries_count - 1;
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        if self.entries_count > 0 && self.selected < self.entries_count - 1 {
            self.selected += 1;
        }
    }
}

/// A single rename operation in bulk rename.
#[derive(Debug, Clone)]
pub struct RenameEntry {
    /// Original file path.
    pub original: PathBuf,
    /// New name (just the file name, not the full path).
    pub new_name: String,
}

/// State for bulk rename confirmation modal.
#[derive(Debug, Clone, Default)]
pub struct BulkRenameState {
    /// List of rename operations to perform.
    pub entries: Vec<RenameEntry>,
    /// Currently selected entry in the confirmation modal.
    pub selected: usize,
    /// Error message if validation failed.
    pub error: Option<String>,
}

impl BulkRenameState {
    /// Create a new bulk rename state from original paths and new names.
    pub fn new(original_paths: Vec<PathBuf>, new_names: Vec<String>) -> Self {
        let entries: Vec<RenameEntry> = original_paths
            .into_iter()
            .zip(new_names)
            .filter_map(|(original, new_name)| {
                // Only include entries where the name actually changed
                let old_name = original.file_name()?.to_string_lossy().to_string();
                if old_name != new_name && !new_name.is_empty() {
                    Some(RenameEntry { original, new_name })
                } else {
                    None
                }
            })
            .collect();

        Self {
            entries,
            selected: 0,
            error: None,
        }
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        if !self.entries.is_empty() && self.selected < self.entries.len() - 1 {
            self.selected += 1;
        }
    }

    /// Validate all rename entries.
    pub fn validate(&mut self) -> bool {
        // Check for duplicate new names
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in &self.entries {
            if seen_names.contains(&entry.new_name) {
                self.error = Some(format!("Duplicate name: {}", entry.new_name));
                return false;
            }
            seen_names.insert(entry.new_name.clone());

            if let Err(error) = gravityfile_ops::validate_filename(&entry.new_name) {
                self.error = Some(format!("{}: {error}", entry.new_name));
                return false;
            }
        }
        self.error = None;
        true
    }
}

/// Persistent user settings stored in config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    /// Descendant-entry budget for quick listings and recursive scans.
    /// None preserves unlimited traversal; zero permits an empty scope.
    #[serde(default)]
    pub max_scan_entries: Option<u64>,
    /// Whether to automatically start scanning on startup.
    pub scan_on_startup: bool,
    /// Show hidden files by default.
    pub show_hidden: bool,
    /// Layout the explorer opens in.
    pub default_layout: LayoutMode,
    /// File opener configuration.
    pub openers: FileOpeners,
    /// Editor configuration for opensesame.
    #[serde(default)]
    pub editor: opensesame::EditorConfig,
    /// Directory bookmarks.
    #[serde(default = "Bookmarks::with_defaults")]
    pub bookmarks: Bookmarks,
    /// Render image files in the preview pane using the terminal's graphics
    /// protocol.
    ///
    /// Enabling this costs one capability query at startup. Terminals answer it
    /// in microseconds, but one that never answers delays the first frame by
    /// the query timeout, so this can be turned off.
    #[serde(default = "default_true")]
    pub image_previews: bool,
}

fn default_true() -> bool {
    true
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            max_scan_entries: None,
            scan_on_startup: true, // Enabled by default
            show_hidden: false,
            default_layout: LayoutMode::Tree,
            openers: FileOpeners::default(),
            editor: opensesame::EditorConfig::default(),
            bookmarks: Bookmarks::with_defaults(),
            image_previews: true,
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
        // Unit tests must never read or write the user's real settings.
        if cfg!(test) {
            return None;
        }
        gravityfile_core::paths::Dirs::discover().map(|dirs| dirs.settings_file())
    }

    const MAX_BYTES: u64 = 1024 * 1024;

    /// Load settings, using defaults when no settings file exists.
    /// Other read and parse failures are returned to the caller.
    pub fn try_load() -> std::io::Result<Self> {
        let Some(path) = Self::config_path() else {
            return Ok(Self::default());
        };
        Self::load_from(&path).map_err(|error| {
            std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })
    }

    fn load_from(path: &std::path::Path) -> std::io::Result<Self> {
        use std::io::{Error, ErrorKind, Read};
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Permit dotfile-manager symlinks, but never wait for a FIFO writer.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > Self::MAX_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "settings must be a regular UTF-8 file of at most 1 MiB",
            ));
        }
        let mut content = String::new();
        file.take(Self::MAX_BYTES + 1)
            .read_to_string(&mut content)?;
        if content.len() as u64 > Self::MAX_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "settings grew beyond 1 MiB",
            ));
        }
        toml::from_str(&content).map_err(|error| Error::new(ErrorKind::InvalidData, error))
    }

    /// Sync a private sibling file, atomically replace settings, and on Unix sync
    /// the containing directory. A post-replacement sync error explicitly reports
    /// that settings were installed but durability is uncertain.
    /// Existing file permissions are not preserved; Unix files are private (0600).
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "No config directory")
        })?;
        self.save_to(&path)
    }

    fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        self.save_to_with_directory_sync(path, sync_settings_directory)
    }

    fn save_to_with_directory_sync(
        &self,
        path: &std::path::Path,
        sync_directory: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        use std::io::Write;
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if content.len() as u64 > Self::MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "settings exceed 1 MiB",
            ));
        }
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".gravityfile-settings-")
            .tempfile_in(parent)?;
        staged.write_all(content.as_bytes())?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|error| error.error)?;
        sync_directory(parent).map_err(|error| {
            std::io::Error::new(error.kind(), format!(
                "Settings installed, but directory sync failed; durability is uncertain: {error}"
            ))
        })?;
        Ok(())
    }
}

fn sync_settings_directory(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK)
            .open(path)?
            .sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
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
                self.settings.default_layout = self.settings.default_layout.toggle();
                self.dirty = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn saved_entry_budget_roundtrips_without_changing_legacy_defaults() {
        use super::UserSettings;
        assert_eq!(
            toml::from_str::<UserSettings>("").unwrap().max_scan_entries,
            None
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        for limit in [0, 1, 100_000] {
            let settings = UserSettings {
                max_scan_entries: Some(limit),
                ..UserSettings::default()
            };
            settings.save_to(&path).unwrap();
            assert_eq!(
                UserSettings::load_from(&path).unwrap().max_scan_entries,
                Some(limit)
            );
        }
        assert!(toml::from_str::<UserSettings>("max_scan_entries = -1").is_err());
    }
    #[test]
    fn tab_labels_fit_terminal_columns_without_unicode_panics() {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;
        for label in [
            "",
            "documents",
            "éééééé",
            "日本語資料",
            "🗂️📁📂",
            "👩‍💻🇺🇸1️⃣",
            "e\u{301}e\u{301}",
        ] {
            for width in 0..=32 {
                for slice in [
                    crate::ui::truncate_to_width(label, width),
                    crate::ui::truncate_left_to_width(label, width),
                ] {
                    assert!(slice.width() <= width);
                    let boundaries: Vec<_> = label
                        .grapheme_indices(true)
                        .map(|(index, _)| index)
                        .chain(std::iter::once(label.len()))
                        .collect();
                    let start = slice.as_ptr() as usize - label.as_ptr() as usize;
                    assert!(boundaries.contains(&start));
                    assert!(boundaries.contains(&(start + slice.len())));
                }
                assert!(crate::ui::fit_left_to_width(label, width).width() <= width);
                let shortened = crate::ui::fit_to_width(label, width);
                assert!(shortened.width() <= width, "{label:?}, width {width}");
                if label.width() <= width {
                    assert_eq!(shortened, label);
                }
                if width == 0 {
                    assert!(shortened.is_empty());
                }
            }
        }
    }
    use super::*;

    #[test]
    fn settings_load_missing_defaults_and_existing_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        assert!(UserSettings::load_from(&path).unwrap().scan_on_startup);
        std::fs::write(&path, "scan_on_startup = false\nimage_previews = false\n").unwrap();
        let settings = UserSettings::load_from(&path).unwrap();
        assert!(!settings.scan_on_startup);
        assert!(!settings.image_previews);
    }

    #[test]
    fn settings_load_rejects_invalid_and_oversized_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        for content in [b"scan_on_startup = [".as_slice(), &[0xff]] {
            std::fs::write(&path, content).unwrap();
            assert_eq!(
                UserSettings::load_from(&path).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(UserSettings::MAX_BYTES + 1).unwrap();
        assert_eq!(
            UserSettings::load_from(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(UserSettings::load_from(directory.path()).is_err());
    }

    #[test]
    fn settings_oversized_save_preserves_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        std::fs::write(&path, b"original").unwrap();
        let settings = UserSettings {
            openers: FileOpeners {
                md: "x".repeat(UserSettings::MAX_BYTES as usize),
                ..FileOpeners::default()
            },
            ..UserSettings::default()
        };
        assert_eq!(
            settings.save_to(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn settings_load_accepts_regular_symlink_and_rejects_fifo() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let path = directory.path().join("settings.toml");
        std::fs::write(&target, "image_previews = false\n").unwrap();
        symlink(&target, &path).unwrap();
        assert!(!UserSettings::load_from(&path).unwrap().image_previews);
        let fifo = directory.path().join("fifo");
        let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `native` is a valid, NUL-terminated path for this call.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        assert_eq!(
            UserSettings::load_from(&fifo).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn settings_save_creates_parents_and_replaces_complete_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config/settings.toml");
        UserSettings::default().save_to(&path).unwrap();
        let settings = UserSettings {
            scan_on_startup: false,
            default_layout: LayoutMode::Miller,
            ..UserSettings::default()
        };
        settings.save_to(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("default_layout = \"miller\""), "{saved}");
        let restored: UserSettings = toml::from_str(&saved).unwrap();
        assert!(!restored.scan_on_startup);
        assert_eq!(restored.default_layout, LayoutMode::Miller);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn settings_publication_failure_preserves_destination_and_removes_staging() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("existing"), b"preserve me").unwrap();
        assert!(UserSettings::default().save_to(&path).is_err());
        assert_eq!(
            std::fs::read(path.join("existing")).unwrap(),
            b"preserve me"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn settings_directory_sync_failure_reports_installed_document_and_allows_retry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        UserSettings::default().save_to(&path).unwrap();
        let settings = UserSettings {
            scan_on_startup: false,
            ..UserSettings::default()
        };
        let error = settings
            .save_to_with_directory_sync(&path, |parent| {
                assert_eq!(parent, directory.path());
                assert!(!UserSettings::load_from(&path).unwrap().scan_on_startup);
                Err(std::io::Error::other("injected sync failure"))
            })
            .unwrap_err();
        assert!(error.to_string().contains("Settings installed"));
        assert!(error.to_string().contains("durability is uncertain"));
        assert!(!UserSettings::load_from(&path).unwrap().scan_on_startup);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        settings.save_to(&path).unwrap();
        assert!(!UserSettings::load_from(&path).unwrap().scan_on_startup);
    }

    #[cfg(unix)]
    #[test]
    fn settings_directory_sync_rejects_regular_files() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        std::fs::write(&file, b"keep").unwrap();
        assert!(sync_settings_directory(&file).is_err());
        sync_settings_directory(directory.path()).unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn settings_save_replaces_leaf_symlink_with_private_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let path = directory.path().join("settings.toml");
        std::fs::write(&target, b"untouched").unwrap();
        symlink(&target, &path).unwrap();
        UserSettings::default().save_to(&path).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn test_visual_state_creation() {
        let state = VisualState::new(5);
        assert_eq!(state.anchor, 5);
        assert_eq!(state.cursor, 5);
    }

    #[test]
    fn test_visual_state_selection_range() {
        let mut state = VisualState::new(5);
        assert_eq!(state.selection_range(), (5, 5));

        state.cursor = 8;
        assert_eq!(state.selection_range(), (5, 8));

        state.cursor = 2;
        assert_eq!(state.selection_range(), (2, 5));
    }

    #[test]
    fn test_visual_state_move_down_bounds() {
        let mut state = VisualState::new(0);

        // Test with max = 0 (empty list)
        state.move_down(0);
        assert_eq!(state.cursor, 0);

        // Test with max = 1 (single item)
        state.move_down(1);
        assert_eq!(state.cursor, 0);

        // Test normal movement
        state.move_down(5);
        assert_eq!(state.cursor, 1);

        // Test at boundary
        state.cursor = 4;
        state.move_down(5);
        assert_eq!(state.cursor, 4); // Should not exceed max-1
    }

    #[test]
    fn test_visual_state_move_up() {
        let mut state = VisualState::new(5);
        state.move_up();
        assert_eq!(state.cursor, 4);

        // Test at zero
        state.cursor = 0;
        state.move_up();
        assert_eq!(state.cursor, 0); // Should not go negative
    }

    #[test]
    fn test_visual_state_page_up() {
        let mut state = VisualState::new(10);
        state.page_up(5);
        assert_eq!(state.cursor, 5);

        state.page_up(10);
        assert_eq!(state.cursor, 0); // Saturating sub
    }

    #[test]
    fn test_visual_state_page_down_bounds() {
        let mut state = VisualState::new(0);

        // Test with max = 0
        state.page_down(5, 0);
        assert_eq!(state.cursor, 0);

        // Test with max = 10
        state.page_down(5, 10);
        assert_eq!(state.cursor, 5);

        state.page_down(10, 10);
        assert_eq!(state.cursor, 9); // Max - 1
    }

    #[test]
    fn test_visual_state_jump_to_top() {
        let mut state = VisualState::new(50);
        state.jump_to_top();
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn test_visual_state_jump_to_bottom_bounds() {
        let mut state = VisualState::new(0);

        // Test with max = 0
        state.jump_to_bottom(0);
        assert_eq!(state.cursor, 0);

        // Test with max = 10
        state.jump_to_bottom(10);
        assert_eq!(state.cursor, 9);
    }
}
