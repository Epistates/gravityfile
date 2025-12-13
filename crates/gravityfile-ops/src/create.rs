//! File and directory creation operations.

use std::fs::{self, File};
use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::rename::validate_filename;
use crate::{OperationError, OPERATION_CHANNEL_SIZE};

/// Result sent through the channel during create operations.
#[derive(Debug)]
pub enum CreateResult {
    /// Progress update.
    Progress(OperationProgress),
    /// The operation completed.
    Complete(OperationComplete),
}

/// Start an async file creation operation.
pub fn start_create_file(path: PathBuf) -> mpsc::Receiver<CreateResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    tokio::spawn(async move {
        create_file_impl(path, tx).await;
    });

    rx
}

/// Start an async directory creation operation.
pub fn start_create_directory(path: PathBuf) -> mpsc::Receiver<CreateResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    tokio::spawn(async move {
        create_directory_impl(path, tx).await;
    });

    rx
}

/// Internal implementation of file creation.
async fn create_file_impl(path: PathBuf, tx: mpsc::Sender<CreateResult>) {
    let mut progress = OperationProgress::new(OperationType::CreateFile, 1, 0);
    progress.set_current_file(Some(path.clone()));

    let _ = tx.send(CreateResult::Progress(progress.clone())).await;

    // Validate the filename
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if let Err(e) = validate_filename(name) {
            progress.add_error(OperationError::new(path.clone(), e));
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateFile,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
            return;
        }
    }

    // Check if file already exists
    if path.exists() {
        progress.add_error(OperationError::new(
            path.clone(),
            "File already exists".to_string(),
        ));
        let _ = tx
            .send(CreateResult::Complete(OperationComplete {
                operation_type: OperationType::CreateFile,
                succeeded: 0,
                failed: 1,
                bytes_processed: 0,
                errors: progress.errors,
            }))
            .await;
        return;
    }

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                progress.add_error(OperationError::new(
                    path.clone(),
                    format!("Failed to create parent directory: {}", e),
                ));
                let _ = tx
                    .send(CreateResult::Complete(OperationComplete {
                        operation_type: OperationType::CreateFile,
                        succeeded: 0,
                        failed: 1,
                        bytes_processed: 0,
                        errors: progress.errors,
                    }))
                    .await;
                return;
            }
        }
    }

    // Create the file
    let path_clone = path.clone();
    let result = tokio::task::spawn_blocking(move || File::create(&path_clone))
        .await
        .map_err(|e| format!("Task failed: {}", e));

    match result {
        Ok(Ok(_)) => {
            progress.complete_file(0);
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateFile,
                    succeeded: 1,
                    failed: 0,
                    bytes_processed: 0,
                    errors: vec![],
                }))
                .await;
        }
        Ok(Err(e)) => {
            progress.add_error(OperationError::new(path, format!("Failed to create file: {}", e)));
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateFile,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
        Err(e) => {
            progress.add_error(OperationError::new(path, e));
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateFile,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
    }
}

/// Internal implementation of directory creation.
async fn create_directory_impl(path: PathBuf, tx: mpsc::Sender<CreateResult>) {
    let mut progress = OperationProgress::new(OperationType::CreateDirectory, 1, 0);
    progress.set_current_file(Some(path.clone()));

    let _ = tx.send(CreateResult::Progress(progress.clone())).await;

    // Validate the directory name
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if let Err(e) = validate_filename(name) {
            progress.add_error(OperationError::new(path.clone(), e));
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateDirectory,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
            return;
        }
    }

    // Check if directory already exists
    if path.exists() {
        progress.add_error(OperationError::new(
            path.clone(),
            "Directory already exists".to_string(),
        ));
        let _ = tx
            .send(CreateResult::Complete(OperationComplete {
                operation_type: OperationType::CreateDirectory,
                succeeded: 0,
                failed: 1,
                bytes_processed: 0,
                errors: progress.errors,
            }))
            .await;
        return;
    }

    // Create the directory
    let path_clone = path.clone();
    let result = tokio::task::spawn_blocking(move || fs::create_dir_all(&path_clone))
        .await
        .map_err(|e| format!("Task failed: {}", e));

    match result {
        Ok(Ok(())) => {
            progress.complete_file(0);
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateDirectory,
                    succeeded: 1,
                    failed: 0,
                    bytes_processed: 0,
                    errors: vec![],
                }))
                .await;
        }
        Ok(Err(e)) => {
            progress.add_error(OperationError::new(
                path,
                format!("Failed to create directory: {}", e),
            ));
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateDirectory,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
        Err(e) => {
            progress.add_error(OperationError::new(path, e));
            let _ = tx
                .send(CreateResult::Complete(OperationComplete {
                    operation_type: OperationType::CreateDirectory,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
    }
}
