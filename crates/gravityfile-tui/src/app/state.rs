//! Application state types and enums.

use std::path::PathBuf;

use strum::{Display, EnumIter, FromRepr, IntoEnumIterator};

use gravityfile_analyze::{AgeReport, DuplicateReport};
use gravityfile_core::FileTree;
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
