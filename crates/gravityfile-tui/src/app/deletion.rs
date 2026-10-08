//! Deletion operations with trash support and symlink safety.

use std::fs;
use std::path::PathBuf;

use tokio::sync::mpsc;

use super::constants::SCAN_CHANNEL_SIZE;
use super::state::{DeletionProgress, ScanResult};

/// Deletion mode - determines if files go to trash or are permanently deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeletionMode {
    /// Move files to system trash (recoverable).
    #[default]
    Trash,
    /// Permanently delete files (not recoverable).
    ///
    /// Supported by [`delete_path`] but deliberately not reachable from the
    /// UI: every user-facing delete goes through the trash. Kept so the
    /// unlink semantics stay covered by tests.
    #[allow(dead_code)]
    Permanent,
}

/// Start background deletion of files/directories.
///
/// Takes a list of (path, size) tuples and returns a receiver for progress updates.
/// Uses system trash. Progress is best-effort; completion is reliable, including
/// for empty batches. Dropping the receiver stops subsequent items; an active
/// filesystem/trash call is awaited and cannot be interrupted.
pub fn start_deletion(items: Vec<(PathBuf, u64)>) -> mpsc::Receiver<ScanResult> {
    start_deletion_with_mode(items, DeletionMode::Trash)
}

/// Start background deletion with explicit mode selection.
pub fn start_deletion_with_mode(
    items: Vec<(PathBuf, u64)>,
    mode: DeletionMode,
) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(SCAN_CHANNEL_SIZE);
    tokio::spawn(delete_items(items, mode, tx));
    rx
}

async fn delete_items(
    items: Vec<(PathBuf, u64)>,
    mode: DeletionMode,
    tx: mpsc::Sender<ScanResult>,
) {
    let total = items.len();
    let mut deleted = 0;
    let mut failed = 0;
    let mut bytes_freed = 0u64;
    let mut errors = Vec::new();
    for (path, size) in items {
        match tx.try_reserve() {
            Ok(permit) => permit.send(ScanResult::DeletionProgress(DeletionProgress {
                total,
                deleted,
                failed,
                bytes_freed,
                current: Some(path.clone()),
            })),
            Err(mpsc::error::TrySendError::Closed(_)) => return,
            Err(mpsc::error::TrySendError::Full(_)) => {}
        }
        let error_path = path.clone();
        let receiver = tx.clone();
        let result = tokio::task::spawn_blocking(move || {
            if receiver.is_closed() {
                return None;
            }
            Some(delete_path(&path, mode))
        })
        .await;
        match result {
            Ok(Some(Ok(()))) => {
                deleted += 1;
                bytes_freed = bytes_freed.saturating_add(size);
            }
            Ok(Some(Err(error))) => {
                tracing::warn!(path = ?error_path, "Deletion failed: {error}");
                errors.push(gravityfile_ops::OperationError::new(error_path, error));
                failed += 1;
            }
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(path = ?error_path, "Deletion task failed: {error}");
                errors.push(gravityfile_ops::OperationError::new(
                    error_path,
                    error.to_string(),
                ));
                failed += 1;
            }
        }
    }
    let _ = tx
        .send(ScanResult::DeletionComplete {
            mode,
            deleted,
            failed,
            bytes_freed,
            errors,
        })
        .await;
}

/// Delete a single path with proper symlink handling.
///
/// Key behaviors:
/// - Symlinks are always removed as files (never follow the target)
/// - Directories use remove_dir_all (which also handles symlinks safely per Rust docs)
/// - Regular files use remove_file
fn delete_path(path: &PathBuf, mode: DeletionMode) -> Result<(), String> {
    // First, check if path exists at all
    // Use symlink_metadata to not follow symlinks
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => return Err(format!("Cannot access path: {}", e)),
    };

    match mode {
        DeletionMode::Trash => {
            // Use trash crate for safe, recoverable deletion
            // The trash crate handles symlinks correctly
            trash::delete(path).map_err(|e| format!("Failed to move to trash: {}", e))
        }
        DeletionMode::Permanent => {
            // IMPORTANT: Check symlink FIRST before checking is_dir()
            // is_dir() follows symlinks, so a symlink to a directory would return true
            // But we want to delete the symlink itself, not follow it
            if metadata.is_symlink() {
                // Symlinks are always deleted as files, regardless of what they point to
                fs::remove_file(path).map_err(|e| format!("Failed to remove symlink: {}", e))
            } else if metadata.is_dir() {
                // Regular directory - remove recursively
                fs::remove_dir_all(path).map_err(|e| format!("Failed to remove directory: {}", e))
            } else {
                // Regular file
                fs::remove_file(path).map_err(|e| format!("Failed to remove file: {}", e))
            }
        }
    }
}

/// Format deletion result as a user-friendly message.
pub fn format_deletion_result(
    mode: DeletionMode,
    deleted: usize,
    failed: usize,
    bytes_freed: u64,
) -> (bool, String) {
    use crate::ui::format_size;

    let success = failed == 0;
    let msg = if mode == DeletionMode::Permanent {
        format!(
            "Permanently deleted {} items ({}), {} failed",
            deleted,
            format_size(bytes_freed),
            failed
        )
    } else if success {
        format!(
            "Moved {} items ({}) to trash",
            deleted,
            format_size(bytes_freed)
        )
    } else if deleted > 0 {
        format!(
            "Moved {} to trash, {} failed ({} moved)",
            deleted,
            failed,
            format_size(bytes_freed)
        )
    } else {
        format!("Failed to delete {} items", failed)
    };

    (success, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use tempfile::TempDir;

    #[tokio::test]
    async fn deletion_stops_before_mutation_when_receiver_is_closed() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("keep");
        fs::write(&path, b"keep").unwrap();
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        delete_items(vec![(path.clone(), 4)], DeletionMode::Permanent, tx).await;
        assert_eq!(fs::read(path).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn deletion_progress_backpressure_does_not_stall_and_empty_batches_complete() {
        let temp = TempDir::new().unwrap();
        let paths: Vec<_> = (0..4).map(|i| temp.path().join(i.to_string())).collect();
        for path in &paths {
            fs::write(path, []).unwrap();
        }
        let (tx, mut rx) = mpsc::channel(1);
        let work = tokio::spawn(delete_items(
            paths.iter().cloned().map(|p| (p, u64::MAX)).collect(),
            DeletionMode::Permanent,
            tx,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while paths.last().unwrap().exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut completed = false;
        while let Some(result) = rx.recv().await {
            if let ScanResult::DeletionComplete {
                deleted,
                failed,
                bytes_freed,
                errors,
                ..
            } = result
            {
                assert_eq!((deleted, failed, bytes_freed), (4, 0, u64::MAX));
                assert!(errors.is_empty());
                completed = true;
            }
        }
        work.await.unwrap();
        assert!(completed);
        let mut rx = start_deletion_with_mode(Vec::new(), DeletionMode::Permanent);
        assert!(matches!(
            rx.recv().await,
            Some(ScanResult::DeletionComplete {
                deleted: 0,
                failed: 0,
                ..
            })
        ));
    }

    #[test]
    fn test_delete_regular_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        File::create(&file_path).unwrap();

        assert!(file_path.exists());
        delete_path(&file_path, DeletionMode::Permanent).unwrap();
        assert!(!file_path.exists());
    }

    #[test]
    fn test_delete_directory() {
        let temp_dir = TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("subdir");
        fs::create_dir(&dir_path).unwrap();
        File::create(dir_path.join("file.txt")).unwrap();

        assert!(dir_path.exists());
        delete_path(&dir_path, DeletionMode::Permanent).unwrap();
        assert!(!dir_path.exists());
    }

    #[test]
    fn test_delete_nested_directory() {
        let temp_dir = TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("level1");
        fs::create_dir(&dir_path).unwrap();
        fs::create_dir(dir_path.join("level2")).unwrap();
        fs::create_dir(dir_path.join("level2/level3")).unwrap();
        File::create(dir_path.join("level2/level3/deep_file.txt")).unwrap();

        assert!(dir_path.exists());
        delete_path(&dir_path, DeletionMode::Permanent).unwrap();
        assert!(!dir_path.exists());
    }

    #[test]
    fn test_delete_nonexistent_path() {
        let temp_dir = TempDir::new().unwrap();
        let nonexistent = temp_dir.path().join("does_not_exist.txt");

        let result = delete_path(&nonexistent, DeletionMode::Permanent);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot access path"));
    }

    #[test]
    fn test_delete_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let empty_dir = temp_dir.path().join("empty");
        fs::create_dir(&empty_dir).unwrap();

        assert!(empty_dir.exists());
        delete_path(&empty_dir, DeletionMode::Permanent).unwrap();
        assert!(!empty_dir.exists());
    }

    #[test]
    fn test_delete_file_with_content() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("content.txt");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "Some content that should be deleted").unwrap();
        drop(file);

        assert!(file_path.exists());
        delete_path(&file_path, DeletionMode::Permanent).unwrap();
        assert!(!file_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_delete_symlink_to_file() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("target.txt");
        let link_path = temp_dir.path().join("link.txt");

        File::create(&file_path).unwrap();
        symlink(&file_path, &link_path).unwrap();

        // Verify setup
        assert!(file_path.exists());
        assert!(link_path.symlink_metadata().unwrap().is_symlink());

        // Delete the symlink
        delete_path(&link_path, DeletionMode::Permanent).unwrap();

        // Symlink should be gone, but target should remain
        assert!(!link_path.exists());
        assert!(
            file_path.exists(),
            "Target file should NOT be deleted when deleting symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_delete_symlink_to_directory() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("target_dir");
        let link_path = temp_dir.path().join("link_dir");

        fs::create_dir(&dir_path).unwrap();
        File::create(dir_path.join("file.txt")).unwrap();
        symlink(&dir_path, &link_path).unwrap();

        // Verify setup
        assert!(dir_path.exists());
        assert!(link_path.symlink_metadata().unwrap().is_symlink());

        // Delete the symlink
        delete_path(&link_path, DeletionMode::Permanent).unwrap();

        // Symlink should be gone, but target directory and contents should remain
        assert!(!link_path.exists());
        assert!(
            dir_path.exists(),
            "Target directory should NOT be deleted when deleting symlink"
        );
        assert!(
            dir_path.join("file.txt").exists(),
            "Files in target directory should NOT be deleted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_delete_broken_symlink() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let link_path = temp_dir.path().join("broken_link");

        // Create symlink to non-existent target
        symlink("/nonexistent/path/that/does/not/exist", &link_path).unwrap();

        // Verify it's a broken symlink
        assert!(link_path.symlink_metadata().unwrap().is_symlink());
        assert!(!link_path.exists()); // exists() follows symlinks, returns false for broken

        // Should be able to delete broken symlink
        delete_path(&link_path, DeletionMode::Permanent).unwrap();

        // Verify symlink is gone
        assert!(link_path.symlink_metadata().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_delete_symlink_chain() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("target.txt");
        let link1_path = temp_dir.path().join("link1");
        let link2_path = temp_dir.path().join("link2");

        File::create(&file_path).unwrap();
        symlink(&file_path, &link1_path).unwrap();
        symlink(&link1_path, &link2_path).unwrap(); // link2 -> link1 -> file

        // Delete the middle link
        delete_path(&link1_path, DeletionMode::Permanent).unwrap();

        // link1 should be gone
        assert!(link1_path.symlink_metadata().is_err());
        // Original file should remain
        assert!(file_path.exists());
        // link2 now points to nothing (broken)
        assert!(link2_path.symlink_metadata().unwrap().is_symlink());
        assert!(!link2_path.exists()); // broken link
    }

    #[cfg(unix)]
    #[test]
    fn test_delete_directory_containing_symlinks() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let outside_file = temp_dir.path().join("outside.txt");
        let dir_path = temp_dir.path().join("container");
        let link_in_dir = dir_path.join("link_to_outside");

        File::create(&outside_file).unwrap();
        fs::create_dir(&dir_path).unwrap();
        symlink(&outside_file, &link_in_dir).unwrap();

        // Delete the directory containing the symlink
        delete_path(&dir_path, DeletionMode::Permanent).unwrap();

        // Directory and symlink should be gone
        assert!(!dir_path.exists());
        // But the outside file should remain
        assert!(
            outside_file.exists(),
            "File outside directory should NOT be deleted"
        );
    }

    #[test]
    fn test_format_deletion_result_success() {
        let (success, msg) = format_deletion_result(DeletionMode::Trash, 5, 0, 1024 * 1024);
        assert!(success);
        assert!(msg.contains("5 items"));
        assert!(msg.contains("trash"));
        assert!(!msg.contains("freed"));
    }

    #[test]
    fn test_format_deletion_result_partial_failure() {
        let (success, msg) = format_deletion_result(DeletionMode::Trash, 3, 2, 512 * 1024);
        assert!(!success);
        assert!(msg.contains("3"));
        assert!(msg.contains("2 failed"));
    }

    #[test]
    fn test_format_deletion_result_total_failure() {
        let (success, msg) = format_deletion_result(DeletionMode::Trash, 0, 5, 0);
        assert!(!success);
        assert!(msg.contains("Failed"));
        assert!(msg.contains("5"));
    }

    #[test]
    fn test_deletion_mode_default() {
        let mode = DeletionMode::default();
        assert_eq!(mode, DeletionMode::Trash);
    }
}
