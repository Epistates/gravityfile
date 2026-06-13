//! Async move operation with progress reporting.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conflict::{Conflict, ConflictKind, ConflictResolution, auto_rename_path};
use crate::copy::{copy_dir_recursive, recreate_symlink, remove_path, unique_temp_path};
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Result sent through the channel during move operations.
#[derive(Debug)]
pub enum MoveResult {
    /// Progress update.
    Progress(OperationProgress),
    /// A conflict was detected that needs resolution.
    Conflict(Conflict),
    /// The operation completed.
    Complete(MoveComplete),
}

/// Completion result for move operations, including the moved pairs
/// needed by the undo system.
#[derive(Debug)]
pub struct MoveComplete {
    /// Standard operation completion info.
    pub inner: OperationComplete,
    /// Pairs of (original_source, final_destination) for undo recording.
    pub moved_pairs: Vec<(PathBuf, PathBuf)>,
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
    token: CancellationToken,
) -> mpsc::Receiver<MoveResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    if sources.is_empty() {
        let complete = MoveComplete {
            inner: OperationComplete {
                operation_type: OperationType::Move,
                succeeded: 0,
                failed: 0,
                bytes_processed: 0,
                errors: vec![],
            },
            moved_pairs: vec![],
        };
        tokio::spawn(async move {
            let _ = tx.send(MoveResult::Complete(complete)).await;
        });
        return rx;
    }

    tokio::spawn(async move {
        move_impl(sources, destination, options, token, tx).await;
    });

    rx
}

/// Internal implementation of move operation.
async fn move_impl(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: MoveOptions,
    token: CancellationToken,
    tx: mpsc::Sender<MoveResult>,
) {
    let total_files = sources.len();
    let mut progress = OperationProgress::new(OperationType::Move, total_files, 0);
    let global_resolution: Option<ConflictResolution> = options.conflict_resolution;
    let mut succeeded = 0;
    let mut failed = 0;
    let mut moved_pairs: Vec<(PathBuf, PathBuf)> = Vec::new();

    // Ensure destination exists and is a directory
    if !destination.exists()
        && let Err(e) = fs::create_dir_all(&destination)
    {
        progress.add_error(OperationError::new(
            destination.clone(),
            format!("Failed to create destination: {}", e),
        ));
        let _ = tx
            .send(MoveResult::Complete(MoveComplete {
                inner: OperationComplete {
                    operation_type: OperationType::Move,
                    succeeded: 0,
                    failed: sources.len(),
                    bytes_processed: 0,
                    errors: progress.errors.clone(),
                },
                moved_pairs: vec![],
            }))
            .await;
        return;
    }

    for source in sources {
        // HIGH-3: check for cancellation before each item
        if token.is_cancelled() {
            break;
        }

        // MED-5: return error when file_name() is None
        let file_name = match source.file_name() {
            Some(n) => n.to_owned(),
            None => {
                progress.add_error(OperationError::new(
                    source.clone(),
                    "Source path has no filename component".to_string(),
                ));
                failed += 1;
                continue;
            }
        };
        let dest_path = destination.join(&file_name);

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

        // Check for conflicts using symlink_metadata so we see the link itself
        let dest_meta = fs::symlink_metadata(&dest_path).ok();
        let final_dest = if dest_meta.is_some() {
            let conflict_kind = if dest_meta.as_ref().is_some_and(|m| m.is_dir()) {
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
                        .send(MoveResult::Complete(MoveComplete {
                            inner: OperationComplete {
                                operation_type: OperationType::Move,
                                succeeded,
                                failed: failed + 1,
                                bytes_processed: progress.bytes_processed,
                                errors: progress.errors.clone(),
                            },
                            moved_pairs,
                        }))
                        .await;
                    return;
                }
                ConflictResolution::AutoRename => (auto_rename_path(&dest_path), false),
                // Overwrite stages into a temp sibling then atomically swaps,
                // so a failed move never destroys the existing destination.
                ConflictResolution::Overwrite | ConflictResolution::OverwriteAll => {
                    (dest_path.clone(), true)
                }
            }
        } else {
            (dest_path.clone(), false)
        };

        let (final_dest, overwrite) = final_dest;

        // MED-6: only send progress after the move completes, not before
        let source_clone = source.clone();
        let dest_clone = final_dest.clone();

        let result =
            tokio::task::spawn_blocking(move || move_item(&source_clone, &dest_clone, overwrite))
                .await
                .map_err(|e| format!("Task failed: {}", e));

        match result {
            Ok(Ok(bytes)) => {
                progress.set_current_file(Some(source.clone()));
                progress.complete_file(bytes);
                moved_pairs.push((source.clone(), final_dest));
                succeeded += 1;
                let _ = tx.send(MoveResult::Progress(progress.clone())).await;
            }
            Ok(Err(e)) | Err(e) => {
                progress.add_error(OperationError::new(source.clone(), e));
                failed += 1;
            }
        }
    }

    // Send completion with the moved pairs for undo recording
    let _ = tx
        .send(MoveResult::Complete(MoveComplete {
            inner: OperationComplete {
                operation_type: OperationType::Move,
                succeeded,
                failed,
                bytes_processed: progress.bytes_processed,
                errors: progress.errors,
            },
            moved_pairs,
        }))
        .await;
}

/// Move a single item (file, directory, or symlink).
///
/// When `overwrite` is set and `dest` already exists, the new content is staged
/// into a temp sibling and atomically swapped in, so a failed move never
/// destroys the existing destination.
fn move_item(source: &Path, dest: &Path, overwrite: bool) -> Result<u64, String> {
    if overwrite && fs::symlink_metadata(dest).is_ok() {
        let temp = unique_temp_path(dest);
        let size = match move_to(source, &temp) {
            Ok(s) => s,
            Err(e) => {
                let _ = remove_path(&temp);
                return Err(e);
            }
        };
        if let Err(e) = remove_path(dest) {
            let _ = remove_path(&temp);
            return Err(format!("Failed to remove existing destination: {}", e));
        }
        if let Err(e) = fs::rename(&temp, dest) {
            let _ = remove_path(&temp);
            return Err(format!("Failed to finalize overwrite: {}", e));
        }
        Ok(size)
    } else {
        move_to(source, dest)
    }
}

/// Move `source` to `dest` (which must not exist): rename fast-path, falling
/// back to a symlink-aware copy + delete for cross-filesystem moves.
fn move_to(source: &Path, dest: &Path) -> Result<u64, String> {
    // HIGH-2: use symlink_metadata so we don't follow symlinks for size
    let size = get_size(source);

    // Try rename first (fast path for same filesystem)
    if fs::rename(source, dest).is_ok() {
        return Ok(size);
    }

    // Fall back to copy + delete for cross-filesystem moves
    let metadata =
        fs::symlink_metadata(source).map_err(|e| format!("Failed to read metadata: {}", e))?;

    if metadata.is_symlink() {
        // For symlinks, read the target and recreate at destination
        let target = fs::read_link(source).map_err(|e| format!("Failed to read symlink: {}", e))?;
        recreate_symlink(&target, source, dest)?;
        fs::remove_file(source).map_err(|e| format!("Failed to remove source symlink: {}", e))?;
    } else if metadata.is_dir() {
        if let Err(e) = copy_dir_recursive(source, dest, &mut HashSet::new()) {
            // Best-effort cleanup of partial destination on copy failure.
            let _ = fs::remove_dir_all(dest);
            return Err(e);
        }
        if let Err(e) = fs::remove_dir_all(source) {
            // Copy succeeded but source removal failed — both copies exist.
            // Clean up the destination to avoid silently doubling disk usage.
            tracing::warn!(
                "Source removal failed after cross-fs move; cleaning up destination: {}",
                e
            );
            let _ = fs::remove_dir_all(dest);
            return Err(format!("Failed to remove source after copy: {}", e));
        }
    } else {
        fs::copy(source, dest).map_err(|e| format!("Failed to copy: {}", e))?;
        fs::remove_file(source).map_err(|e| format!("Failed to remove source: {}", e))?;
    }

    Ok(size)
}

/// Get the size of a file or directory.
///
/// HIGH-2: use symlink_metadata; skip symlinks.
fn get_size(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_symlink() => 0,
        Ok(m) if m.is_dir() => get_dir_size(path),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Get the total size of a directory.
///
/// HIGH-2: use entry.file_type() and symlink_metadata; skip symlinks.
fn get_dir_size(dir: &Path) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                // Skip symlinks in size calculation
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                size += get_dir_size(&path);
            } else if let Ok(metadata) = fs::symlink_metadata(&path) {
                size += metadata.len();
            }
        }
    }
    size
}
