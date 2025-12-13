//! Progress reporting types for file operations.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::OperationError;

/// The type of operation being performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    Copy,
    Move,
    Delete,
    Rename,
    CreateFile,
    CreateDirectory,
}

impl std::fmt::Display for OperationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Copy => write!(f, "Copy"),
            Self::Move => write!(f, "Move"),
            Self::Delete => write!(f, "Delete"),
            Self::Rename => write!(f, "Rename"),
            Self::CreateFile => write!(f, "Create file"),
            Self::CreateDirectory => write!(f, "Create directory"),
        }
    }
}

/// Progress information for an ongoing operation.
#[derive(Debug, Clone)]
pub struct OperationProgress {
    /// The type of operation.
    pub operation_type: OperationType,
    /// Number of files/directories completed.
    pub files_completed: usize,
    /// Total number of files/directories to process.
    pub files_total: usize,
    /// Number of bytes processed so far.
    pub bytes_processed: u64,
    /// Total bytes to process (may be 0 if unknown).
    pub bytes_total: u64,
    /// The file currently being processed.
    pub current_file: Option<PathBuf>,
    /// Errors encountered so far.
    pub errors: Vec<OperationError>,
}

impl OperationProgress {
    /// Create a new progress tracker for an operation.
    pub fn new(operation_type: OperationType, files_total: usize, bytes_total: u64) -> Self {
        Self {
            operation_type,
            files_completed: 0,
            files_total,
            bytes_processed: 0,
            bytes_total,
            current_file: None,
            errors: Vec::new(),
        }
    }

    /// Get the progress as a percentage (0.0 to 100.0).
    pub fn percentage(&self) -> f64 {
        if self.bytes_total > 0 {
            (self.bytes_processed as f64 / self.bytes_total as f64) * 100.0
        } else if self.files_total > 0 {
            (self.files_completed as f64 / self.files_total as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Check if the operation has any errors.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Get the number of errors.
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    /// Add an error to the progress.
    pub fn add_error(&mut self, error: OperationError) {
        self.errors.push(error);
    }

    /// Update the current file being processed.
    pub fn set_current_file(&mut self, path: Option<PathBuf>) {
        self.current_file = path;
    }

    /// Increment the completed count and add bytes.
    pub fn complete_file(&mut self, bytes: u64) {
        self.files_completed += 1;
        self.bytes_processed += bytes;
    }
}

/// Result of a completed operation.
#[derive(Debug, Clone)]
pub struct OperationComplete {
    /// The type of operation.
    pub operation_type: OperationType,
    /// Number of items successfully processed.
    pub succeeded: usize,
    /// Number of items that failed.
    pub failed: usize,
    /// Total bytes processed.
    pub bytes_processed: u64,
    /// Errors that occurred.
    pub errors: Vec<OperationError>,
}

impl OperationComplete {
    /// Check if the operation was fully successful.
    pub fn is_success(&self) -> bool {
        self.failed == 0
    }

    /// Get a human-readable summary of the operation.
    pub fn summary(&self) -> String {
        let action = match self.operation_type {
            OperationType::Copy => "Copied",
            OperationType::Move => "Moved",
            OperationType::Delete => "Deleted",
            OperationType::Rename => "Renamed",
            OperationType::CreateFile => "Created",
            OperationType::CreateDirectory => "Created",
        };

        if self.failed == 0 {
            format!("{} {} items", action, self.succeeded)
        } else {
            format!(
                "{} {} items, {} failed",
                action, self.succeeded, self.failed
            )
        }
    }
}
