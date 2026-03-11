//! Async move operation with progress reporting.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conflict::{Conflict, ConflictKind, ConflictResolution, auto_rename_path};
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
                ConflictResolution::AutoRename => auto_rename_path(&dest_path),
                ConflictResolution::Overwrite | ConflictResolution::OverwriteAll => {
                    // CRIT-3: handle removal failure — record error and skip this source
                    let remove_result = if let Some(ref m) = dest_meta {
                        if m.is_symlink() || !m.is_dir() {
                            fs::remove_file(&dest_path)
                        } else {
                            fs::remove_dir_all(&dest_path)
                        }
                    } else {
                        Ok(())
                    };
                    if let Err(e) = remove_result {
                        progress.add_error(OperationError::new(
                            dest_path.clone(),
                            format!("Failed to remove existing destination: {}", e),
                        ));
                        failed += 1;
                        continue;
                    }
                    dest_path.clone()
                }
            }
        } else {
            dest_path.clone()
        };

        // MED-6: only send progress after the move completes, not before
        let source_clone = source.clone();
        let dest_clone = final_dest.clone();

        let result = tokio::task::spawn_blocking(move || move_item(&source_clone, &dest_clone))
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
fn move_item(source: &PathBuf, dest: &PathBuf) -> Result<u64, String> {
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
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, dest)
                .map_err(|e| format!("Failed to create symlink: {}", e))?;
        }
        #[cfg(windows)]
        {
            if target.is_dir() {
                std::os::windows::fs::symlink_dir(&target, dest)
                    .map_err(|e| format!("Failed to create symlink: {}", e))?;
            } else {
                std::os::windows::fs::symlink_file(&target, dest)
                    .map_err(|e| format!("Failed to create symlink: {}", e))?;
            }
        }
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
fn get_size(path: &PathBuf) -> u64 {
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
fn get_dir_size(dir: &PathBuf) -> u64 {
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

/// Public wrapper for `copy_dir_recursive` used by the undo path in
/// `executor.rs` for cross-filesystem move reversal.
pub fn copy_dir_recursive_pub(
    source: &PathBuf,
    dest: &PathBuf,
    visited: &mut HashSet<u64>,
) -> Result<(), String> {
    copy_dir_recursive(source, dest, visited)
}

/// Recursively copy a directory (for cross-filesystem moves).
///
/// Uses `entry.file_type()` (no symlink-follow), handles symlinks as a distinct
/// branch, and tracks inodes via a visited set to detect loops on Unix.
fn copy_dir_recursive(
    source: &PathBuf,
    dest: &PathBuf,
    visited: &mut HashSet<u64>,
) -> Result<(), String> {
    // Loop detection via inode on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = fs::symlink_metadata(source) {
            let inode = meta.ino();
            if !visited.insert(inode) {
                return Ok(()); // Already visited — break the loop
            }
        }
    }

    fs::create_dir_all(dest).map_err(|e| format!("Failed to create directory: {}", e))?;

    let entries = fs::read_dir(source).map_err(|e| format!("Failed to read directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        // CRIT-2: use entry.file_type() — does NOT follow symlinks
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to read file type: {}", e))?;
        let path = entry.path();
        let dest_path = dest.join(entry.file_name());

        if file_type.is_symlink() {
            // Recreate the symlink at the destination instead of following it
            let target =
                fs::read_link(&path).map_err(|e| format!("Failed to read symlink: {}", e))?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &dest_path)
                    .map_err(|e| format!("Failed to create symlink: {}", e))?;
            }
            #[cfg(windows)]
            {
                // Choose symlink_dir vs symlink_file based on what the
                // original link pointed at, and log failures rather than
                // silently discarding them.
                let result = if path.is_dir() {
                    std::os::windows::fs::symlink_dir(&target, &dest_path)
                } else {
                    std::os::windows::fs::symlink_file(&target, &dest_path)
                };
                if let Err(e) = result {
                    tracing::warn!(
                        "Failed to create symlink {} -> {}: {}",
                        dest_path.display(),
                        target.display(),
                        e
                    );
                }
            }
        } else if file_type.is_dir() {
            copy_dir_recursive(&path, &dest_path, visited)?;
        } else {
            fs::copy(&path, &dest_path).map_err(|e| format!("Failed to copy file: {}", e))?;
        }
    }

    Ok(())
}
