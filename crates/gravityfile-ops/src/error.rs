//! Per-path operation diagnostics.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A problem with one path during an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationError {
    pub path: PathBuf,
    pub message: String,
    /// When nonzero, this record stands in for that many omitted diagnostics.
    #[serde(default)]
    pub omitted_count: usize,
}

impl OperationError {
    pub fn new(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
            omitted_count: 0,
        }
    }

    /// Number of diagnostics this record represents.
    pub fn represented_count(&self) -> usize {
        self.omitted_count.max(1)
    }

    pub(crate) fn omitted(count: usize) -> Self {
        Self {
            path: PathBuf::new(),
            message: format!("{count} more problem(s) not listed"),
            omitted_count: count,
        }
    }
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.as_os_str().is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path.display(), self.message)
        }
    }
}
