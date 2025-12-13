//! Undo log for file operations.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

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
}

impl UndoEntry {
    /// Create a new undo entry.
    pub fn new(id: u64, operation: UndoableOperation, description: impl Into<String>) -> Self {
        Self {
            id,
            timestamp: SystemTime::now(),
            operation,
            description: description.into(),
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
    /// Files were moved to trash.
    FilesDeleted {
        /// List of (original_path, trash_path) pairs.
        /// Only populated if trash was used.
        trash_entries: Vec<(PathBuf, PathBuf)>,
    },
    /// A file or directory was renamed.
    FileRenamed {
        /// The path (with new name).
        path: PathBuf,
        /// The old name.
        old_name: String,
        /// The new name.
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
    /// Get a description of how to undo this operation.
    pub fn undo_description(&self) -> String {
        match self {
            Self::FilesMoved { moves } => {
                format!("Move {} items back to original location", moves.len())
            }
            Self::FilesCopied { created } => {
                format!("Delete {} copied items", created.len())
            }
            Self::FilesDeleted { trash_entries } => {
                if trash_entries.is_empty() {
                    "Cannot undo permanent deletion".to_string()
                } else {
                    format!("Restore {} items from trash", trash_entries.len())
                }
            }
            Self::FileRenamed { old_name, .. } => {
                format!("Rename back to '{}'", old_name)
            }
            Self::FileCreated { .. } => "Delete the created file".to_string(),
            Self::DirectoryCreated { .. } => "Delete the created directory".to_string(),
        }
    }

    /// Check if this operation can be undone.
    pub fn can_undo(&self) -> bool {
        match self {
            Self::FilesDeleted { trash_entries } => !trash_entries.is_empty(),
            _ => true,
        }
    }
}

/// Undo log with configurable maximum depth.
#[derive(Debug)]
pub struct UndoLog {
    entries: VecDeque<UndoEntry>,
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

        // Remove oldest entry if at capacity
        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }

        self.entries
            .push_back(UndoEntry::new(id, operation, description));

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
    pub fn record_delete(&mut self, trash_entries: Vec<(PathBuf, PathBuf)>) -> u64 {
        let count = trash_entries.len();
        let desc = if trash_entries.is_empty() {
            format!("Permanently deleted {} items", count)
        } else {
            format!("Moved {} items to trash", count)
        };
        self.record(UndoableOperation::FilesDeleted { trash_entries }, desc)
    }

    /// Record a rename operation.
    pub fn record_rename(&mut self, path: PathBuf, old_name: String, new_name: String) -> u64 {
        self.record(
            UndoableOperation::FileRenamed {
                path,
                old_name: old_name.clone(),
                new_name: new_name.clone(),
            },
            format!("Renamed '{}' to '{}'", old_name, new_name),
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
    /// Returns None if the log is empty or the most recent operation cannot be undone.
    pub fn pop(&mut self) -> Option<UndoEntry> {
        // Find the most recent undoable entry
        while let Some(entry) = self.entries.pop_back() {
            if entry.operation.can_undo() {
                return Some(entry);
            }
        }
        None
    }

    /// Peek at the most recent entry without removing it.
    pub fn peek(&self) -> Option<&UndoEntry> {
        self.entries.back()
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

    /// Get an iterator over all entries (oldest first).
    pub fn iter(&self) -> impl Iterator<Item = &UndoEntry> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        log.record_rename(
            PathBuf::from("/test/new.txt"),
            "old.txt".to_string(),
            "new.txt".to_string(),
        );

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
}
