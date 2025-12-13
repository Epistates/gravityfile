//! Rename operation.

use std::fs;
use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OperationError, OPERATION_CHANNEL_SIZE};

/// Result sent through the channel during rename operations.
#[derive(Debug)]
pub enum RenameResult {
    /// Progress update.
    Progress(OperationProgress),
    /// The operation completed.
    Complete(OperationComplete),
}

/// Start an async rename operation.
///
/// Renames a single file or directory.
pub fn start_rename(source: PathBuf, new_name: String) -> mpsc::Receiver<RenameResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    tokio::spawn(async move {
        rename_impl(source, new_name, tx).await;
    });

    rx
}

/// Internal implementation of rename operation.
async fn rename_impl(source: PathBuf, new_name: String, tx: mpsc::Sender<RenameResult>) {
    let mut progress = OperationProgress::new(OperationType::Rename, 1, 0);
    progress.set_current_file(Some(source.clone()));

    let _ = tx.send(RenameResult::Progress(progress.clone())).await;

    // Validate the new name
    if let Err(e) = validate_filename(&new_name) {
        progress.add_error(OperationError::new(source.clone(), e));
        let _ = tx
            .send(RenameResult::Complete(OperationComplete {
                operation_type: OperationType::Rename,
                succeeded: 0,
                failed: 1,
                bytes_processed: 0,
                errors: progress.errors,
            }))
            .await;
        return;
    }

    // Construct the new path
    let parent = source.parent().unwrap_or(std::path::Path::new(""));
    let new_path = parent.join(&new_name);

    // Check if target already exists
    if new_path.exists() && new_path != source {
        progress.add_error(OperationError::new(
            source.clone(),
            format!("'{}' already exists", new_name),
        ));
        let _ = tx
            .send(RenameResult::Complete(OperationComplete {
                operation_type: OperationType::Rename,
                succeeded: 0,
                failed: 1,
                bytes_processed: 0,
                errors: progress.errors,
            }))
            .await;
        return;
    }

    // Perform the rename
    let source_clone = source.clone();
    let new_path_clone = new_path.clone();

    let result = tokio::task::spawn_blocking(move || fs::rename(&source_clone, &new_path_clone))
        .await
        .map_err(|e| format!("Task failed: {}", e));

    match result {
        Ok(Ok(())) => {
            progress.complete_file(0);
            let _ = tx
                .send(RenameResult::Complete(OperationComplete {
                    operation_type: OperationType::Rename,
                    succeeded: 1,
                    failed: 0,
                    bytes_processed: 0,
                    errors: vec![],
                }))
                .await;
        }
        Ok(Err(e)) => {
            progress.add_error(OperationError::new(source, format!("Rename failed: {}", e)));
            let _ = tx
                .send(RenameResult::Complete(OperationComplete {
                    operation_type: OperationType::Rename,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
        Err(e) => {
            progress.add_error(OperationError::new(source, e));
            let _ = tx
                .send(RenameResult::Complete(OperationComplete {
                    operation_type: OperationType::Rename,
                    succeeded: 0,
                    failed: 1,
                    bytes_processed: 0,
                    errors: progress.errors,
                }))
                .await;
        }
    }
}

/// Validate a filename for cross-platform compatibility.
pub fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty".into());
    }

    if name.len() > 255 {
        return Err("Name is too long (max 255 characters)".into());
    }

    // Check for invalid characters
    let invalid_chars = ['/', '\0'];
    for c in invalid_chars {
        if name.contains(c) {
            return Err(format!("Name cannot contain '{}'", c));
        }
    }

    // Additional Windows restrictions (good to enforce everywhere for portability)
    #[cfg(target_os = "windows")]
    {
        let windows_invalid = ['\\', ':', '*', '?', '"', '<', '>', '|'];
        for c in windows_invalid {
            if name.contains(c) {
                return Err(format!("Name cannot contain '{}'", c));
            }
        }

        // Check for reserved names
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        let upper_name = name.to_uppercase();
        let base_name = upper_name.split('.').next().unwrap_or("");
        if reserved.contains(&base_name) {
            return Err("Reserved filename".into());
        }
    }

    // Check for leading/trailing spaces or dots (problematic on Windows)
    if name.starts_with(' ') || name.ends_with(' ') {
        return Err("Name cannot start or end with spaces".into());
    }

    if name.ends_with('.') {
        return Err("Name cannot end with a dot".into());
    }

    // Check for . and .. which are reserved
    if name == "." || name == ".." {
        return Err("'.' and '..' are reserved names".into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_filename_valid() {
        assert!(validate_filename("test.txt").is_ok());
        assert!(validate_filename("my-file").is_ok());
        assert!(validate_filename(".hidden").is_ok());
        assert!(validate_filename("file with spaces").is_ok());
    }

    #[test]
    fn test_validate_filename_invalid() {
        assert!(validate_filename("").is_err());
        assert!(validate_filename("test/file").is_err());
        assert!(validate_filename(".").is_err());
        assert!(validate_filename("..").is_err());
        assert!(validate_filename("file ").is_err());
        assert!(validate_filename(" file").is_err());
        assert!(validate_filename("file.").is_err());
    }
}
