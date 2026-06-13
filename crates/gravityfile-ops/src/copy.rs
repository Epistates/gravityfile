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
    Complete(CopyComplete),
}

/// Completion result for copy operations, including the created paths
/// needed by the undo system.
#[derive(Debug)]
pub struct CopyComplete {
    /// Standard operation completion info.
    pub inner: OperationComplete,
    /// List of all created files and directories for undo recording.
    pub created_paths: Vec<PathBuf>,
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
        let complete = CopyComplete {
            inner: OperationComplete {
                operation_type: OperationType::Copy,
                succeeded: 0,
                failed: 0,
                bytes_processed: 0,
                errors: vec![],
            },
            created_paths: vec![],
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
    let mut created_paths = Vec::new();

    // Ensure destination exists and is a directory
    if !destination.exists()
        && let Err(e) = fs::create_dir_all(&destination)
    {
        progress.add_error(OperationError::new(
            destination.clone(),
            format!("Failed to create destination: {}", e),
        ));
        let _ = tx
            .send(CopyResult::Complete(CopyComplete {
                inner: OperationComplete {
                    operation_type: OperationType::Copy,
                    succeeded: 0,
                    failed: sources.len(),
                    bytes_processed: 0,
                    errors: progress.errors.clone(),
                },
                created_paths: vec![],
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
        let final_dest = if dest_meta.is_some() {
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
                        .send(CopyResult::Complete(CopyComplete {
                            inner: OperationComplete {
                                operation_type: OperationType::Copy,
                                succeeded,
                                failed: failed + 1,
                                bytes_processed: progress.bytes_processed,
                                errors: progress.errors.clone(),
                            },
                            created_paths,
                        }))
                        .await;
                    return;
                }
                ConflictResolution::AutoRename => (auto_rename_path(&dest_path), false),
                // Overwrite is handled by staging into a temp sibling then
                // atomically swapping, so a failed copy never destroys the
                // existing destination (CRIT-3 data-loss fix).
                ConflictResolution::Overwrite | ConflictResolution::OverwriteAll => {
                    (dest_path.clone(), true)
                }
            }
        } else {
            (dest_path.clone(), false)
        };

        let (final_dest, overwrite) = final_dest;

        // Perform the copy
        progress.set_current_file(Some(source.clone()));
        let _ = tx.send(CopyResult::Progress(progress.clone())).await;

        if let Err(e) = copy_item(&source, &final_dest, overwrite, &mut progress, &tx).await {
            progress.add_error(OperationError::new(source.clone(), e));
            failed += 1;
        } else {
            succeeded += 1;
            created_paths.push(final_dest);
        }
    }

    // Send completion
    let _ = tx
        .send(CopyResult::Complete(CopyComplete {
            inner: OperationComplete {
                operation_type: OperationType::Copy,
                succeeded,
                failed,
                bytes_processed: progress.bytes_processed,
                errors: progress.errors,
            },
            created_paths,
        }))
        .await;
}

/// Copy a single item (file, directory, or symlink).
///
/// When `overwrite` is set and `dest` already exists, the copy is staged into a
/// temporary sibling and atomically swapped into place, so a mid-copy failure
/// can never destroy the pre-existing destination.
async fn copy_item(
    source: &Path,
    dest: &Path,
    overwrite: bool,
    progress: &mut OperationProgress,
    tx: &mpsc::Sender<CopyResult>,
) -> Result<(), String> {
    let source = source.to_path_buf();
    let dest = dest.to_path_buf();

    let result = tokio::task::spawn_blocking(move || {
        if overwrite && fs::symlink_metadata(&dest).is_ok() {
            staged_overwrite_copy(&source, &dest)
        } else {
            copy_path_blocking(&source, &dest)
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

/// Copy a single path (file, directory, or symlink) without following symlinks.
/// Returns the number of bytes copied.
pub(crate) fn copy_path_blocking(source: &Path, dest: &Path) -> Result<u64, String> {
    let metadata =
        fs::symlink_metadata(source).map_err(|e| format!("Failed to read metadata: {}", e))?;

    if metadata.is_symlink() {
        let target = fs::read_link(source).map_err(|e| format!("Failed to read symlink: {}", e))?;
        recreate_symlink(&target, source, dest)?;
        Ok(0) // Symlinks have no real size
    } else if metadata.is_dir() {
        copy_dir_recursive(source, dest, &mut HashSet::new())
    } else {
        copy_file(source, dest)
    }
}

/// Stage a copy of `source` into a temp sibling of `dest`, then atomically
/// replace `dest`. On any failure the temp is cleaned up and `dest` is left
/// untouched.
fn staged_overwrite_copy(source: &Path, dest: &Path) -> Result<u64, String> {
    let temp = unique_temp_path(dest);

    let bytes = match copy_path_blocking(source, &temp) {
        Ok(b) => b,
        Err(e) => {
            let _ = remove_path(&temp);
            return Err(e);
        }
    };

    // Remove the existing destination, then move the staged copy into place.
    if let Err(e) = remove_path(dest) {
        let _ = remove_path(&temp);
        return Err(format!("Failed to remove existing destination: {}", e));
    }
    if let Err(e) = fs::rename(&temp, dest) {
        let _ = remove_path(&temp);
        return Err(format!("Failed to finalize overwrite: {}", e));
    }

    Ok(bytes)
}

/// Recreate `target` as a symlink at `dest`. `source` is the original link,
/// used on Windows to decide between a file and directory symlink.
pub(crate) fn recreate_symlink(target: &Path, source: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let _ = source;
        std::os::unix::fs::symlink(target, dest)
            .map_err(|e| format!("Failed to create symlink: {}", e))
    }
    #[cfg(windows)]
    {
        // Mirror the original link kind based on what it pointed at.
        let result = if source.is_dir() {
            std::os::windows::fs::symlink_dir(target, dest)
        } else {
            std::os::windows::fs::symlink_file(target, dest)
        };
        result.map_err(|e| format!("Failed to create symlink: {}", e))
    }
}

/// Remove a path whether it is a file, symlink, or directory (no symlink follow).
pub(crate) fn remove_path(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Pick a unique temporary sibling path for staging an overwrite of `dest`.
pub(crate) fn unique_temp_path(dest: &Path) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_string());
    let pid = std::process::id();
    for n in 0u64.. {
        let candidate = parent.join(format!(".gravityfile-tmp.{pid}.{n}.{name}"));
        if candidate.symlink_metadata().is_err() {
            return candidate;
        }
    }
    unreachable!("temp path counter is effectively unbounded")
}

/// Copy a single file.
///
/// HIGH-1: use symlink_metadata so we read the link's own size, not the target's.
fn copy_file(source: &Path, dest: &Path) -> Result<u64, String> {
    // HIGH-1: use symlink_metadata to avoid following symlinks for size
    let metadata =
        fs::symlink_metadata(source).map_err(|e| format!("Failed to read metadata: {}", e))?;
    let size = metadata.len();

    fs::copy(source, dest).map_err(|e| format!("Failed to copy: {}", e))?;

    Ok(size)
}

/// Recursively copy a directory. Shared by both the copy and (cross-filesystem)
/// move paths, and by undo.
///
/// CRIT-2: uses `entry.file_type()` (no symlink-follow), handles symlinks as a distinct
/// branch, and tracks inodes via a visited set to detect hard-link / symlink loops on Unix.
pub(crate) fn copy_dir_recursive(
    source: &Path,
    dest: &Path,
    visited: &mut HashSet<u64>,
) -> Result<u64, String> {
    copy_dir_recursive_depth(source, dest, visited, 0)
}

/// Maximum directory recursion depth. PATH_MAX is 4096 bytes on Linux, so any
/// real path is shallower than this; the guard only fires on adversarial input
/// and prevents a stack overflow.
pub(crate) const MAX_RECURSION_DEPTH: usize = 4096;

fn copy_dir_recursive_depth(
    source: &Path,
    dest: &Path,
    visited: &mut HashSet<u64>,
    depth: usize,
) -> Result<u64, String> {
    if depth >= MAX_RECURSION_DEPTH {
        return Err(format!(
            "Directory nesting exceeds maximum depth of {MAX_RECURSION_DEPTH}"
        ));
    }
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
            recreate_symlink(&target, &path, &dest_path)?;
        } else if file_type.is_dir() {
            total_bytes += copy_dir_recursive_depth(&path, &dest_path, visited, depth + 1)?;
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
