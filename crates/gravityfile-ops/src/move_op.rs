//! Async move operation with progress reporting.

use std::fs;
use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::conflict::{auto_rename_path, Conflict, ConflictKind, ConflictResolution};
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OperationError, OPERATION_CHANNEL_SIZE};

/// Result sent through the channel during move operations.
#[derive(Debug)]
pub enum MoveResult {
    /// Progress update.
    Progress(OperationProgress),
    /// A conflict was detected that needs resolution.
    Conflict(Conflict),
    /// The operation completed.
    Complete(OperationComplete),
}

/// Options for move operations.
#[derive(Debug, Clone, Default)]
pub struct MoveOptions {
    /// How to handle conflicts (None means ask for each).
    pub conflict_resolution: Option<ConflictResolution>,
}

/// Start an async move operation.
///
/// Returns a receiver for progress updates and results.
pub fn start_move(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: MoveOptions,
) -> mpsc::Receiver<MoveResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    if sources.is_empty() {
        let complete = OperationComplete {
            operation_type: OperationType::Move,
            succeeded: 0,
            failed: 0,
            bytes_processed: 0,
            errors: vec![],
        };
        tokio::spawn(async move {
            let _ = tx.send(MoveResult::Complete(complete)).await;
        });
        return rx;
    }

    tokio::spawn(async move {
        move_impl(sources, destination, options, tx).await;
    });

    rx
}

/// Internal implementation of move operation.
async fn move_impl(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: MoveOptions,
    tx: mpsc::Sender<MoveResult>,
) {
    let total_files = sources.len();
    let mut progress = OperationProgress::new(OperationType::Move, total_files, 0);
    let global_resolution: Option<ConflictResolution> = options.conflict_resolution;
    let mut succeeded = 0;
    let mut failed = 0;
    let mut moved_pairs: Vec<(PathBuf, PathBuf)> = Vec::new();

    // Ensure destination exists and is a directory
    if !destination.exists() {
        if let Err(e) = fs::create_dir_all(&destination) {
            progress.add_error(OperationError::new(
                destination.clone(),
                format!("Failed to create destination: {}", e),
            ));
            let _ = tx
                .send(MoveResult::Complete(OperationComplete {
                    operation_type: OperationType::Move,
                    succeeded: 0,
                    failed: sources.len(),
                    bytes_processed: 0,
                    errors: progress.errors.clone(),
                }))
                .await;
            return;
        }
    }

    for source in sources {
        let dest_path = destination.join(source.file_name().unwrap_or_default());

        // Check for self-move (moving directory into itself)
        if dest_path.starts_with(&source) {
            let _ = tx
                .send(MoveResult::Conflict(Conflict::source_is_ancestor(
                    source.clone(),
                    dest_path.clone(),
                )))
                .await;
            failed += 1;
            continue;
        }

        // Check for conflicts
        let final_dest = if dest_path.exists() {
            let conflict_kind = if dest_path.is_dir() {
                ConflictKind::DirectoryExists
            } else {
                ConflictKind::FileExists
            };

            let resolution = if let Some(res) = global_resolution {
                res.to_single()
            } else {
                let _ = tx
                    .send(MoveResult::Conflict(Conflict::new(
                        source.clone(),
                        dest_path.clone(),
                        conflict_kind,
                    )))
                    .await;
                ConflictResolution::Skip
            };

            match resolution {
                ConflictResolution::Skip | ConflictResolution::SkipAll => {
                    failed += 1;
                    continue;
                }
                ConflictResolution::Abort => {
                    let _ = tx
                        .send(MoveResult::Complete(OperationComplete {
                            operation_type: OperationType::Move,
                            succeeded,
                            failed: failed + 1,
                            bytes_processed: progress.bytes_processed,
                            errors: progress.errors.clone(),
                        }))
                        .await;
                    return;
                }
                ConflictResolution::AutoRename => auto_rename_path(&dest_path),
                ConflictResolution::Overwrite | ConflictResolution::OverwriteAll => {
                    // Remove existing before move
                    let _ = if dest_path.is_dir() {
                        fs::remove_dir_all(&dest_path)
                    } else {
                        fs::remove_file(&dest_path)
                    };
                    dest_path.clone()
                }
            }
        } else {
            dest_path.clone()
        };

        // Update progress
        progress.set_current_file(Some(source.clone()));
        let _ = tx.send(MoveResult::Progress(progress.clone())).await;

        // Perform the move
        let source_clone = source.clone();
        let dest_clone = final_dest.clone();

        let result = tokio::task::spawn_blocking(move || move_item(&source_clone, &dest_clone))
            .await
            .map_err(|e| format!("Task failed: {}", e));

        match result {
            Ok(Ok(bytes)) => {
                progress.complete_file(bytes);
                moved_pairs.push((source.clone(), final_dest));
                succeeded += 1;
            }
            Ok(Err(e)) | Err(e) => {
                progress.add_error(OperationError::new(source.clone(), e));
                failed += 1;
            }
        }

        let _ = tx.send(MoveResult::Progress(progress.clone())).await;
    }

    // Send completion
    let _ = tx
        .send(MoveResult::Complete(OperationComplete {
            operation_type: OperationType::Move,
            succeeded,
            failed,
            bytes_processed: progress.bytes_processed,
            errors: progress.errors,
        }))
        .await;
}

/// Move a single item (file or directory).
fn move_item(source: &PathBuf, dest: &PathBuf) -> Result<u64, String> {
    // Get size before move
    let size = get_size(source);

    // Try rename first (fast path for same filesystem)
    if fs::rename(source, dest).is_ok() {
        return Ok(size);
    }

    // Fall back to copy + delete for cross-filesystem moves
    if source.is_dir() {
        copy_dir_recursive(source, dest)?;
        fs::remove_dir_all(source).map_err(|e| format!("Failed to remove source: {}", e))?;
    } else {
        fs::copy(source, dest).map_err(|e| format!("Failed to copy: {}", e))?;
        fs::remove_file(source).map_err(|e| format!("Failed to remove source: {}", e))?;
    }

    Ok(size)
}

/// Get the size of a file or directory.
fn get_size(path: &PathBuf) -> u64 {
    if path.is_dir() {
        get_dir_size(path)
    } else {
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }
}

/// Get the total size of a directory.
fn get_dir_size(dir: &PathBuf) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                size += get_dir_size(&path);
            } else if let Ok(metadata) = fs::metadata(&path) {
                size += metadata.len();
            }
        }
    }
    size
}

/// Recursively copy a directory (for cross-filesystem moves).
fn copy_dir_recursive(source: &PathBuf, dest: &PathBuf) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("Failed to create directory: {}", e))?;

    let entries =
        fs::read_dir(source).map_err(|e| format!("Failed to read directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        let dest_path = dest.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path).map_err(|e| format!("Failed to copy file: {}", e))?;
        }
    }

    Ok(())
}
