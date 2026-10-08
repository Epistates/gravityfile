//! Undo log for file operations.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Reported completion evidence for an incomplete undo attempt. Counts do not
/// identify which individual paths succeeded, and errors may describe partial work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoAttempt {
    /// Time the caller received completion, not a filesystem commit timestamp.
    pub observed_at: SystemTime,
    /// Whether cancellation was reported.
    pub cancelled: bool,
    /// Reported successful item count.
    pub succeeded: usize,
    /// Reported failed item count.
    pub failed: usize,
    /// Reported skipped item count.
    pub skipped: usize,
    /// Reported bytes processed.
    pub bytes_processed: u64,
    /// Original path-specific diagnostics.
    pub errors: Vec<crate::OperationError>,
}

/// An entry in the undo log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    /// Unique ID for this entry.
    pub id: u64,
    /// When the operation was performed.
    pub timestamp: SystemTime,
    /// The operation that was performed.
    pub operation: UndoableOperation,
    /// Human-readable description.
    pub description: String,
    /// An incomplete undo needs manual review and must not be replayed.
    #[serde(default)]
    pub review_required: bool,
    /// Completion evidence, or None if no completion was observed.
    #[serde(default)]
    pub last_attempt: Option<UndoAttempt>,
}

impl UndoEntry {
    /// Create a new undo entry.
    pub fn new(id: u64, operation: UndoableOperation, description: impl Into<String>) -> Self {
        Self {
            id,
            timestamp: SystemTime::now(),
            operation,
            description: description.into(),
            review_required: false,
            last_attempt: None,
        }
    }
}

/// An operation that can be undone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UndoableOperation {
    /// Files were moved from one location to another.
    FilesMoved {
        /// List of (original_path, new_path) pairs.
        moves: Vec<(PathBuf, PathBuf)>,
    },
    /// Files were copied to a destination.
    FilesCopied {
        /// List of created files/directories.
        created: Vec<PathBuf>,
    },
    /// Files were moved to trash or permanently deleted.
    FilesDeleted {
        /// List of paths that were deleted.
        paths: Vec<PathBuf>,
    },
    /// A file or directory was renamed.
    FileRenamed {
        /// The full path with the new name (after rename).
        path: PathBuf,
        /// The full original path (before rename) — used to undo.
        // HIGH-5: store full old path instead of bare old_name string
        old_path: PathBuf,
        /// The new name (informational).
        new_name: String,
    },
    /// A file was created.
    FileCreated {
        /// Path to the created file.
        path: PathBuf,
    },
    /// A directory was created.
    DirectoryCreated {
        /// Path to the created directory.
        path: PathBuf,
    },
}

impl UndoableOperation {
    /// Number of original top-level items represented by this record.
    pub fn item_count(&self) -> usize {
        match self {
            Self::FilesMoved { moves } => moves.len(),
            Self::FilesCopied { created } => created.len(),
            Self::FilesDeleted { paths } => paths.len(),
            Self::FileRenamed { .. } | Self::FileCreated { .. } | Self::DirectoryCreated { .. } => {
                1
            }
        }
    }

    /// Get a description of how to undo this operation.
    pub fn undo_description(&self) -> String {
        match self {
            Self::FilesMoved { moves } => {
                format!("Move {} items back to original location", moves.len())
            }
            Self::FilesCopied { created } => {
                format!("Retain {} copied items in recovery journals", created.len())
            }
            Self::FilesDeleted { paths } => {
                format!("Deleted {} items (recover via OS trash)", paths.len())
            }
            Self::FileRenamed { old_path, .. } => {
                format!(
                    "Rename back to '{}'",
                    old_path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )
            }
            Self::FileCreated { .. } => "Delete the created file if still empty".to_string(),
            Self::DirectoryCreated { .. } => {
                "Delete the created directory if still empty".to_string()
            }
        }
    }

    /// Check if this operation can be undone.
    pub fn can_undo(&self) -> bool {
        !matches!(self, Self::FilesDeleted { .. })
    }
}

/// Undo log with configurable maximum depth.
#[derive(Debug)]
pub struct UndoLog {
    entries: VecDeque<Arc<UndoEntry>>,
    max_entries: usize,
    next_id: u64,
}

impl Default for UndoLog {
    fn default() -> Self {
        Self::new(100)
    }
}

impl UndoLog {
    /// Create a new undo log with the specified maximum entries.
    /// A zero limit disables retention. The limit counts operations, not paths
    /// within an operation or their memory usage.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(max_entries.min(1000)),
            max_entries,
            next_id: 0,
        }
    }

    /// Record an operation in the undo log.
    ///
    /// Returns the ID assigned to this entry.
    pub fn record(&mut self, operation: UndoableOperation, description: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        if self.max_entries == 0 {
            return id;
        }

        // Remove oldest entry if at capacity
        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }

        self.entries
            .push_back(Arc::new(UndoEntry::new(id, operation, description)));

        id
    }

    /// Record a move operation.
    pub fn record_move(&mut self, moves: Vec<(PathBuf, PathBuf)>) -> u64 {
        let count = moves.len();
        self.record(
            UndoableOperation::FilesMoved { moves },
            format!("Moved {} items", count),
        )
    }

    /// Record a copy operation.
    pub fn record_copy(&mut self, created: Vec<PathBuf>) -> u64 {
        let count = created.len();
        self.record(
            UndoableOperation::FilesCopied { created },
            format!("Copied {} items", count),
        )
    }

    /// Record a delete operation.
    pub fn record_delete(&mut self, paths: Vec<PathBuf>) -> u64 {
        let count = paths.len();
        self.record(
            UndoableOperation::FilesDeleted { paths },
            format!("Deleted {} items", count),
        )
    }

    /// Record a rename operation.
    ///
    /// `source` is the full original path before the rename.
    /// `new_name` is the new name component (used for the description).
    pub fn record_rename(&mut self, source: PathBuf, new_name: String) -> u64 {
        let parent = source.parent().unwrap_or(std::path::Path::new(""));
        let new_path = parent.join(&new_name);
        let old_name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let description = format!("Renamed '{}' to '{}'", old_name, new_name);
        self.record(
            UndoableOperation::FileRenamed {
                path: new_path,
                old_path: source,
                new_name,
            },
            description,
        )
    }

    /// Record a file creation.
    pub fn record_create_file(&mut self, path: PathBuf) -> u64 {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.record(
            UndoableOperation::FileCreated { path },
            format!("Created file '{}'", name),
        )
    }

    /// Record a directory creation.
    pub fn record_create_directory(&mut self, path: PathBuf) -> u64 {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.record(
            UndoableOperation::DirectoryCreated { path },
            format!("Created directory '{}'", name),
        )
    }

    /// Pop the most recent undoable entry.
    ///
    /// Returns `None` if empty, review is required, or the back entry cannot be undone.
    /// Does NOT drain through non-undoable entries — only examines the back.
    pub fn pop(&mut self) -> Option<UndoEntry> {
        // HIGH-4: only look at the back entry; don't consume non-undoable ones
        if !self.entries.back()?.review_required && self.entries.back()?.operation.can_undo() {
            self.entries.pop_back().map(Arc::unwrap_or_clone)
        } else {
            None
        }
    }

    /// Retain the original record after an incomplete undo, without permitting
    /// a blind replay of items that may already have completed. Retention uses
    /// the same operation-count limit as ordinary history; it is not durable.
    pub fn retain_for_review(&mut self, mut entry: UndoEntry) {
        entry.review_required = true;
        if self.max_entries == 0 {
            return;
        }
        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(Arc::new(entry));
    }

    /// Peek at the most recent entry without removing it.
    pub fn peek(&self) -> Option<&UndoEntry> {
        self.entries.back().map(AsRef::as_ref)
    }

    /// Get the number of entries in the log.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all entries from the log.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Share immutable incomplete records without cloning path vectors. The
    /// snapshot remains valid if the live history changes.
    pub fn review_snapshot(&self) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .filter(|entry| entry.review_required)
                .cloned()
                .collect(),
            max_entries: self.max_entries,
            next_id: self.next_id,
        }
    }

    /// Stream a versioned review-only JSON document for incomplete attempts.
    /// Native path bytes/code units are authoritative; display strings may be
    /// lossy. This is not an import/replay format. Errors can leave partial output;
    /// callers publishing files must stage and atomically publish them separately.
    pub fn write_review_json(&self, writer: impl std::io::Write) -> Result<(), serde_json::Error> {
        crate::undo_review::write(self, writer)
    }

    /// Export review-only JSON through private same-directory staging, syncing
    /// file contents before atomic no-replace publication. Existing destinations
    /// are preserved. Parent-directory durability and concurrent ancestor
    /// replacement protection are not provided. This synchronous API should run
    /// on a worker when called from an interactive application.
    pub fn export_review_json(&self, destination: &std::path::Path) -> std::io::Result<()> {
        crate::undo_review::export(self, destination)
    }

    /// Get an iterator over all entries (oldest first).
    pub fn iter(&self) -> impl Iterator<Item = &UndoEntry> {
        self.entries.iter().map(AsRef::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_snapshot_shares_records_and_survives_live_history_changes() {
        let mut log = UndoLog::new(2);
        log.record_create_file("ordinary".into());
        log.retain_for_review(UndoEntry::new(
            9,
            UndoableOperation::FilesCopied {
                created: vec!["retained".into()],
            },
            "review",
        ));
        let snapshot = log.review_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert!(std::ptr::eq(log.peek().unwrap(), snapshot.peek().unwrap()));
        log.clear();
        log.record_create_file("new".into());
        assert_eq!(snapshot.peek().unwrap().id, 9);
        assert!(snapshot.peek().unwrap().review_required);
        assert!(!log.peek().unwrap().review_required);
    }

    #[test]
    fn retention_limit_holds_across_record_pop_and_clear() {
        for limit in 0..=3 {
            let mut log = UndoLog::new(limit);
            for id in 0..10 {
                assert_eq!(
                    log.record_create_file(PathBuf::from(format!("file-{id}"))),
                    id
                );
                assert_eq!(log.len(), limit.min(id as usize + 1));
                assert!(log.len() <= limit);
            }
            let retained: Vec<_> = log.iter().map(|entry| entry.id).collect();
            assert_eq!(retained, ((10 - limit as u64)..10).collect::<Vec<_>>());
            for expected in retained.into_iter().rev() {
                assert_eq!(log.pop().unwrap().id, expected);
            }
            assert!(log.pop().is_none());
            assert!(log.peek().is_none());
            log.record_create_directory(PathBuf::from("directory"));
            log.clear();
            assert!(log.is_empty());
            assert_eq!(log.record_delete(vec![PathBuf::from("deleted")]), 11);
            assert_eq!(log.len(), limit.min(1));
            assert!(log.pop().is_none());
        }
    }

    #[test]
    fn test_undo_log_record() {
        let mut log = UndoLog::new(10);

        let id = log.record_create_file(PathBuf::from("/test/file.txt"));
        assert_eq!(id, 0);
        assert_eq!(log.len(), 1);

        let id = log.record_create_directory(PathBuf::from("/test/dir"));
        assert_eq!(id, 1);
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn test_undo_log_max_entries() {
        let mut log = UndoLog::new(3);

        log.record_create_file(PathBuf::from("/test/1.txt"));
        log.record_create_file(PathBuf::from("/test/2.txt"));
        log.record_create_file(PathBuf::from("/test/3.txt"));
        assert_eq!(log.len(), 3);

        log.record_create_file(PathBuf::from("/test/4.txt"));
        assert_eq!(log.len(), 3);

        // First entry should be removed
        let entry = log.pop().unwrap();
        assert!(entry.description.contains("4.txt"));
    }

    #[test]
    fn test_undo_log_pop() {
        let mut log = UndoLog::new(10);

        log.record_create_file(PathBuf::from("/test/file.txt"));
        log.record_rename(PathBuf::from("/test/old.txt"), "new.txt".to_string());

        let entry = log.pop().unwrap();
        assert!(matches!(
            entry.operation,
            UndoableOperation::FileRenamed { .. }
        ));

        let entry = log.pop().unwrap();
        assert!(matches!(
            entry.operation,
            UndoableOperation::FileCreated { .. }
        ));

        assert!(log.pop().is_none());
    }

    #[test]
    fn test_pop_does_not_drain_non_undoable() {
        let mut log = UndoLog::new(10);
        log.record_create_file(PathBuf::from("/test/file.txt"));
        // Delete is non-undoable; push it last so it is the back entry
        log.record_delete(vec![PathBuf::from("/test/other.txt")]);

        // pop() should return None because the back entry is non-undoable
        assert!(log.pop().is_none());
        // The undoable create entry must still be present
        assert_eq!(log.len(), 2);
    }
}
