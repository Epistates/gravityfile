//! Deletion through the ops engine, adapted to the app's result channel.

use std::path::PathBuf;

use gravityfile_ops::{CancellationToken, DeleteMode, DeleteResult};
use tokio::sync::mpsc;

use super::constants::SCAN_CHANNEL_SIZE;
use super::state::{DeletionProgress, ScanResult};

/// Delete `(path, size)` items in the background (to the Trash by default).
/// Dropping the receiver stops after the current item.
pub fn start_deletion(items: Vec<(PathBuf, u64)>) -> mpsc::Receiver<ScanResult> {
    start_deletion_with_mode(items, DeleteMode::Trash)
}

pub fn start_deletion_with_mode(
    items: Vec<(PathBuf, u64)>,
    mode: DeleteMode,
) -> mpsc::Receiver<ScanResult> {
    let total = items.len();
    let token = CancellationToken::new();
    let mut rx = gravityfile_ops::start_delete(items, mode, token.clone());
    let (tx, result_rx) = mpsc::channel(SCAN_CHANNEL_SIZE);
    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                biased;
                _ = tx.closed() => { token.cancel(); break; }
                result = rx.recv() => match result {
                    Some(result) => result,
                    None => break,
                },
            };
            let message = match result {
                DeleteResult::Progress(progress) => {
                    ScanResult::DeletionProgress(DeletionProgress {
                        total,
                        deleted: progress
                            .files_completed
                            .saturating_sub(progress.error_count()),
                        failed: progress.error_count(),
                        bytes_freed: progress.bytes_processed,
                        current: progress.current_file,
                    })
                }
                DeleteResult::Complete(complete) => ScanResult::DeletionComplete {
                    mode: complete.mode,
                    failed: complete.inner.failed,
                    bytes_freed: complete.inner.bytes_processed,
                    errors: complete.inner.errors,
                    deleted: complete.deleted,
                },
            };
            if tx.send(message).await.is_err() {
                token.cancel();
                break;
            }
        }
    });
    result_rx
}

/// User-facing summary of a finished deletion.
pub fn format_deletion_result(
    mode: DeleteMode,
    deleted: usize,
    failed: usize,
    bytes_freed: u64,
) -> (bool, String) {
    use crate::ui::format_size;

    let success = failed == 0;
    let size = format_size(bytes_freed);
    let msg = match mode {
        DeleteMode::Permanent if success => format!("Permanently deleted {deleted} items ({size})"),
        DeleteMode::Permanent => {
            format!("Permanently deleted {deleted} items ({size}), {failed} failed")
        }
        DeleteMode::Trash if success => format!("Moved {deleted} items ({size}) to trash"),
        DeleteMode::Trash if deleted > 0 => {
            format!("Moved {deleted} to trash, {failed} failed ({size} moved)")
        }
        DeleteMode::Trash => format!("Failed to delete {failed} items"),
    };
    (success, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    async fn complete(mut rx: mpsc::Receiver<ScanResult>) -> (Vec<PathBuf>, usize, u64) {
        while let Some(result) = rx.recv().await {
            if let ScanResult::DeletionComplete {
                deleted,
                failed,
                bytes_freed,
                ..
            } = result
            {
                return (deleted, failed, bytes_freed);
            }
        }
        panic!("deletion ended without completion");
    }

    #[tokio::test]
    async fn permanent_deletion_reports_paths_bytes_and_failures() {
        let temp = TempDir::new().unwrap();
        let paths: Vec<_> = (0..4).map(|i| temp.path().join(i.to_string())).collect();
        for path in &paths {
            fs::write(path, []).unwrap();
        }
        let mut items: Vec<_> = paths.iter().cloned().map(|path| (path, 10)).collect();
        items.push((temp.path().join("missing"), 5));
        let (deleted, failed, bytes) =
            complete(start_deletion_with_mode(items, DeleteMode::Permanent)).await;
        assert_eq!(deleted, paths);
        assert_eq!((failed, bytes), (1, 40));
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[tokio::test]
    async fn empty_batches_complete() {
        let (deleted, failed, _) =
            complete(start_deletion_with_mode(Vec::new(), DeleteMode::Permanent)).await;
        assert!(deleted.is_empty() && failed == 0);
    }

    #[test]
    fn summaries() {
        assert_eq!(
            format_deletion_result(DeleteMode::Trash, 3, 0, 0),
            (true, "Moved 3 items (0 B) to trash".to_string())
        );
        let (success, message) = format_deletion_result(DeleteMode::Trash, 0, 2, 0);
        assert!(!success && message.contains("Failed to delete 2"));
        let (success, message) = format_deletion_result(DeleteMode::Permanent, 1, 1, 0);
        assert!(!success && message.contains("Permanently deleted 1"));
    }
}
