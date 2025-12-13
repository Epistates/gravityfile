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
}

impl OperationError {
    /// Create a new operation error.
    pub fn new(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}
