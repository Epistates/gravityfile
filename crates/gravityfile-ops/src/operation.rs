//! File operation types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A file operation to be executed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileOperation {
    /// Copy files/directories to a destination.
    Copy {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    /// Move files/directories to a destination.
    Move {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    /// Rename a single file or directory.
    Rename { source: PathBuf, new_name: String },
    /// Delete files/directories.
    Delete {
        targets: Vec<PathBuf>,
        use_trash: bool,
    },
    /// Create a new empty file.
    CreateFile { path: PathBuf },
    /// Create a new directory.
    CreateDirectory { path: PathBuf },
}

impl FileOperation {
    /// Create a copy operation.
    pub fn copy(sources: Vec<PathBuf>, destination: PathBuf) -> Self {
        Self::Copy {
            sources,
            destination,
        }
    }

    /// Create a move operation.
    pub fn move_to(sources: Vec<PathBuf>, destination: PathBuf) -> Self {
        Self::Move {
            sources,
            destination,
        }
    }

    /// Create a rename operation.
    pub fn rename(source: PathBuf, new_name: impl Into<String>) -> Self {
        Self::Rename {
            source,
            new_name: new_name.into(),
        }
    }

    /// Create a delete operation.
    pub fn delete(targets: Vec<PathBuf>, use_trash: bool) -> Self {
        Self::Delete { targets, use_trash }
    }

    /// Create a file creation operation.
    pub fn create_file(path: PathBuf) -> Self {
        Self::CreateFile { path }
    }

    /// Create a directory creation operation.
    pub fn create_directory(path: PathBuf) -> Self {
        Self::CreateDirectory { path }
    }
}

/// An error that occurred during a file operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationError {
    /// The path that caused the error.
    pub path: PathBuf,
    /// A human-readable error message.
    pub message: String,
    /// Number of diagnostics represented by an omission record; zero for a
    /// normal path-specific diagnostic. Omission records have no associated path.
    #[serde(default)]
    pub omitted_count: usize,
}

impl OperationError {
    /// Number of reported diagnostics represented by this record.
    pub fn represented_count(&self) -> usize {
        self.omitted_count.max(1)
    }

    pub(crate) fn omitted(count: usize) -> Self {
        Self {
            path: PathBuf::new(),
            message: format!(
                "{count} additional operation diagnostics omitted from retained detail"
            ),
            omitted_count: count,
        }
    }

    /// Create a new operation error.
    pub fn new(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
            omitted_count: 0,
        }
    }
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}
