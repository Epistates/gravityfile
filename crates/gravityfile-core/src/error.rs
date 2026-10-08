//! Error types for scanning operations.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur during scanning.
#[derive(Debug, Error, Serialize)]
pub enum ScanError {
    /// Permission denied for a path.
    #[error("Permission denied: {}: {}", crate::escape_filename(.path.as_os_str()), serialize_io_error_display(source))]
    PermissionDenied {
        #[serde(serialize_with = "crate::path_serde::serialize")]
        path: PathBuf,
        #[source]
        #[serde(serialize_with = "serialize_io_error")]
        source: std::io::Error,
    },

    /// Path not found.
    #[error("Path not found: {}: {}", crate::escape_filename(.path.as_os_str()), serialize_io_error_display(source))]
    NotFound {
        #[serde(serialize_with = "crate::path_serde::serialize")]
        path: PathBuf,
        #[source]
        #[serde(serialize_with = "serialize_io_error")]
        source: std::io::Error,
    },

    /// Generic I/O error.
    #[error("I/O error at {}: {}", crate::escape_filename(.path.as_os_str()), serialize_io_error_display(source))]
    Io {
        #[serde(serialize_with = "crate::path_serde::serialize")]
        path: PathBuf,
        #[source]
        #[serde(serialize_with = "serialize_io_error")]
        source: std::io::Error,
    },

    /// Operation was interrupted.
    #[error("Operation interrupted")]
    Interrupted,

    /// Too many errors occurred.
    #[error("Too many errors ({count}), aborting")]
    TooManyErrors { count: usize },

    /// The configured descendant-entry budget was exhausted.
    #[error("Scan entry limit exceeded ({limit} descendant entries)")]
    EntryLimitExceeded { limit: u64 },

    /// Invalid configuration.
    #[error("Invalid configuration: {}", crate::escape_terminal_text(message))]
    InvalidConfig { message: String },

    /// Root path is not a directory.
    #[error("Root path is not a directory: {}", crate::escape_filename(.path.as_os_str()))]
    NotADirectory {
        #[serde(serialize_with = "crate::path_serde::serialize")]
        path: PathBuf,
    },

    /// Other error.
    #[error("{}", crate::escape_terminal_text(message))]
    Other { message: String },
}

fn serialize_io_error<S: serde::Serializer>(
    error: &std::io::Error,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.serialize_str(&error.to_string())
}

fn serialize_io_error_display(error: &std::io::Error) -> String {
    crate::escape_terminal_text(&error.to_string()).into_owned()
}

impl ScanError {
    /// Create an I/O error with path context. Preserves the original `io::Error` as the source.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let path = path.into();
        match source.kind() {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied { path, source },
            std::io::ErrorKind::NotFound => Self::NotFound { path, source },
            _ => Self::Io { path, source },
        }
    }
}

/// Kind of scan warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// Warning details omitted by a bounded scanner accumulator.
    Omitted { count: u64 },
}

/// Non-fatal warning encountered during scan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanWarning {
    /// Path where the warning occurred.
    #[serde(with = "crate::path_serde")]
    pub path: PathBuf,
    /// Human-readable message.
    pub message: String,
    /// Kind of warning.
    pub kind: WarningKind,
}

impl fmt::Display for ScanWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", crate::escape_terminal_text(&self.message))
    }
}

impl ScanWarning {
    /// Number of observed warnings represented by this record.
    pub fn represented_count(&self) -> u64 {
        match self.kind {
            WarningKind::Omitted { count } => count.max(1),
            _ => 1,
        }
    }

    /// Create a new scan warning.
    pub fn new(path: impl Into<PathBuf>, kind: WarningKind, message: impl Into<String>) -> Self {
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
            message: format!(
                "Permission denied: {}",
                crate::escape_filename(path.as_os_str())
            ),
            path,
            kind: WarningKind::PermissionDenied,
        }
    }

    /// Create a broken symlink warning.
    pub fn broken_symlink(path: impl Into<PathBuf>, target: &str) -> Self {
        let path = path.into();
        Self {
            message: format!(
                "Broken symlink: {} -> {}",
                crate::escape_filename(path.as_os_str()),
                crate::escape_filename(std::ffi::OsStr::new(target))
            ),
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
    fn displayed_errors_escape_paths_and_messages_but_preserve_sources() {
        let path = PathBuf::from("line\n\u{1b}\\n");
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::Other,
        ] {
            let error = ScanError::io(&path, std::io::Error::new(kind, "bad\n\u{1b}source"));
            let displayed = error.to_string();
            assert!(displayed.contains(r"line\n\u{1b}\\n"));
            assert!(displayed.contains(r"bad\n\u{1b}source"));
            assert!(!displayed.contains(['\n', '\u{1b}']));
            assert_eq!(
                std::error::Error::source(&error).unwrap().to_string(),
                "bad\n\u{1b}source"
            );
        }
        for error in [
            ScanError::NotADirectory { path },
            ScanError::InvalidConfig {
                message: "bad\nconfig".into(),
            },
            ScanError::Other {
                message: "bad\nother".into(),
            },
        ] {
            assert!(!error.to_string().contains(['\n', '\u{1b}']));
        }
        let warning = ScanWarning::new("root", WarningKind::ReadError, "bad\nwarning");
        assert_eq!(warning.to_string(), r"bad\nwarning");
        assert_eq!(warning.message, "bad\nwarning");
    }

    #[test]
    fn test_scan_error_io() {
        let err = ScanError::io(
            "/test/path",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        );
        assert!(matches!(err, ScanError::PermissionDenied { .. }));
    }

    #[test]
    fn test_scan_error_preserves_source() {
        let err = ScanError::io(
            "/test/path",
            std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        );
        match err {
            ScanError::NotFound { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            _ => panic!("Expected NotFound variant"),
        }
    }

    #[test]
    fn test_scan_warning_creation() {
        let warning = ScanWarning::permission_denied("/test/path");
        assert_eq!(warning.kind, WarningKind::PermissionDenied);
        assert!(warning.message.contains("Permission denied"));
        assert_eq!(format!("{warning}"), warning.message);
    }
}
