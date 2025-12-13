//! Async copy operation with progress reporting.

use std::fs;
use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::conflict::{auto_rename_path, Conflict, ConflictKind, ConflictResolution};
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OperationError, OPERATION_CHANNEL_SIZE};

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
        copy_impl(sources, destination, options, tx).await;
    });

    rx
}

/// Internal implementation of copy operation.
async fn copy_impl(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: CopyOptions,
    tx: mpsc::Sender<CopyResult>,
) {
    // First, calculate total size and file count
    let (total_files, total_bytes) = calculate_totals(&sources);

    let mut progress = OperationProgress::new(OperationType::Copy, total_files, total_bytes);
    let global_resolution: Option<ConflictResolution> = options.conflict_resolution;
    let mut succeeded = 0;
    let mut failed = 0;

    // Ensure destination exists and is a directory
    if !destination.exists() {
        if let Err(e) = fs::create_dir_all(&destination) {
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
    }

    for source in sources {
        let dest_path = destination.join(source.file_name().unwrap_or_default());

        // Check for conflicts
        if dest_path.exists() {
            let conflict_kind = if dest_path.is_dir() {
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
                    // Remove existing before copy
                    let _ = if dest_path.is_dir() {
                        fs::remove_dir_all(&dest_path)
                    } else {
                        fs::remove_file(&dest_path)
                    };
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

/// Copy a single item (file or directory).
async fn copy_item(
    source: &PathBuf,
    dest: &PathBuf,
    progress: &mut OperationProgress,
    tx: &mpsc::Sender<CopyResult>,
) -> Result<(), String> {
    let source = source.clone();
    let dest = dest.clone();

    let result = tokio::task::spawn_blocking(move || {
        if source.is_dir() {
            copy_dir_recursive(&source, &dest)
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
fn copy_file(source: &PathBuf, dest: &PathBuf) -> Result<u64, String> {
    let metadata = fs::metadata(source).map_err(|e| format!("Failed to read metadata: {}", e))?;
    let size = metadata.len();

    fs::copy(source, dest).map_err(|e| format!("Failed to copy: {}", e))?;

    Ok(size)
}

/// Recursively copy a directory.
fn copy_dir_recursive(source: &PathBuf, dest: &PathBuf) -> Result<u64, String> {
    fs::create_dir_all(dest).map_err(|e| format!("Failed to create directory: {}", e))?;

    let mut total_bytes = 0u64;

    let entries =
        fs::read_dir(source).map_err(|e| format!("Failed to read directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        let dest_path = dest.join(entry.file_name());

        if path.is_dir() {
            total_bytes += copy_dir_recursive(&path, &dest_path)?;
        } else {
            total_bytes += copy_file(&path, &dest_path)?;
        }
    }

    Ok(total_bytes)
}

/// Calculate total files and bytes for a list of sources.
fn calculate_totals(sources: &[PathBuf]) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0u64;

    for source in sources {
        if source.is_dir() {
            let (f, b) = calculate_dir_totals(source);
            files += f;
            bytes += b;
        } else if let Ok(metadata) = fs::metadata(source) {
            files += 1;
            bytes += metadata.len();
        }
    }

    (files, bytes)
}

/// Calculate totals for a directory recursively.
fn calculate_dir_totals(dir: &PathBuf) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0u64;

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let (f, b) = calculate_dir_totals(&path);
                files += f;
                bytes += b;
            } else if let Ok(metadata) = fs::metadata(&path) {
                files += 1;
                bytes += metadata.len();
            }
        }
    }

    (files, bytes)
}
