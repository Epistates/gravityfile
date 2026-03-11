//! Scan progress reporting.

use std::path::PathBuf;
use std::time::Duration;

/// Progress information during a scan.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// Number of files scanned so far.
    pub files_scanned: u64,
    /// Number of directories scanned so far.
    pub dirs_scanned: u64,
    /// Total bytes scanned so far.
    pub bytes_scanned: u64,
    /// Current path being scanned.
    pub current_path: PathBuf,
    /// Number of errors/warnings encountered.
    pub errors_count: u64,
    /// Time elapsed since scan started.
    pub elapsed: Duration,
}

impl ScanProgress {
    /// Create initial progress state.
    pub fn new() -> Self {
        Self {
            files_scanned: 0,
            dirs_scanned: 0,
            bytes_scanned: 0,
            current_path: PathBuf::new(),
            errors_count: 0,
            elapsed: Duration::ZERO,
        }
    }

    /// Calculate scan rate in files per second.
    pub fn files_per_second(&self) -> f64 {
        if self.elapsed.as_secs_f64() > 0.0 {
            self.files_scanned as f64 / self.elapsed.as_secs_f64()
        } else {
            0.0
        }
    }

    /// Calculate scan rate in bytes per second.
    pub fn bytes_per_second(&self) -> f64 {
        if self.elapsed.as_secs_f64() > 0.0 {
            self.bytes_scanned as f64 / self.elapsed.as_secs_f64()
        } else {
            0.0
        }
    }

    /// Get total items scanned (files + dirs).
    pub fn total_items(&self) -> u64 {
        self.files_scanned + self.dirs_scanned
    }
}

impl Default for ScanProgress {
    fn default() -> Self {
        Self::new()
    }
}
