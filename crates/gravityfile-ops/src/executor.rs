//! High-level operation executor with unified result handling.

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::conflict::ConflictResolution;
use crate::copy::{start_copy, CopyOptions, CopyResult};
use crate::create::{start_create_directory, start_create_file, CreateResult};
use crate::move_op::{start_move, MoveOptions, MoveResult};
use crate::progress::{OperationComplete, OperationProgress};
use crate::rename::{start_rename, RenameResult};
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

    /// Execute a copy operation.
    pub fn copy(
        &self,
        sources: Vec<PathBuf>,
        destination: PathBuf,
    ) -> mpsc::Receiver<OperationResult> {
        let options = CopyOptions {
            conflict_resolution: self.default_resolution,
            preserve_timestamps: true,
        };

        let copy_rx = start_copy(sources, destination, options);
        Self::adapt_copy_results(copy_rx)
    }

    /// Execute a move operation.
    pub fn move_to(
        &self,
        sources: Vec<PathBuf>,
        destination: PathBuf,
    ) -> mpsc::Receiver<OperationResult> {
        let options = MoveOptions {
            conflict_resolution: self.default_resolution,
        };

        let move_rx = start_move(sources, destination, options);
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
    fn adapt_move_results(mut rx: mpsc::Receiver<MoveResult>) -> mpsc::Receiver<OperationResult> {
        let (tx, result_rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

        tokio::spawn(async move {
            while let Some(result) = rx.recv().await {
                let unified = match result {
                    MoveResult::Progress(p) => OperationResult::Progress(p),
                    MoveResult::Conflict(c) => OperationResult::Conflict(c),
                    MoveResult::Complete(c) => OperationResult::Complete(c),
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
                // Reverse the moves
                let reverse_moves: Vec<(PathBuf, PathBuf)> =
                    moves.into_iter().map(|(from, to)| (to, from)).collect();

                let options = MoveOptions {
                    conflict_resolution: Some(ConflictResolution::Overwrite),
                };

                let sources: Vec<PathBuf> = reverse_moves.iter().map(|(from, _)| from.clone()).collect();
                let mut move_rx = start_move(
                    sources,
                    reverse_moves
                        .first()
                        .and_then(|(_, to)| to.parent())
                        .map(|p| p.to_path_buf())
                        .unwrap_or_default(),
                    options,
                );

                while let Some(result) = move_rx.recv().await {
                    let unified = match result {
                        MoveResult::Progress(p) => OperationResult::Progress(p),
                        MoveResult::Conflict(c) => OperationResult::Conflict(c),
                        MoveResult::Complete(c) => OperationResult::Complete(c),
                    };
                    if tx.send(unified).await.is_err() {
                        break;
                    }
                }
            }
            UndoableOperation::FilesCopied { created } => {
                // Delete the copied files
                use crate::progress::OperationType;
                use std::fs;

                let mut progress = OperationProgress::new(OperationType::Delete, created.len(), 0);
                let mut succeeded = 0;
                let mut failed = 0;

                for path in created {
                    progress.set_current_file(Some(path.clone()));
                    let _ = tx.send(OperationResult::Progress(progress.clone())).await;

                    let result = tokio::task::spawn_blocking(move || {
                        if path.is_dir() {
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
            UndoableOperation::FilesDeleted { trash_entries } => {
                // Restore from trash (if possible)
                use crate::progress::OperationType;

                if trash_entries.is_empty() {
                    let _ = tx
                        .send(OperationResult::Complete(OperationComplete {
                            operation_type: OperationType::Move,
                            succeeded: 0,
                            failed: 0,
                            bytes_processed: 0,
                            errors: vec![crate::OperationError::new(
                                PathBuf::new(),
                                "Cannot undo permanent deletion".to_string(),
                            )],
                        }))
                        .await;
                    return;
                }

                // Move files back from trash
                let mut progress = OperationProgress::new(OperationType::Move, trash_entries.len(), 0);
                let mut succeeded = 0;
                let mut failed = 0;

                for (original, trash) in trash_entries {
                    progress.set_current_file(Some(trash.clone()));
                    let _ = tx.send(OperationResult::Progress(progress.clone())).await;

                    let result = tokio::task::spawn_blocking(move || std::fs::rename(&trash, &original))
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
                        operation_type: OperationType::Move,
                        succeeded,
                        failed,
                        bytes_processed: 0,
                        errors: progress.errors,
                    }))
                    .await;
            }
            UndoableOperation::FileRenamed {
                path,
                old_name,
                new_name: _,
            } => {
                // Rename back to old name
                let mut rename_rx = start_rename(path, old_name);

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
            UndoableOperation::FileCreated { path } | UndoableOperation::DirectoryCreated { path } => {
                // Delete the created item
                use crate::progress::OperationType;
                use std::fs;

                let mut progress = OperationProgress::new(OperationType::Delete, 1, 0);
                progress.set_current_file(Some(path.clone()));
                let _ = tx.send(OperationResult::Progress(progress.clone())).await;

                let path_clone = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    if path_clone.is_dir() {
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
