//! Async copy operation with progress reporting.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conflict::{Conflict, ConflictKind, ConflictResolution, auto_rename_path};
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Result sent through the channel during copy operations.
#[derive(Debug)]
pub enum CopyResult {
    /// Progress update.
    Progress(OperationProgress),
    /// A conflict was detected that needs resolution.
    Conflict(Conflict),
    /// The operation completed.
    Complete(OperationComplete),
}

/// Options for copy operations.
#[derive(Debug, Clone, Default)]
pub struct CopyOptions {
    /// How to handle conflicts (None means ask for each).
    pub conflict_resolution: Option<ConflictResolution>,
    /// Whether to preserve timestamps.
    pub preserve_timestamps: bool,
}

/// Start an async copy operation.
///
/// Returns a receiver for progress updates and results.
pub fn start_copy(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: CopyOptions,
    token: CancellationToken,
) -> mpsc::Receiver<CopyResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    if sources.is_empty() {
        // Send immediate completion for empty sources
        let complete = OperationComplete {
            operation_type: OperationType::Copy,
            succeeded: 0,
            failed: 0,
            bytes_processed: 0,
            errors: vec![],
        };
        tokio::spawn(async move {
            let _ = tx.send(CopyResult::Complete(complete)).await;
        });
        return rx;
    }

    tokio::spawn(async move {
        copy_impl(sources, destination, options, token, tx).await;
    });

    rx
}

/// Internal implementation of copy operation.
async fn copy_impl(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: CopyOptions,
    token: CancellationToken,
    tx: mpsc::Sender<CopyResult>,
) {
    // First, calculate total size and file count
    let (total_files, total_bytes) = calculate_totals(&sources);

    let mut progress = OperationProgress::new(OperationType::Copy, total_files, total_bytes);
    let global_resolution: Option<ConflictResolution> = options.conflict_resolution;
    let mut succeeded = 0;
    let mut failed = 0;

    // Ensure destination exists and is a directory
    if !destination.exists()
        && let Err(e) = fs::create_dir_all(&destination)
    {
        progress.add_error(OperationError::new(
            destination.clone(),
            format!("Failed to create destination: {}", e),
        ));
        let _ = tx
            .send(CopyResult::Complete(OperationComplete {
                operation_type: OperationType::Copy,
                succeeded: 0,
                failed: sources.len(),
                bytes_processed: 0,
                errors: progress.errors.clone(),
            }))
            .await;
        return;
    }

    for source in sources {
        // HIGH-3: check for cancellation before each item
        if token.is_cancelled() {
            break;
        }

        // MED-5: return error when file_name() is None (e.g. path is "/" or ends with "..")
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

        // Check for conflicts using symlink_metadata so we see the link itself
        let dest_meta = fs::symlink_metadata(&dest_path).ok();
        if dest_meta.is_some() {
            let conflict_kind = if dest_meta.as_ref().is_some_and(|m| m.is_dir()) {
                ConflictKind::DirectoryExists
            } else {
                ConflictKind::FileExists
            };

            let resolution = if let Some(res) = global_resolution {
                res.to_single()
            } else {
                // Send conflict and wait (in real impl, would need response channel)
                // For now, default to skip
                let _ = tx
                    .send(CopyResult::Conflict(Conflict::new(
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
                        .send(CopyResult::Complete(OperationComplete {
                            operation_type: OperationType::Copy,
                            succeeded,
                            failed: failed + 1,
                            bytes_processed: progress.bytes_processed,
                            errors: progress.errors.clone(),
                        }))
                        .await;
                    return;
                }
                ConflictResolution::AutoRename => {
                    let new_dest = auto_rename_path(&dest_path);
                    if let Err(e) = copy_item(&source, &new_dest, &mut progress, &tx).await {
                        progress.add_error(OperationError::new(source.clone(), e));
                        failed += 1;
                    } else {
                        succeeded += 1;
                    }
                    continue;
                }
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
                }
            }
        }

        // Perform the copy
        progress.set_current_file(Some(source.clone()));
        let _ = tx.send(CopyResult::Progress(progress.clone())).await;

        if let Err(e) = copy_item(&source, &dest_path, &mut progress, &tx).await {
            progress.add_error(OperationError::new(source.clone(), e));
            failed += 1;
        } else {
            succeeded += 1;
        }
    }

    // Send completion
    let _ = tx
        .send(CopyResult::Complete(OperationComplete {
            operation_type: OperationType::Copy,
            succeeded,
            failed,
            bytes_processed: progress.bytes_processed,
            errors: progress.errors,
        }))
        .await;
}

/// Copy a single item (file, directory, or symlink).
async fn copy_item(
    source: &Path,
    dest: &Path,
    progress: &mut OperationProgress,
    tx: &mpsc::Sender<CopyResult>,
) -> Result<(), String> {
    let source = source.to_path_buf();
    let dest = dest.to_path_buf();

    let result = tokio::task::spawn_blocking(move || {
        // Use symlink_metadata to avoid following symlinks
        let metadata =
            fs::symlink_metadata(&source).map_err(|e| format!("Failed to read metadata: {}", e))?;

        if metadata.is_symlink() {
            // For symlinks, read the target and recreate at destination
            let target =
                fs::read_link(&source).map_err(|e| format!("Failed to read symlink: {}", e))?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &dest)
                    .map_err(|e| format!("Failed to create symlink: {}", e))?;
            }
            #[cfg(windows)]
            {
                if target.is_dir() {
                    std::os::windows::fs::symlink_dir(&target, &dest)
                        .map_err(|e| format!("Failed to create symlink: {}", e))?;
                } else {
                    std::os::windows::fs::symlink_file(&target, &dest)
                        .map_err(|e| format!("Failed to create symlink: {}", e))?;
                }
            }
            Ok(0u64) // Symlinks have no real size
        } else if metadata.is_dir() {
            copy_dir_recursive(&source, &dest, &mut HashSet::new())
        } else {
            copy_file(&source, &dest)
        }
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?;

    match result {
        Ok(bytes) => {
            progress.complete_file(bytes);
            let _ = tx.send(CopyResult::Progress(progress.clone())).await;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Copy a single file.
///
/// HIGH-1: use symlink_metadata so we read the link's own size, not the target's.
fn copy_file(source: &PathBuf, dest: &PathBuf) -> Result<u64, String> {
    // HIGH-1: use symlink_metadata to avoid following symlinks for size
    let metadata =
        fs::symlink_metadata(source).map_err(|e| format!("Failed to read metadata: {}", e))?;
    let size = metadata.len();

    fs::copy(source, dest).map_err(|e| format!("Failed to copy: {}", e))?;

    Ok(size)
}

/// Recursively copy a directory.
///
/// CRIT-2: uses `entry.file_type()` (no symlink-follow), handles symlinks as a distinct
/// branch, and tracks inodes via a visited set to detect hard-link / symlink loops on Unix.
fn copy_dir_recursive(
    source: &PathBuf,
    dest: &PathBuf,
    visited: &mut HashSet<u64>,
) -> Result<u64, String> {
    // Loop detection via inode on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = fs::symlink_metadata(source) {
            let inode = meta.ino();
            if !visited.insert(inode) {
                // Already visited this inode — skip to break the loop
                return Ok(0);
            }
        }
    }

    fs::create_dir_all(dest).map_err(|e| format!("Failed to create directory: {}", e))?;

    let mut total_bytes = 0u64;

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
            // Recreate the symlink rather than following it
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
                // original link pointed at, and log failures.
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
            total_bytes += copy_dir_recursive(&path, &dest_path, visited)?;
        } else {
            total_bytes += copy_file(&path, &dest_path)?;
        }
    }

    Ok(total_bytes)
}

/// Calculate total files and bytes for a list of sources.
///
/// HIGH-2: use symlink_metadata; skip symlinks in size calculations.
fn calculate_totals(sources: &[PathBuf]) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0u64;

    for source in sources {
        match fs::symlink_metadata(source) {
            Ok(meta) if meta.is_dir() => {
                let (f, b) = calculate_dir_totals(source);
                files += f;
                bytes += b;
            }
            Ok(meta) if !meta.is_symlink() => {
                files += 1;
                bytes += meta.len();
            }
            _ => {} // skip symlinks and inaccessible entries
        }
    }

    (files, bytes)
}

/// Calculate totals for a directory recursively.
///
/// HIGH-2: use symlink_metadata; skip symlinks.
fn calculate_dir_totals(dir: &PathBuf) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0u64;

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            // Use entry.file_type() — does not follow symlinks
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                // Skip symlinks in size calculation
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                let (f, b) = calculate_dir_totals(&path);
                files += f;
                bytes += b;
            } else if let Ok(metadata) = fs::symlink_metadata(&path) {
                files += 1;
                bytes += metadata.len();
            }
        }
    }

    (files, bytes)
}
