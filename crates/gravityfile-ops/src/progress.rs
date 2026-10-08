//! Progress reporting types for file operations.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::OperationError;

const MAX_DIAGNOSTIC_PAYLOAD_BYTES: usize = 8 * 1024;

/// The type of operation being performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    Copy,
    Move,
    Delete,
    /// Items restored from the system Trash.
    Restore,
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
            Self::Restore => write!(f, "Restore"),
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
    /// When the operation started; the basis for throughput and ETA.
    pub started_at: Instant,
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
            started_at: Instant::now(),
        }
    }

    /// Average bytes per second since the operation started.
    pub fn bytes_per_second(&self) -> f64 {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.bytes_processed as f64 / elapsed
        } else {
            0.0
        }
    }

    /// Estimated time remaining, when the byte total and rate are known.
    pub fn eta(&self) -> Option<Duration> {
        let rate = self.bytes_per_second();
        let remaining = self.bytes_total.checked_sub(self.bytes_processed)?;
        (rate >= 1.0 && self.bytes_total > 0)
            .then(|| Duration::from_secs_f64(remaining as f64 / rate))
    }

    /// Record bytes transferred within the current item.
    pub fn add_bytes(&mut self, bytes: u64) {
        self.bytes_processed = self.bytes_processed.saturating_add(bytes);
    }

    /// Get the estimated progress as a percentage (0.0 to 100.0).
    /// Totals may become stale; 100% is not a terminal success signal.
    pub fn percentage(&self) -> f64 {
        if self.bytes_total > 0 {
            ((self.bytes_processed as f64 / self.bytes_total as f64) * 100.0).min(100.0)
        } else if self.files_total > 0 {
            ((self.files_completed as f64 / self.files_total as f64) * 100.0).min(100.0)
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
        self.errors.iter().fold(0usize, |total, error| {
            total.saturating_add(error.represented_count())
        })
    }

    /// Retain at most 1,000 diagnostic records, preserving total counts through
    /// an explicit omission record after the first 999 details when truncated.
    /// Individual path/message payloads above 8 KiB become omission records too;
    /// native paths are never shortened into misleading partial filenames.
    pub fn add_error(&mut self, error: OperationError) {
        const MAX_RETAINED_ERRORS: usize = 1000;
        let payload_bytes = error
            .path
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .saturating_add(error.message.len());
        let error = if payload_bytes > MAX_DIAGNOSTIC_PAYLOAD_BYTES {
            let mut omitted = OperationError::omitted(error.represented_count());
            omitted
                .message
                .push_str(" (diagnostic payload exceeded 8 KiB)");
            omitted
        } else {
            error
        };
        if self.errors.len() < MAX_RETAINED_ERRORS {
            self.errors.push(error);
        } else {
            let omitted = self
                .errors
                .drain(MAX_RETAINED_ERRORS - 1..)
                .fold(error.represented_count(), |total, prior| {
                    total.saturating_add(prior.represented_count())
                });
            self.errors.push(OperationError::omitted(omitted));
        }
    }

    /// Update the current file being processed.
    pub fn set_current_file(&mut self, path: Option<PathBuf>) {
        self.current_file = path;
    }

    /// Increment the completed count and add bytes, saturating at counter limits.
    pub fn complete_file(&mut self, bytes: u64) {
        self.files_completed = self.files_completed.saturating_add(1);
        self.bytes_processed = self.bytes_processed.saturating_add(bytes);
    }
}

/// Result of a completed operation.
#[derive(Debug, Clone)]
pub struct OperationComplete {
    /// Whether cancellation or an explicit abort stopped the operation.
    /// Completed items remain reflected in the counts and undo records.
    pub cancelled: bool,
    /// The type of operation.
    pub operation_type: OperationType,
    /// Number of items successfully processed.
    pub succeeded: usize,
    /// Number of items that failed.
    pub failed: usize,
    /// Number of items the user chose to skip. Skips are not failures.
    pub skipped: usize,
    /// Total bytes processed.
    pub bytes_processed: u64,
    /// Errors that occurred.
    pub errors: Vec<OperationError>,
}

impl OperationComplete {
    /// Check if the operation was fully successful.
    pub fn is_success(&self) -> bool {
        self.failed == 0 && self.errors.is_empty() && !self.cancelled
    }

    /// Get a human-readable summary of the operation.
    pub fn summary(&self) -> String {
        let action = match self.operation_type {
            OperationType::Copy => "Copied",
            OperationType::Move => "Moved",
            OperationType::Delete => "Deleted",
            OperationType::Restore => "Restored",
            OperationType::Rename => "Renamed",
            OperationType::CreateFile => "Created",
            OperationType::CreateDirectory => "Created",
        };

        let mut summary = format!("{action} {} items", self.succeeded);
        if self.skipped > 0 {
            summary.push_str(&format!(", {} skipped", self.skipped));
        }
        if self.failed > 0 {
            summary.push_str(&format!(", {} failed", self.failed));
        } else if !self.errors.is_empty() {
            summary.push_str(&format!(
                ", {} errors",
                self.errors.iter().fold(0usize, |total, error| total
                    .saturating_add(error.represented_count()))
            ));
        }
        if self.cancelled {
            summary.push_str("; operation cancelled");
        }
        summary
    }
}

/// Progress is replaceable telemetry; a slow consumer must not suspend mutation
/// cleanup or cancellation. Reserve capacity before constructing the snapshot so
/// dropped updates never clone potentially large error lists. Terminal results
/// and conflict requests use reliable sends.
pub(crate) fn publish_progress<T>(
    tx: &tokio::sync::mpsc::Sender<T>,
    make_progress: impl FnOnce() -> T,
    token: &tokio_util::sync::CancellationToken,
) {
    match tx.try_reserve() {
        Ok(permit) => permit.send(make_progress()),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => token.cancel(),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn diagnostic_payload_limit_preserves_boundaries_and_failure_counts() {
        let mut progress = OperationProgress::new(OperationType::Copy, 0, 0);
        let path = PathBuf::from("item");
        let message = "x".repeat(MAX_DIAGNOSTIC_PAYLOAD_BYTES - 4);
        progress.add_error(OperationError::new(path.clone(), message.clone()));
        assert_eq!(progress.errors[0].path, path);
        assert_eq!(progress.errors[0].message, message);
        progress.add_error(OperationError::new(path, message + "x"));
        assert_eq!(progress.errors[1].omitted_count, 1);
        assert!(progress.errors[1].path.as_os_str().is_empty());
        assert!(progress.errors[1].message.contains("exceeded 8 KiB"));
        let mut oversized_omission = OperationError::omitted(37);
        oversized_omission.message = "界".repeat(MAX_DIAGNOSTIC_PAYLOAD_BYTES);
        progress.add_error(oversized_omission);
        assert_eq!(progress.error_count(), 39);
        assert_eq!(progress.errors[2].omitted_count, 37);
        for _ in 0..2000 {
            progress.add_error(OperationError::new(
                PathBuf::from("p".repeat(MAX_DIAGNOSTIC_PAYLOAD_BYTES + 1)),
                "failed",
            ));
        }
        assert_eq!(progress.error_count(), 2039);
        assert_eq!(progress.errors.len(), 1000);
        assert!(progress.errors.iter().all(|error| {
            error.path.as_os_str().as_encoded_bytes().len() + error.message.len()
                <= MAX_DIAGNOSTIC_PAYLOAD_BYTES
        }));
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_payload_budget_uses_native_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![0xff; 4096]));
        let mut progress = OperationProgress::new(OperationType::Copy, 0, 0);
        progress.add_error(OperationError::new(path.clone(), "x".repeat(4096)));
        assert_eq!(progress.errors[0].path, path);
        assert_eq!(progress.errors[0].omitted_count, 0);
        progress.add_error(OperationError::new(path, "x".repeat(4097)));
        assert_eq!(progress.errors[1].omitted_count, 1);
    }

    #[test]
    fn retained_diagnostics_are_bounded_without_losing_total_counts() {
        let mut progress = OperationProgress::new(OperationType::Copy, 100_000, 0);
        for index in 0..100_000 {
            progress.add_error(OperationError::new(
                PathBuf::from(format!("item-{index}")),
                "failed",
            ));
            assert!(progress.errors.len() <= 1000);
            if [998, 999, 1000, 99_999].contains(&index) {
                assert_eq!(progress.error_count(), index + 1);
            }
        }
        assert_eq!(progress.errors.len(), 1000);
        for (index, error) in progress.errors[..999].iter().enumerate() {
            assert_eq!(error.path, PathBuf::from(format!("item-{index}")));
            assert_eq!(error.omitted_count, 0);
        }
        assert_eq!(progress.errors[999].omitted_count, 99_001);
        assert!(progress.errors[999].path.as_os_str().is_empty());
        assert!(progress.errors[999].message.contains("99001"));
        assert_eq!(progress.clone().error_count(), 100_000);
        let completion = OperationComplete {
            operation_type: OperationType::Copy,
            cancelled: false,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            bytes_processed: 0,
            errors: progress.errors,
        };
        assert!(!completion.is_success());
        assert!(completion.summary().contains("100000 errors"));
        let legacy: OperationError =
            serde_json::from_str(r#"{"path":"legacy","message":"failure"}"#).unwrap();
        assert_eq!(legacy.omitted_count, 0);
        assert_eq!(legacy.represented_count(), 1);
    }

    #[test]
    fn completion_diagnostics_prevent_success_even_without_failure_counts() {
        for cancelled in [false, true] {
            for failed in [0, 1] {
                for errors in [
                    Vec::new(),
                    vec![OperationError::new(PathBuf::from("item"), "failure")],
                ] {
                    let complete = OperationComplete {
                        operation_type: OperationType::Copy,
                        cancelled,
                        failed,
                        succeeded: 1,
                        skipped: 2,
                        bytes_processed: 0,
                        errors,
                    };
                    assert_eq!(
                        complete.is_success(),
                        !cancelled && failed == 0 && complete.errors.is_empty()
                    );
                    let summary = complete.summary();
                    assert!(summary.contains("2 skipped"));
                    assert_eq!(summary.contains("1 failed"), failed == 1);
                    assert_eq!(
                        summary.contains("1 errors"),
                        failed == 0 && !complete.errors.is_empty()
                    );
                    assert_eq!(summary.contains("cancelled"), cancelled);
                }
            }
        }
    }

    #[test]
    fn progress_is_bounded_for_unknown_stale_and_extreme_totals() {
        let mut progress = OperationProgress::new(OperationType::Copy, 0, 0);
        assert_eq!(progress.percentage(), 0.0);
        progress.files_total = 4;
        progress.complete_file(0);
        assert_eq!(progress.percentage(), 25.0);
        progress.bytes_total = 8;
        progress.bytes_processed = 4;
        assert_eq!(progress.percentage(), 50.0);
        for files_total in [0, 1, usize::MAX] {
            for bytes_total in [0, 1, u64::MAX] {
                progress.files_total = files_total;
                progress.bytes_total = bytes_total;
                progress.files_completed = usize::MAX;
                progress.bytes_processed = u64::MAX;
                progress.complete_file(u64::MAX);
                assert_eq!(progress.files_completed, usize::MAX);
                assert_eq!(progress.bytes_processed, u64::MAX);
                let expected = if files_total == 0 && bytes_total == 0 {
                    0.0
                } else {
                    100.0
                };
                assert_eq!(progress.percentage(), expected);
            }
        }
    }

    #[test]
    fn full_progress_queue_is_nonblocking_and_does_not_cancel() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(1).unwrap();
        let token = CancellationToken::new();
        publish_progress(
            &tx,
            || panic!("full queue must not construct a snapshot"),
            &token,
        );
        assert!(!token.is_cancelled());
        assert_eq!(rx.try_recv().unwrap(), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn closed_progress_receiver_cancels_work() {
        let (tx, rx) = tokio::sync::mpsc::channel::<usize>(1);
        drop(rx);
        let token = CancellationToken::new();
        publish_progress(
            &tx,
            || panic!("closed queue must not construct a snapshot"),
            &token,
        );
        assert!(token.is_cancelled());
    }

    #[test]
    fn snapshots_resume_after_backpressure_without_building_discarded_updates() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let token = CancellationToken::new();
        let constructed = std::cell::Cell::new(0);
        let snapshot = || {
            constructed.set(constructed.get() + 1);
            constructed.get()
        };
        publish_progress(&tx, snapshot, &token);
        for _ in 0..10_000 {
            publish_progress(&tx, snapshot, &token);
        }
        assert_eq!(constructed.get(), 1);
        assert_eq!(rx.try_recv().unwrap(), 1);
        publish_progress(&tx, snapshot, &token);
        assert_eq!(constructed.get(), 2);
        assert_eq!(rx.try_recv().unwrap(), 2);
        assert!(!token.is_cancelled());
    }
}
