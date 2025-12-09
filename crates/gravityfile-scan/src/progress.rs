//! Scan progress reporting.

use std::path::PathBuf;
use std::time::{Duration, Instant};

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

/// Internal progress tracker with timing.
/// Reserved for async progress reporting with tokio channels.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ProgressTracker {
    start_time: Instant,
    files_scanned: u64,
    dirs_scanned: u64,
    bytes_scanned: u64,
    errors_count: u64,
    current_path: PathBuf,
}

#[allow(dead_code)]
impl ProgressTracker {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            files_scanned: 0,
            dirs_scanned: 0,
            bytes_scanned: 0,
            errors_count: 0,
            current_path: PathBuf::new(),
        }
    }

    pub fn record_file(&mut self, size: u64) {
        self.files_scanned += 1;
        self.bytes_scanned += size;
    }

    pub fn record_dir(&mut self) {
        self.dirs_scanned += 1;
    }

    pub fn record_error(&mut self) {
        self.errors_count += 1;
    }

    pub fn set_current_path(&mut self, path: PathBuf) {
        self.current_path = path;
    }

    pub fn snapshot(&self) -> ScanProgress {
        ScanProgress {
            files_scanned: self.files_scanned,
            dirs_scanned: self.dirs_scanned,
            bytes_scanned: self.bytes_scanned,
            current_path: self.current_path.clone(),
            errors_count: self.errors_count,
            elapsed: self.start_time.elapsed(),
        }
    }
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}
