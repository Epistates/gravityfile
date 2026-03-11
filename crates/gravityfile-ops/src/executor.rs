//! High-level operation executor with unified result handling.

use std::fs;
use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conflict::ConflictResolution;
use crate::copy::{CopyOptions, CopyResult, start_copy};
use crate::create::{CreateResult, start_create_directory, start_create_file};
use crate::move_op::{MoveOptions, MoveResult, start_move};
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::rename::{RenameResult, start_rename};
use crate::undo::UndoableOperation;
use crate::{Conflict, OPERATION_CHANNEL_SIZE};

/// Unified result type for all operations.
#[derive(Debug)]
pub enum OperationResult {
    /// Progress update.
    Progress(OperationProgress),
    /// A conflict needs resolution.
    Conflict(Conflict),
    /// The operation completed.
    Complete(OperationComplete),
}

/// Executor for file operations with unified interface.
#[derive(Debug, Default)]
pub struct OperationExecutor {
    /// Default conflict resolution.
    pub default_resolution: Option<ConflictResolution>,
    /// Whether to use trash for deletions.
    pub use_trash: bool,
}

impl OperationExecutor {
    /// Create a new executor with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an executor that uses trash for deletions.
    pub fn with_trash() -> Self {
        Self {
            use_trash: true,
            ..Default::default()
        }
    }

    /// Set the default conflict resolution.
    pub fn with_resolution(mut self, resolution: ConflictResolution) -> Self {
        self.default_resolution = Some(resolution);
        self
    }

    /// Execute a copy operation (non-cancellable convenience wrapper).
    ///
    /// Creates an internal `CancellationToken` that is never cancelled.
    /// Use [`copy_with_cancellation`](Self::copy_with_cancellation) when the
    /// caller needs to abort the operation.
    pub fn copy(
        &self,
        sources: Vec<PathBuf>,
        destination: PathBuf,
    ) -> mpsc::Receiver<OperationResult> {
        self.copy_with_cancellation(sources, destination, CancellationToken::new())
    }

    /// Execute a copy operation with cancellation support.
    pub fn copy_with_cancellation(
        &self,
        sources: Vec<PathBuf>,
        destination: PathBuf,
        token: CancellationToken,
    ) -> mpsc::Receiver<OperationResult> {
        let options = CopyOptions {
            conflict_resolution: self.default_resolution,
            preserve_timestamps: true,
        };

        let copy_rx = start_copy(sources, destination, options, token);
        Self::adapt_copy_results(copy_rx)
    }

    /// Execute a move operation (non-cancellable convenience wrapper).
    ///
    /// Creates an internal `CancellationToken` that is never cancelled.
    /// Use [`move_to_with_cancellation`](Self::move_to_with_cancellation)
    /// when the caller needs to abort the operation.
    pub fn move_to(
        &self,
        sources: Vec<PathBuf>,
        destination: PathBuf,
    ) -> mpsc::Receiver<OperationResult> {
        self.move_to_with_cancellation(sources, destination, CancellationToken::new())
    }

    /// Execute a move operation with cancellation support.
    pub fn move_to_with_cancellation(
        &self,
        sources: Vec<PathBuf>,
        destination: PathBuf,
        token: CancellationToken,
    ) -> mpsc::Receiver<OperationResult> {
        let options = MoveOptions {
            conflict_resolution: self.default_resolution,
        };

        let move_rx = start_move(sources, destination, options, token);
        Self::adapt_move_results(move_rx)
    }

    /// Execute a rename operation.
    pub fn rename(&self, source: PathBuf, new_name: String) -> mpsc::Receiver<OperationResult> {
        let rename_rx = start_rename(source, new_name);
        Self::adapt_rename_results(rename_rx)
    }

    /// Execute a file creation operation.
    pub fn create_file(&self, path: PathBuf) -> mpsc::Receiver<OperationResult> {
        let create_rx = start_create_file(path);
        Self::adapt_create_results(create_rx)
    }

    /// Execute a directory creation operation.
    pub fn create_directory(&self, path: PathBuf) -> mpsc::Receiver<OperationResult> {
        let create_rx = start_create_directory(path);
        Self::adapt_create_results(create_rx)
    }

    /// Adapt copy results to unified result type.
    fn adapt_copy_results(mut rx: mpsc::Receiver<CopyResult>) -> mpsc::Receiver<OperationResult> {
        let (tx, result_rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let unified = match result {
                    CopyResult::Progress(p) => OperationResult::Progress(p),
                    CopyResult::Conflict(c) => OperationResult::Conflict(c),
                    CopyResult::Complete(c) => OperationResult::Complete(c),
                };
                if tx.send(unified).await.is_err() {
                    break;
                }
            }
        });

        result_rx
    }

    /// Adapt move results to unified result type.
    ///
    /// Note: `MoveComplete.moved_pairs` is available to callers who receive
    /// the raw `MoveResult` channel (e.g., the TUI for undo recording).
    /// This adapter drops the pairs since `OperationResult` is generic.
    fn adapt_move_results(mut rx: mpsc::Receiver<MoveResult>) -> mpsc::Receiver<OperationResult> {
        let (tx, result_rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let unified = match result {
                    MoveResult::Progress(p) => OperationResult::Progress(p),
                    MoveResult::Conflict(c) => OperationResult::Conflict(c),
                    MoveResult::Complete(mc) => OperationResult::Complete(mc.inner),
                };
                if tx.send(unified).await.is_err() {
                    break;
                }
            }
        });

        result_rx
    }

    /// Adapt rename results to unified result type.
    fn adapt_rename_results(
        mut rx: mpsc::Receiver<RenameResult>,
    ) -> mpsc::Receiver<OperationResult> {
        let (tx, result_rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let unified = match result {
                    RenameResult::Progress(p) => OperationResult::Progress(p),
                    RenameResult::Complete(c) => OperationResult::Complete(c),
                };
                if tx.send(unified).await.is_err() {
                    break;
                }
            }
        });

        result_rx
    }

    /// Adapt create results to unified result type.
    fn adapt_create_results(
        mut rx: mpsc::Receiver<CreateResult>,
    ) -> mpsc::Receiver<OperationResult> {
        let (tx, result_rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let unified = match result {
                    CreateResult::Progress(p) => OperationResult::Progress(p),
                    CreateResult::Complete(c) => OperationResult::Complete(c),
                };
                if tx.send(unified).await.is_err() {
                    break;
                }
            }
        });

        result_rx
    }
}

/// Execute an undo operation.
///
/// Returns a receiver for the undo operation's progress.
pub fn execute_undo(entry: crate::UndoEntry) -> mpsc::Receiver<OperationResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    tokio::spawn(async move {
        match entry.operation {
            UndoableOperation::FilesMoved { moves } => {
                // CRIT-1: Reverse each (original→dest) pair back individually.
                // We rename dest→original for every pair rather than collecting all
                // sources and supplying one destination directory (which was wrong
                // when files came from different parent directories).
                let total = moves.len();
                let mut progress = OperationProgress::new(OperationType::Move, total, 0);
                let mut succeeded = 0usize;
                let mut failed = 0usize;

                for (original, current) in moves {
                    // current is where the file lives now; original is where it should go back
                    progress.set_current_file(Some(current.clone()));
                    let _ = tx.send(OperationResult::Progress(progress.clone())).await;

                    let orig_clone = original.clone();
                    let cur_clone = current.clone();

                    let result = tokio::task::spawn_blocking(move || {
                        // Ensure the original parent directory exists
                        if let Some(parent) = orig_clone.parent() {
                            fs::create_dir_all(parent)
                                .map_err(|e| format!("Failed to create parent directory: {}", e))?;
                        }
                        // Try rename first (fast path, same filesystem).
                        // Fall back to copy+delete for cross-filesystem pairs
                        // (mirrors the forward move_item logic).
                        match fs::rename(&cur_clone, &orig_clone) {
                            Ok(()) => Ok(()),
                            Err(_rename_err) => {
                                let meta = fs::symlink_metadata(&cur_clone)
                                    .map_err(|e| format!("Failed to read metadata: {}", e))?;
                                if meta.is_dir() {
                                    let mut visited = std::collections::HashSet::new();
                                    crate::move_op::copy_dir_recursive_pub(
                                        &cur_clone,
                                        &orig_clone,
                                        &mut visited,
                                    )?;
                                    fs::remove_dir_all(&cur_clone)
                                        .map_err(|e| format!("Failed to remove source: {}", e))
                                } else {
                                    fs::copy(&cur_clone, &orig_clone)
                                        .map_err(|e| format!("Failed to copy: {}", e))?;
                                    fs::remove_file(&cur_clone)
                                        .map_err(|e| format!("Failed to remove source: {}", e))
                                }
                            }
                        }
                    })
                    .await;

                    match result {
                        Ok(Ok(())) => {
                            progress.complete_file(0);
                            succeeded += 1;
                        }
                        Ok(Err(e)) => {
                            progress.add_error(crate::OperationError::new(current, e));
                            failed += 1;
                        }
                        Err(e) => {
                            progress.add_error(crate::OperationError::new(current, e.to_string()));
                            failed += 1;
                        }
                    }
                }

                let _ = tx
                    .send(OperationResult::Complete(OperationComplete {
                        operation_type: OperationType::Move,
                        succeeded,
                        failed,
                        bytes_processed: 0,
                        errors: progress.errors,
                    }))
                    .await;
            }
            UndoableOperation::FilesCopied { created } => {
                // Delete the copied files
                let mut progress = OperationProgress::new(OperationType::Delete, created.len(), 0);
                let mut succeeded = 0;
                let mut failed = 0;

                for path in created {
                    progress.set_current_file(Some(path.clone()));
                    let _ = tx.send(OperationResult::Progress(progress.clone())).await;

                    let result = tokio::task::spawn_blocking(move || {
                        // Use symlink_metadata to check type without following symlinks
                        let metadata = fs::symlink_metadata(&path)?;
                        if metadata.is_symlink() {
                            fs::remove_file(&path)
                        } else if metadata.is_dir() {
                            fs::remove_dir_all(&path)
                        } else {
                            fs::remove_file(&path)
                        }
                    })
                    .await;

                    match result {
                        Ok(Ok(())) => {
                            succeeded += 1;
                            progress.complete_file(0);
                        }
                        _ => {
                            failed += 1;
                        }
                    }
                }

                let _ = tx
                    .send(OperationResult::Complete(OperationComplete {
                        operation_type: OperationType::Delete,
                        succeeded,
                        failed,
                        bytes_processed: 0,
                        errors: progress.errors,
                    }))
                    .await;
            }
            UndoableOperation::FilesDeleted { paths: _ } => {
                let _ = tx
                    .send(OperationResult::Complete(OperationComplete {
                        operation_type: OperationType::Delete,
                        succeeded: 0,
                        failed: 0,
                        bytes_processed: 0,
                        errors: vec![crate::OperationError::new(
                            PathBuf::new(),
                            "Cannot undo trash operations from within the application".to_string(),
                        )],
                    }))
                    .await;
            }
            UndoableOperation::FileRenamed {
                path: current_path,
                old_path,
                new_name: _,
            } => {
                // HIGH-5: use the stored full old_path to determine the original name.
                // `current_path` is where the file lives now (after rename).
                // We rename current_path back to old_path's file_name component.
                let old_name = old_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();

                let mut rename_rx = start_rename(current_path, old_name);

                while let Some(result) = rename_rx.recv().await {
                    let unified = match result {
                        RenameResult::Progress(p) => OperationResult::Progress(p),
                        RenameResult::Complete(c) => OperationResult::Complete(c),
                    };
                    if tx.send(unified).await.is_err() {
                        break;
                    }
                }
            }
            UndoableOperation::FileCreated { path }
            | UndoableOperation::DirectoryCreated { path } => {
                // Delete the created item
                let mut progress = OperationProgress::new(OperationType::Delete, 1, 0);
                progress.set_current_file(Some(path.clone()));
                let _ = tx.send(OperationResult::Progress(progress.clone())).await;

                let path_clone = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let metadata = fs::symlink_metadata(&path_clone)?;
                    if metadata.is_symlink() {
                        fs::remove_file(&path_clone)
                    } else if metadata.is_dir() {
                        fs::remove_dir_all(&path_clone)
                    } else {
                        fs::remove_file(&path_clone)
                    }
                })
                .await;

                let (succeeded, failed) = match result {
                    Ok(Ok(())) => (1, 0),
                    _ => (0, 1),
                };

                let _ = tx
                    .send(OperationResult::Complete(OperationComplete {
                        operation_type: OperationType::Delete,
                        succeeded,
                        failed,
                        bytes_processed: 0,
                        errors: progress.errors,
                    }))
                    .await;
            }
        }
    });

    rx
}
