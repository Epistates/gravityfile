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
use crate::{ConflictRequest, OPERATION_CHANNEL_SIZE};

/// Unified result type for all operations.
#[derive(Debug)]
pub enum OperationResult {
    /// Progress update.
    Progress(OperationProgress),
    /// A conflict needs resolution. The operation stays paused until the
    /// request is answered or dropped.
    Conflict(ConflictRequest),
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

    /// Adapt copy results to unified result type. Raw created paths remain
    /// available through `CopyResult` for callers that record undo information.
    fn adapt_copy_results(rx: mpsc::Receiver<CopyResult>) -> mpsc::Receiver<OperationResult> {
        adapt_results(rx, |result| match result {
            CopyResult::Progress(p) => OperationResult::Progress(p),
            CopyResult::Conflict(c) => OperationResult::Conflict(c),
            CopyResult::Complete(c) => OperationResult::Complete(c.inner),
        })
    }

    /// Raw moved pairs remain available through `MoveResult` for undo recording.
    fn adapt_move_results(rx: mpsc::Receiver<MoveResult>) -> mpsc::Receiver<OperationResult> {
        adapt_results(rx, |result| match result {
            MoveResult::Progress(p) => OperationResult::Progress(p),
            MoveResult::Conflict(c) => OperationResult::Conflict(c),
            MoveResult::Complete(c) => OperationResult::Complete(c.inner),
        })
    }

    fn adapt_rename_results(rx: mpsc::Receiver<RenameResult>) -> mpsc::Receiver<OperationResult> {
        adapt_results(rx, |result| match result {
            RenameResult::Progress(p) => OperationResult::Progress(p),
            RenameResult::Complete(c) => OperationResult::Complete(c),
        })
    }

    fn adapt_create_results(rx: mpsc::Receiver<CreateResult>) -> mpsc::Receiver<OperationResult> {
        adapt_results(rx, |result| match result {
            CreateResult::Progress(p) => OperationResult::Progress(p),
            CreateResult::Complete(c) => OperationResult::Complete(c),
        })
    }
}

/// Propagate downstream closure even while a worker has no result to send.
/// Dropping its receiver lets the worker observe abandonment at its next check.
fn adapt_results<T: Send + 'static>(
    mut rx: mpsc::Receiver<T>,
    convert: fn(T) -> OperationResult,
) -> mpsc::Receiver<OperationResult> {
    let (tx, result_rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);
    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                biased;
                _ = tx.closed() => break,
                result = rx.recv() => match result {
                    Some(result) => result,
                    None => break,
                },
            };
            if tx.send(convert(result)).await.is_err() {
                break;
            }
        }
    });
    result_rx
}

/// Execute an undo operation.
///
/// Returns a receiver for the undo operation's progress.
pub fn execute_undo(entry: crate::UndoEntry) -> mpsc::Receiver<OperationResult> {
    execute_undo_with_cancellation(entry, CancellationToken::new())
}

/// Execute undo with cooperative cancellation. Dropping the receiver cancels
/// further work. Active filesystem calls are awaited; they cannot be interrupted.
/// Progress is best-effort; callers must drain the reliable completion result.
pub fn execute_undo_with_cancellation(
    entry: crate::UndoEntry,
    token: CancellationToken,
) -> mpsc::Receiver<OperationResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);
    let monitor = tx.clone();
    let work_token = token.clone();
    let mut work = tokio::spawn(undo_impl(entry, tx, work_token));
    tokio::spawn(async move {
        tokio::select! {
            _ = monitor.closed() => {
                token.cancel();
                let _ = work.await;
            }
            _ = &mut work => {}
        }
    });
    rx
}

async fn undo_impl(
    entry: crate::UndoEntry,
    tx: mpsc::Sender<OperationResult>,
    token: CancellationToken,
) {
    if entry.review_required {
        let _ = tx
            .send(OperationResult::Complete(OperationComplete {
                operation_type: match &entry.operation {
                    UndoableOperation::FilesMoved { .. } => OperationType::Move,
                    UndoableOperation::FilesCopied { .. } => OperationType::Retain,
                    UndoableOperation::FileRenamed { .. } => OperationType::Rename,
                    _ => OperationType::Delete,
                },
                cancelled: false,
                succeeded: 0,
                failed: 1,
                skipped: 0,
                bytes_processed: 0,
                errors: vec![crate::OperationError::new(
                    PathBuf::new(),
                    "Incomplete undo requires manual review; automatic replay refused",
                )],
            }))
            .await;
        return;
    }
    if tx.is_closed() {
        token.cancel();
    }
    if token.is_cancelled() {
        let operation_type = match &entry.operation {
            UndoableOperation::FilesMoved { .. } => OperationType::Move,
            UndoableOperation::FilesCopied { .. } => OperationType::Retain,
            UndoableOperation::FileRenamed { .. } => OperationType::Rename,
            _ => OperationType::Delete,
        };
        let _ = tx
            .send(OperationResult::Complete(OperationComplete {
                operation_type,
                cancelled: true,
                succeeded: 0,
                failed: 0,
                skipped: 0,
                bytes_processed: 0,
                errors: Vec::new(),
            }))
            .await;
        return;
    }
    let created_directory = matches!(&entry.operation, UndoableOperation::DirectoryCreated { .. });
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
                crate::progress::publish_progress(
                    &tx,
                    || OperationResult::Progress(progress.clone()),
                    &token,
                );
                if token.is_cancelled() {
                    break;
                }

                let orig_clone = original.clone();
                let cur_clone = current.clone();

                let work_token = token.clone();
                let result = tokio::task::spawn_blocking(move || {
                    if work_token.is_cancelled() {
                        return Err("Undo cancelled".to_string());
                    }
                    // Ensure the original parent directory exists
                    if let Some(parent) = orig_clone.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("Failed to create parent directory: {}", e))?;
                    }
                    // Undo uses the same no-clobber publication and source
                    // recovery behavior as a forward move.
                    crate::move_op::move_to(&cur_clone, &orig_clone, &work_token).map(|_| ())
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
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Move,
                    succeeded,
                    failed,
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
        UndoableOperation::FilesCopied { created } => {
            // Retain destinations in recovery journals, including later edits.
            let mut progress = OperationProgress::new(OperationType::Retain, created.len(), 0);
            let mut succeeded = 0;
            let mut failed = 0;

            for path in created {
                progress.set_current_file(Some(path.clone()));
                crate::progress::publish_progress(
                    &tx,
                    || OperationResult::Progress(progress.clone()),
                    &token,
                );
                if token.is_cancelled() {
                    break;
                }

                let error_path = path.clone();
                let work_token = token.clone();
                let result = tokio::task::spawn_blocking(move || {
                    if work_token.is_cancelled() {
                        return Err("Undo cancelled".to_string());
                    }
                    crate::recovery::retain_copy_undo(&path)
                })
                .await;

                match result {
                    Ok(Ok(())) => {
                        succeeded += 1;
                        progress.complete_file(0);
                    }
                    Ok(Err(error)) => {
                        progress
                            .add_error(crate::OperationError::new(error_path, error.to_string()));
                        failed += 1;
                    }
                    Err(error) => {
                        progress
                            .add_error(crate::OperationError::new(error_path, error.to_string()));
                        failed += 1;
                    }
                }
            }

            let _ = tx
                .send(OperationResult::Complete(OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Retain,
                    succeeded,
                    failed,
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
        UndoableOperation::FilesDeleted { paths: _ } => {
            let _ = tx
                .send(OperationResult::Complete(OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Delete,
                    succeeded: 0,
                    failed: 1,
                    skipped: 0,
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
            let work_token = token.clone();
            let error_path = current_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::rename_paths_with_cancellation(&[(current_path, old_path)], &work_token)
            })
            .await;
            let (succeeded, failed, errors) = match result {
                Ok(Ok(())) => (1, 0, Vec::new()),
                Ok(Err(error)) => (0, 1, vec![crate::OperationError::new(error_path, error)]),
                Err(error) => (
                    0,
                    1,
                    vec![crate::OperationError::new(error_path, error.to_string())],
                ),
            };
            let _ = tx
                .send(OperationResult::Complete(OperationComplete {
                    operation_type: OperationType::Rename,
                    cancelled: token.is_cancelled(),
                    succeeded,
                    failed,
                    skipped: 0,
                    bytes_processed: 0,
                    errors,
                }))
                .await;
        }
        UndoableOperation::FileCreated { path } | UndoableOperation::DirectoryCreated { path } => {
            // Delete the created item
            let mut progress = OperationProgress::new(OperationType::Delete, 1, 0);
            progress.set_current_file(Some(path.clone()));
            crate::progress::publish_progress(
                &tx,
                || OperationResult::Progress(progress.clone()),
                &token,
            );

            let path_clone = path.clone();
            let work_token = token.clone();
            let result = tokio::task::spawn_blocking(move || {
                if work_token.is_cancelled() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Undo cancelled",
                    ));
                }
                let metadata = fs::symlink_metadata(&path_clone)?;
                if created_directory {
                    if !metadata.is_dir() {
                        return Err(std::io::Error::other(
                            "Created directory was replaced by another file type",
                        ));
                    }
                    // Directory creation did not create these later contents.
                    // Let the filesystem atomically refuse a nonempty directory.
                    fs::remove_dir(&path_clone)
                } else {
                    if !metadata.is_file() {
                        return Err(std::io::Error::other(
                            "Created file was replaced by another file type",
                        ));
                    }
                    if metadata.len() != 0 {
                        return Err(std::io::Error::other(
                            "Created file now contains data; refusing to delete it",
                        ));
                    }
                    fs::remove_file(&path_clone)
                }
            })
            .await;

            let (succeeded, failed) = match result {
                Ok(Ok(())) => (1, 0),
                Ok(Err(error)) => {
                    progress.add_error(crate::OperationError::new(path, error.to_string()));
                    (0, 1)
                }
                Err(error) => {
                    progress.add_error(crate::OperationError::new(path, error.to_string()));
                    (0, 1)
                }
            };

            let _ = tx
                .send(OperationResult::Complete(OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Delete,
                    succeeded,
                    failed,
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
    }
}

#[cfg(test)]
mod undo_delivery_tests {
    use super::*;

    #[tokio::test]
    async fn undo_completion_keeps_failure_totals_when_details_are_omitted() {
        let fixture = tempfile::tempdir().unwrap();
        let entry = crate::UndoEntry::new(
            1,
            UndoableOperation::FilesCopied {
                created: (0..1100)
                    .map(|index| fixture.path().join(format!("missing-{index}")))
                    .collect(),
            },
            "copied",
        );
        let mut output = execute_undo(entry);
        let completion = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(result) = output.recv().await {
                if let OperationResult::Complete(completion) = result {
                    return completion;
                }
            }
            panic!("missing completion");
        })
        .await
        .unwrap();
        assert_eq!(completion.failed, 1100);
        assert_eq!(completion.succeeded, 0);
        assert_eq!(completion.errors.len(), 1000);
        assert_eq!(completion.errors.last().unwrap().omitted_count, 101);
        assert!(!completion.is_success());
    }

    #[tokio::test]
    async fn review_required_undo_is_not_executed_by_public_api() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("retained");
        fs::write(&path, "").unwrap();
        let mut entry = crate::UndoEntry::new(
            1,
            UndoableOperation::FileCreated { path: path.clone() },
            "created",
        );
        entry.review_required = true;
        let mut output = execute_undo(entry);
        let Some(OperationResult::Complete(result)) = output.recv().await else {
            panic!("completion expected");
        };
        assert_eq!(result.succeeded, 0);
        assert_eq!(result.failed, 1);
        assert!(result.errors[0].message.contains("manual review"));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn all_executor_adapters_close_idle_worker_receivers() {
        async fn check<T: Send + 'static>(
            adapt: fn(mpsc::Receiver<T>) -> mpsc::Receiver<OperationResult>,
        ) {
            let (worker, results) = mpsc::channel(1);
            let output = adapt(results);
            // Keep the worker alive without emitting any event. Closure must
            // propagate without relying on progress or completion arriving.
            drop(output);
            tokio::time::timeout(std::time::Duration::from_secs(5), worker.closed())
                .await
                .expect("adapter retained an idle worker receiver");
        }
        check(OperationExecutor::adapt_copy_results).await;
        check(OperationExecutor::adapt_move_results).await;
        check(OperationExecutor::adapt_rename_results).await;
        check(OperationExecutor::adapt_create_results).await;
    }

    #[tokio::test]
    async fn executor_adapter_preserves_terminal_diagnostics_and_counts() {
        let (worker, results) = mpsc::channel(1);
        let mut output = OperationExecutor::adapt_create_results(results);
        let path = PathBuf::from("failed-path");
        worker
            .send(CreateResult::Complete(OperationComplete {
                operation_type: OperationType::CreateFile,
                cancelled: true,
                succeeded: 2,
                failed: 1,
                skipped: 3,
                bytes_processed: 42,
                errors: vec![crate::OperationError::new(path.clone(), "failure")],
            }))
            .await
            .unwrap();
        drop(worker);
        let Some(OperationResult::Complete(result)) = output.recv().await else {
            panic!("completion expected")
        };
        assert_eq!(
            (
                result.succeeded,
                result.failed,
                result.skipped,
                result.bytes_processed
            ),
            (2, 1, 3, 42)
        );
        assert!(result.cancelled);
        assert_eq!(result.errors[0].path, path);
        assert_eq!(result.errors[0].message, "failure");
        assert!(output.recv().await.is_none());
    }

    #[tokio::test]
    async fn closed_undo_receiver_prevents_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keep");
        fs::write(&path, []).unwrap();
        let entry = crate::UndoEntry::new(
            0,
            UndoableOperation::FileCreated { path: path.clone() },
            "create",
        );
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let token = CancellationToken::new();
        undo_impl(entry, tx, token.clone()).await;
        assert!(token.is_cancelled());
        assert!(path.exists());
    }

    #[tokio::test]
    async fn full_undo_progress_queue_does_not_stall_work() {
        let directory = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|index| directory.path().join(index.to_string()))
            .collect();
        for path in &paths {
            fs::write(path, []).unwrap();
        }
        let entry = crate::UndoEntry::new(
            0,
            UndoableOperation::FilesCopied {
                created: paths.clone(),
            },
            "copy",
        );
        let (tx, mut rx) = mpsc::channel(1);
        let work = tokio::spawn(undo_impl(entry, tx, CancellationToken::new()));
        // Leave progress unread. All filesystem work must finish despite the
        // one-slot queue; only the reliable completion may wait for a reader.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while paths.last().unwrap().exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("progress delivery stalled undo");
        let mut complete = None;
        while let Some(result) = rx.recv().await {
            if let OperationResult::Complete(result) = result {
                complete = Some(result);
            }
        }
        work.await.unwrap();
        assert_eq!(complete.unwrap().succeeded, 4);
    }
}
