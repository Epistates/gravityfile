//! Application state types and enums.

use std::path::PathBuf;

use strum::{Display, EnumIter, FromRepr, IntoEnumIterator};

use gravityfile_analyze::{AgeReport, DuplicateReport};
use gravityfile_core::FileTree;
use gravityfile_ops::{Conflict, OperationProgress, OperationType};
use gravityfile_scan::ScanProgress;

/// Application mode representing the current UI state.
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
    /// Waiting for conflict resolution.
    ConflictResolution,
    /// Command palette input mode (vim-style :command).
    Command,
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

/// Active view/tab during scanning mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanView {
    #[default]
    Progress,
    Errors,
}

impl ScanView {
    /// Move to next view (cyclic).
    pub fn next(self) -> Self {
        match self {
            ScanView::Progress => ScanView::Errors,
            ScanView::Errors => ScanView::Progress,
        }
    }

    /// Move to previous view (cyclic).
    pub fn prev(self) -> Self {
        self.next() // Only 2 options, so same as next
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
