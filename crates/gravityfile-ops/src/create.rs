//! File and directory creation operations.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::rename::validate_filename;
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Result sent through the channel during create operations.
#[derive(Debug)]
pub enum CreateResult {
    /// Progress update.
    Progress(OperationProgress),
    /// The operation completed.
    Complete(OperationComplete),
}

/// Start file creation. Dropping the receiver prevents queued work from starting;
/// active filesystem calls are awaited and cannot be interrupted.
pub fn start_create_file(path: PathBuf) -> mpsc::Receiver<CreateResult> {
    start_create(path, false)
}

/// Start directory creation with the same delivery semantics as file creation.
pub fn start_create_directory(path: PathBuf) -> mpsc::Receiver<CreateResult> {
    start_create(path, true)
}

fn start_create(path: PathBuf, directory: bool) -> mpsc::Receiver<CreateResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);
    tokio::spawn(create_impl(path, directory, tx));
    rx
}

async fn create_impl(path: PathBuf, directory: bool, tx: mpsc::Sender<CreateResult>) {
    let operation_type = if directory {
        OperationType::CreateDirectory
    } else {
        OperationType::CreateFile
    };
    match tx.try_reserve() {
        Ok(permit) => {
            let mut progress = OperationProgress::new(operation_type, 1, 0);
            progress.set_current_file(Some(path.clone()));
            permit.send(CreateResult::Progress(progress));
        }
        Err(mpsc::error::TrySendError::Closed(_)) => return,
        Err(mpsc::error::TrySendError::Full(_)) => {}
    }
    let work_path = path.clone();
    let receiver = tx.clone();
    let result = tokio::task::spawn_blocking(move || {
        create_on_worker(&work_path, directory, || !receiver.is_closed())
    })
    .await;
    let (succeeded, failed, cancelled, errors) = match result {
        Ok(Ok(created)) => (usize::from(created), 0, !created, Vec::new()),
        Ok(Err(error)) => (0, 1, false, vec![OperationError::new(path, error)]),
        Err(error) => (
            0,
            1,
            false,
            vec![OperationError::new(path, format!("Task failed: {error}"))],
        ),
    };
    let _ = tx
        .send(CreateResult::Complete(OperationComplete {
            operation_type,
            succeeded,
            failed,
            cancelled,
            skipped: 0,
            bytes_processed: 0,
            errors,
        }))
        .await;
}

/// Returns false if abandoned before final creation. Parent directories already
/// created remain in place; this is not a transaction across the parent chain.
fn create_on_worker(
    path: &std::path::Path,
    directory: bool,
    wanted: impl Fn() -> bool,
) -> Result<bool, String> {
    if !wanted() {
        return Ok(false);
    }
    let name = path
        .file_name()
        .ok_or_else(|| "A filename is required".to_string())?;
    if let Some(name) = name.to_str() {
        validate_filename(name)?;
    }
    match path.symlink_metadata() {
        Ok(_) => {
            return Err(if directory {
                "Directory already exists"
            } else {
                "File already exists"
            }
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect destination: {error}")),
    }
    if !wanted() {
        return Ok(false);
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create parent directory: {error}"))?;
    }
    if !wanted() {
        return Ok(false);
    }
    // Exclusive creation is authoritative, including when another writer wins
    // after the metadata check. Never claim an existing item for undo.
    if directory {
        fs::create_dir(path).map_err(|error| format!("Failed to create directory: {error}"))?;
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("Failed to create file: {error}"))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_progress_queue_does_not_stall_creation() {
        for directory in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("item");
            let (tx, mut rx) = mpsc::channel(1);
            tx.try_send(CreateResult::Progress(OperationProgress::new(
                OperationType::CreateFile,
                1,
                0,
            )))
            .unwrap();
            let work = tokio::spawn(create_impl(path.clone(), directory, tx));
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !path.exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("progress queue stalled creation");
            rx.recv().await.unwrap();
            let CreateResult::Complete(result) = rx.recv().await.unwrap() else {
                panic!("completion expected")
            };
            assert!(result.is_success());
            assert_eq!(result.succeeded, 1);
            assert_eq!(path.is_dir(), directory);
            work.await.unwrap();
        }
    }

    #[tokio::test]
    async fn closed_receiver_prevents_creating_parents_or_item() {
        for directory in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("parent/item");
            let (tx, rx) = mpsc::channel(1);
            drop(rx);
            create_impl(path.clone(), directory, tx).await;
            assert!(!path.parent().unwrap().exists());
        }
    }

    #[test]
    fn abandonment_after_parent_creation_does_not_create_the_item() {
        for directory in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("parent/item");
            let checks = std::cell::Cell::new(0);
            assert!(
                !create_on_worker(&path, directory, || {
                    checks.set(checks.get() + 1);
                    checks.get() < 3
                })
                .unwrap()
            );
            assert!(path.parent().unwrap().is_dir());
            assert!(!path.exists());
        }
    }

    #[test]
    fn concurrent_destination_creation_is_never_claimed_as_success() {
        for directory in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("item");
            let checks = std::cell::Cell::new(0);
            let result = create_on_worker(&path, directory, || {
                checks.set(checks.get() + 1);
                if checks.get() == 3 {
                    if directory {
                        fs::create_dir(&path).unwrap();
                    } else {
                        fs::write(&path, b"other writer").unwrap();
                    }
                }
                true
            });
            assert!(result.is_err());
            if directory {
                assert!(path.is_dir());
            } else {
                assert_eq!(fs::read(path).unwrap(), b"other writer");
            }
        }
    }
}
