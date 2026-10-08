//! Deletion to the system Trash or permanently.

use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::progress::{OperationComplete, OperationProgress, OperationType, publish_progress};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Where deleted items go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeleteMode {
    /// The system Trash / Recycle Bin (recoverable).
    #[default]
    Trash,
    /// Unlinked immediately (not recoverable).
    Permanent,
}

/// Message sent while a delete runs.
#[derive(Debug)]
pub enum DeleteResult {
    Progress(OperationProgress),
    Complete(DeleteComplete),
}

/// Completion of a delete.
#[derive(Debug)]
pub struct DeleteComplete {
    pub inner: OperationComplete,
    /// Items that were removed, in request order.
    pub deleted: Vec<PathBuf>,
    pub mode: DeleteMode,
}

/// Delete `items`, each given as `(path, size)`; the size feeds progress.
/// Symlinks are removed themselves, never their targets.
pub fn start_delete(
    items: Vec<(PathBuf, u64)>,
    mode: DeleteMode,
    token: CancellationToken,
) -> mpsc::Receiver<DeleteResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);
    tokio::spawn(async move {
        let total_bytes = items
            .iter()
            .map(|(_, size)| *size)
            .fold(0u64, u64::saturating_add);
        let mut progress = OperationProgress::new(OperationType::Delete, items.len(), total_bytes);
        let mut deleted = Vec::new();
        let mut failed = 0;
        for (path, size) in items {
            if token.is_cancelled() || tx.is_closed() {
                break;
            }
            progress.set_current_file(Some(path.clone()));
            publish_progress(&tx, || DeleteResult::Progress(progress.clone()), &token);
            let work_path = path.clone();
            let result = tokio::task::spawn_blocking(move || delete_one(&work_path, mode))
                .await
                .unwrap_or_else(|error| Err(format!("worker failed: {error}")));
            match result {
                Ok(()) => {
                    progress.complete_file(size);
                    deleted.push(path);
                }
                Err(error) => {
                    progress.files_completed += 1;
                    progress.add_error(OperationError::new(path, error));
                    failed += 1;
                }
            }
        }
        let complete = DeleteComplete {
            inner: OperationComplete {
                cancelled: token.is_cancelled(),
                operation_type: OperationType::Delete,
                succeeded: deleted.len(),
                failed,
                skipped: 0,
                bytes_processed: progress.bytes_processed,
                errors: progress.errors,
            },
            deleted,
            mode,
        };
        let _ = tx.send(DeleteResult::Complete(complete)).await;
    });
    rx
}

fn delete_one(path: &std::path::Path, mode: DeleteMode) -> Result<(), String> {
    std::fs::symlink_metadata(path).map_err(|error| format!("Cannot access path: {error}"))?;
    match mode {
        DeleteMode::Trash => trash_context()
            .delete(path)
            .map_err(|error| format!("could not move to the Trash: {error}")),
        DeleteMode::Permanent => crate::fs::remove_path(path).map_err(|error| error.to_string()),
    }
}

/// The Trash context used for every trash operation in this crate.
///
/// On macOS the Finder (AppleScript) method needs automation permission and
/// spawns a process per call; `NSFileManager` is fast and prompt-free.
pub(crate) fn trash_context() -> trash::TrashContext {
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut context = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        context.set_delete_method(DeleteMethod::NsFileManager);
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permanent_delete_removes_links_not_targets() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("inner")).unwrap();
        std::fs::write(tree.join("inner/file"), "x").unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        let mut items = vec![(tree.clone(), 1), (dir.path().join("missing"), 0)];
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            items.push((link, 0));
        }
        let mut rx = start_delete(items, DeleteMode::Permanent, CancellationToken::new());
        let complete = loop {
            if let Some(DeleteResult::Complete(complete)) = rx.recv().await {
                break complete;
            }
        };
        assert_eq!(complete.inner.failed, 1);
        assert!(!tree.exists());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
    }
}
