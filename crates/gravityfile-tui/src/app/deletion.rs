//! Deletion operations.

use std::fs;
use std::path::PathBuf;

use tokio::sync::mpsc;

use super::constants::SCAN_CHANNEL_SIZE;
use super::state::{DeletionProgress, ScanResult};

/// Start background deletion of files/directories.
///
/// Takes a list of (path, size) tuples and returns a receiver for progress updates.
pub fn start_deletion(items: Vec<(PathBuf, u64)>) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(SCAN_CHANNEL_SIZE);
    let total = items.len();

    if total == 0 {
        return rx;
    }

    tokio::spawn(async move {
        let mut deleted = 0;
        let mut failed = 0;
        let mut bytes_freed: u64 = 0;

        for (i, (path, size)) in items.iter().enumerate() {
            // Send progress update
            let _ = tx
                .send(ScanResult::DeletionProgress(DeletionProgress {
                    total,
                    deleted,
                    failed,
                    bytes_freed,
                    current: Some(path.clone()),
                }))
                .await;

            // Perform deletion in blocking task to not block the async runtime
            let path_clone = path.clone();
            let result = tokio::task::spawn_blocking(move || {
                if path_clone.is_dir() {
                    fs::remove_dir_all(&path_clone)
                } else {
                    fs::remove_file(&path_clone)
                }
            })
            .await;

            match result {
                Ok(Ok(())) => {
                    deleted += 1;
                    bytes_freed += size;
                }
                _ => {
                    failed += 1;
                }
            }

            // Send updated progress after each deletion
            if i < total - 1 {
                let _ = tx
                    .send(ScanResult::DeletionProgress(DeletionProgress {
                        total,
                        deleted,
                        failed,
                        bytes_freed,
                        current: items.get(i + 1).map(|(p, _)| p.clone()),
                    }))
                    .await;
            }
        }

        // Send completion
        let _ = tx
            .send(ScanResult::DeletionComplete {
                deleted,
                failed,
                bytes_freed,
            })
            .await;
    });

    rx
}

/// Format deletion result as a user-friendly message.
pub fn format_deletion_result(deleted: usize, failed: usize, bytes_freed: u64) -> (bool, String) {
    use crate::ui::format_size;

    let success = failed == 0;
    let msg = if success {
        format!("Deleted {} items, freed {}", deleted, format_size(bytes_freed))
    } else {
        format!(
            "Deleted {}, failed {} (freed {})",
            deleted,
            failed,
            format_size(bytes_freed)
        )
    };

    (success, msg)
}
