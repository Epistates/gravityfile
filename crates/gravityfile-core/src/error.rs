//! Error types for scanning operations.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur during scanning.
#[derive(Debug, Error)]
pub enum ScanError {
    /// Permission denied for a path.
    #[error("Permission denied: {path}")]
    PermissionDenied { path: PathBuf },

    /// Path not found.
    #[error("Path not found: {path}")]
    NotFound { path: PathBuf },

    /// Generic I/O error.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Operation was interrupted.
    #[error("Operation interrupted")]
    Interrupted,

    /// Too many errors occurred.
    #[error("Too many errors ({count}), aborting")]
    TooManyErrors { count: usize },

    /// Invalid configuration.
    #[error("Invalid configuration: {message}")]
    InvalidConfig { message: String },

    /// Root path is not a directory.
    #[error("Root path is not a directory: {path}")]
    NotADirectory { path: PathBuf },

    /// Other error.
    #[error("{message}")]
    Other { message: String },
}

impl ScanError {
    /// Create an I/O error with path context.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let path = path.into();
        match source.kind() {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied { path },
            std::io::ErrorKind::NotFound => Self::NotFound { path },
            _ => Self::Io { path, source },
        }
    }
}

/// Kind of scan warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarningKind {
    /// Permission was denied.
    PermissionDenied,
    /// Symbolic link target does not exist.
    BrokenSymlink,
    /// Error reading file/directory.
    ReadError,
    /// Error reading metadata.
    MetadataError,
    /// Filesystem boundary crossed (when not allowed).
    CrossFilesystem,
}

/// Non-fatal warning encountered during scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanWarning {
    /// Path where the warning occurred.
    pub path: PathBuf,
    /// Human-readable message.
    pub message: String,
    /// Kind of warning.
    pub kind: WarningKind,
}

impl ScanWarning {
    /// Create a new scan warning.
    pub fn new(path: impl Into<PathBuf>, message: impl Into<String>, kind: WarningKind) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            kind,
        }
    }

    /// Create a permission denied warning.
    pub fn permission_denied(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            message: format!("Permission denied: {}", path.display()),
            path,
            kind: WarningKind::PermissionDenied,
        }
    }

    /// Create a broken symlink warning.
    pub fn broken_symlink(path: impl Into<PathBuf>, target: &str) -> Self {
        let path = path.into();
        Self {
            message: format!("Broken symlink: {} -> {target}", path.display()),
            path,
            kind: WarningKind::BrokenSymlink,
        }
    }

    /// Create a read error warning.
    pub fn read_error(path: impl Into<PathBuf>, error: &std::io::Error) -> Self {
        let path = path.into();
        Self {
            message: format!("Read error: {error}"),
            path,
            kind: WarningKind::ReadError,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_error_io() {
        let err = ScanError::io(
            "/test/path",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        );
        assert!(matches!(err, ScanError::PermissionDenied { .. }));
    }

    #[test]
    fn test_scan_warning_creation() {
        let warning = ScanWarning::permission_denied("/test/path");
        assert_eq!(warning.kind, WarningKind::PermissionDenied);
        assert!(warning.message.contains("Permission denied"));
    }
}
