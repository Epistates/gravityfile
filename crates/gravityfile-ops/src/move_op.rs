//! Async move operation with progress reporting.

use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conflict::{
    Conflict, ConflictKind, ConflictRequest, ConflictResolution, auto_rename_path,
    request_resolution,
};
use crate::copy::{
    RenameError, destination_is_within_directory, remove_path, rename_noreplace, staged_new_copy,
};
use crate::progress::publish_progress;
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Result sent through the channel during move operations.
#[derive(Debug)]
pub enum MoveResult {
    /// Progress update.
    Progress(OperationProgress),
    /// A conflict was detected. The operation is paused until the request is
    /// answered (or dropped, which skips the item).
    Conflict(ConflictRequest),
    /// The operation completed.
    Complete(MoveComplete),
}

/// Completion result for move operations, including the moved pairs
/// needed by the undo system.
#[derive(Debug)]
pub struct MoveComplete {
    /// Standard operation completion info.
    pub inner: OperationComplete,
    /// Pairs of (original_source, final_destination) for undo recording.
    pub moved_pairs: Vec<(PathBuf, PathBuf)>,
}

/// Options for move operations.
#[derive(Debug, Clone, Default)]
pub struct MoveOptions {
    /// How to handle conflicts (None means ask for each).
    pub conflict_resolution: Option<ConflictResolution>,
}

/// Start an async move operation.
///
/// Returns a receiver for progress updates and results.
pub fn start_move(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: MoveOptions,
    token: CancellationToken,
) -> mpsc::Receiver<MoveResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    if sources.is_empty() {
        let complete = MoveComplete {
            inner: OperationComplete {
                cancelled: token.is_cancelled(),
                operation_type: OperationType::Move,
                succeeded: 0,
                failed: 0,
                skipped: 0,
                bytes_processed: 0,
                errors: vec![],
            },
            moved_pairs: vec![],
        };
        tokio::spawn(async move {
            let _ = tx.send(MoveResult::Complete(complete)).await;
        });
        return rx;
    }

    tokio::spawn(async move {
        let receiver_closed = tx.clone();
        let work = move_impl(sources, destination, options, token.clone(), tx);
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = receiver_closed.closed() => {
                token.cancel();
                // Finish cooperative cleanup; dropping the future would detach
                // an in-flight blocking filesystem operation.
                work.await;
            }
            _ = &mut work => {}
        }
    });

    rx
}

/// Internal implementation of move operation.
async fn move_impl(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: MoveOptions,
    token: CancellationToken,
    tx: mpsc::Sender<MoveResult>,
) {
    let total_files = sources.len();
    let mut progress = OperationProgress::new(OperationType::Move, total_files, 0);
    let mut global_resolution: Option<ConflictResolution> = options.conflict_resolution;
    let mut succeeded = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut moved_pairs: Vec<(PathBuf, PathBuf)> = Vec::new();

    if let Some(source) = sources
        .iter()
        .find(|source| destination_is_within_directory(source, &destination))
    {
        progress.add_error(OperationError::new(
            source.clone(),
            format!(
                "Cannot move a directory into itself: destination '{}' is inside the source",
                destination.display()
            ),
        ));
        let _ = tx
            .send(MoveResult::Complete(MoveComplete {
                inner: OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Move,
                    succeeded: 0,
                    failed: sources.len(),
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors,
                },
                moved_pairs,
            }))
            .await;
        return;
    }

    // Ensure destination exists and is a directory
    if !destination.exists()
        && let Err(e) = fs::create_dir_all(&destination)
    {
        progress.add_error(OperationError::new(
            destination.clone(),
            format!("Failed to create destination: {}", e),
        ));
        let _ = tx
            .send(MoveResult::Complete(MoveComplete {
                inner: OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Move,
                    succeeded: 0,
                    failed: sources.len(),
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors.clone(),
                },
                moved_pairs: vec![],
            }))
            .await;
        return;
    }
    if !fs::metadata(&destination).is_ok_and(|metadata| metadata.is_dir()) {
        progress.add_error(OperationError::new(
            destination.clone(),
            "Move destination is not an accessible directory".to_string(),
        ));
        let _ = tx
            .send(MoveResult::Complete(MoveComplete {
                inner: OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Move,
                    succeeded: 0,
                    failed: sources.len(),
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors,
                },
                moved_pairs,
            }))
            .await;
        return;
    }

    for source in sources {
        // HIGH-3: check for cancellation before each item
        if token.is_cancelled() {
            break;
        }

        // MED-5: return error when file_name() is None
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

        // Check for self-move (moving directory into itself)
        if dest_path != source && dest_path.starts_with(&source) {
            let resolution = request_resolution(
                &tx,
                Conflict::source_is_ancestor(source.clone(), dest_path.clone()),
                MoveResult::Conflict,
                &token,
            )
            .await;
            if resolution == ConflictResolution::Abort {
                token.cancel();
                break;
            }
            progress.add_error(OperationError::new(
                source.clone(),
                "Cannot move a directory into itself".to_string(),
            ));
            failed += 1;
            continue;
        }

        // Source and destination resolving to the same path can never be
        // satisfied by any resolution, so it is surfaced and always skipped.
        if source == dest_path {
            let resolution = request_resolution(
                &tx,
                Conflict::new(source.clone(), dest_path.clone(), ConflictKind::SameFile),
                MoveResult::Conflict,
                &token,
            )
            .await;
            if resolution == ConflictResolution::Abort {
                token.cancel();
                break;
            }
            skipped += 1;
            continue;
        }

        // Check for conflicts using symlink_metadata so we see the link itself
        let dest_meta = fs::symlink_metadata(&dest_path).ok();
        let final_dest = if dest_meta.is_some() {
            let conflict_kind = if dest_meta.as_ref().is_some_and(|m| m.is_dir()) {
                ConflictKind::DirectoryExists
            } else {
                ConflictKind::FileExists
            };

            let resolution = match global_resolution {
                Some(res) => res.to_single(),
                None => {
                    let answer = request_resolution(
                        &tx,
                        Conflict::new(source.clone(), dest_path.clone(), conflict_kind),
                        MoveResult::Conflict,
                        &token,
                    )
                    .await;
                    // "…All" answers apply to every remaining conflict.
                    if answer.is_global() {
                        global_resolution = Some(answer);
                    }
                    answer.to_single()
                }
            };

            match resolution {
                ConflictResolution::Skip | ConflictResolution::SkipAll => {
                    skipped += 1;
                    continue;
                }
                ConflictResolution::Abort => {
                    let _ = tx
                        .send(MoveResult::Complete(MoveComplete {
                            inner: OperationComplete {
                                cancelled: true,
                                operation_type: OperationType::Move,
                                succeeded,
                                failed,
                                skipped,
                                bytes_processed: progress.bytes_processed,
                                errors: progress.errors.clone(),
                            },
                            moved_pairs,
                        }))
                        .await;
                    return;
                }
                ConflictResolution::AutoRename => (auto_rename_path(&dest_path), false),
                // Overwrites retain recovery intent and saved paths before
                // moving source or destination content.
                ConflictResolution::Overwrite | ConflictResolution::OverwriteAll => {
                    (dest_path.clone(), true)
                }
            }
        } else {
            (dest_path.clone(), false)
        };

        let (final_dest, overwrite) = final_dest;

        // MED-6: only send progress after the move completes, not before
        let source_clone = source.clone();
        let dest_clone = final_dest.clone();

        let move_token = token.clone();
        let result = tokio::task::spawn_blocking(move || {
            move_item(&source_clone, &dest_clone, overwrite, &move_token)
        })
        .await
        .map_err(|e| format!("Task failed: {}", e));

        match result {
            Ok(Ok(bytes)) => {
                progress.set_current_file(Some(source.clone()));
                progress.complete_file(bytes);
                // Overwrite destroys a pre-existing destination and cannot be
                // represented safely by the current move undo record. Do not
                // offer an undo that would silently erase that prior state.
                if !overwrite {
                    moved_pairs.push((source.clone(), final_dest));
                }
                succeeded += 1;
                publish_progress(&tx, || MoveResult::Progress(progress.clone()), &token);
            }
            Ok(Err(e)) | Err(e) => {
                progress.add_error(OperationError::new(source.clone(), e));
                failed += 1;
            }
        }
    }

    // Send completion with the moved pairs for undo recording
    let _ = tx
        .send(MoveResult::Complete(MoveComplete {
            inner: OperationComplete {
                cancelled: token.is_cancelled(),
                operation_type: OperationType::Move,
                succeeded,
                failed,
                skipped,
                bytes_processed: progress.bytes_processed,
                errors: progress.errors,
            },
            moved_pairs,
        }))
        .await;
}

/// Move a single item (file, directory, or symlink).
///
/// Overwrites record recovery paths before moving source or destination data.
/// A failed operation retains the journal and any unrestored payload/backup.
fn move_item(
    source: &Path,
    dest: &Path,
    overwrite: bool,
    token: &CancellationToken,
) -> Result<u64, String> {
    if token.is_cancelled() {
        return Err(crate::copy::CANCELLED_MESSAGE.to_string());
    }
    if overwrite && fs::symlink_metadata(dest).is_ok() {
        let mut journal = crate::recovery::OverwriteJournal::begin(dest, Some(source))
            .map_err(|error| format!("Failed to reserve overwrite journal: {error}"))?;
        let temp = journal.payload();
        let size = move_to(source, &temp, token).map_err(|error| journal.error(error))?;
        publish_staged_move(&mut journal, source, token)?;
        let recovery_path = journal.path().to_path_buf();
        if let Err(error) = journal.finish() {
            tracing::warn!(path = %recovery_path.display(), %error, "Move overwrite succeeded; recovery cleanup incomplete");
        }
        Ok(size)
    } else {
        move_to(source, dest, token)
    }
}

/// Publication is the commit boundary. Before it, cancellation restores the
/// staged source when possible. Rollback is never cancelled or allowed to clobber.
fn publish_staged_move(
    journal: &mut crate::recovery::OverwriteJournal,
    source: &Path,
    token: &CancellationToken,
) -> Result<(), String> {
    let publication = if token.is_cancelled() {
        Err(crate::copy::CANCELLED_MESSAGE.to_string())
    } else {
        journal.publish()
    };
    if let Err(error) = publication {
        let payload = journal.payload();
        let rollback = match fs::symlink_metadata(&payload) {
            Ok(_) => move_to(&payload, source, &CancellationToken::new()).map(|_| ()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Cannot inspect staged source: {error}")),
        };
        return Err(journal.error(format!("{error}; source rollback: {rollback:?}")));
    }
    Ok(())
}

/// Move `source` to `dest` (which must not exist): rename fast-path, falling
/// back to a symlink-aware copy + delete for cross-filesystem moves.
///
/// The copy + delete fallback is reached **only** for a genuine cross-device
/// rename. Any other rename failure is returned as-is: in particular, a
/// destination that appeared after the caller's conflict check must abort the
/// move rather than fall through to `fs::copy`, which would truncate it.
pub(crate) fn move_to(
    source: &Path,
    dest: &Path,
    token: &CancellationToken,
) -> Result<u64, String> {
    if token.is_cancelled() {
        return Err(crate::copy::CANCELLED_MESSAGE.to_string());
    }
    // HIGH-2: use symlink_metadata so we don't follow symlinks for size
    let size = get_size(source, token);
    if token.is_cancelled() {
        return Err(crate::copy::CANCELLED_MESSAGE.to_string());
    }

    // Try rename first (fast path for same filesystem)
    match rename_noreplace(source, dest) {
        Ok(()) => return Ok(size),
        Err(RenameError::CrossDevice) => {}
        Err(error) => {
            return Err(format!("Failed to move to '{}': {error}", dest.display()));
        }
    }

    cross_device_move(source, dest, token, remove_path)
}

/// Publish the complete copy before removing the source. Removal of a directory
/// may fail after deleting some children: the destination then contains the only
/// surviving copy of those children and must be retained for recovery.
fn cross_device_move(
    source: &Path,
    dest: &Path,
    token: &CancellationToken,
    remove_source: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<u64, String> {
    let bytes = staged_new_copy(source, dest, true, token)?;
    remove_source(source).map_err(|error| format!(
        "Copy completed at '{}', but source removal failed: {error}. Destination retained for recovery; source may be partially removed",
        dest.display()
    ))?;
    Ok(bytes)
}

/// Get the size of a file or directory.
///
/// HIGH-2: use symlink_metadata; skip symlinks.
fn get_size(path: &Path, token: &CancellationToken) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_symlink() => 0,
        Ok(m) if m.is_dir() => get_dir_size(path, token),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Get the total size of a directory.
///
/// HIGH-2: use entry.file_type() and symlink_metadata; skip symlinks.
fn get_dir_size(dir: &Path, token: &CancellationToken) -> u64 {
    let mut size = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if token.is_cancelled() {
            break;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if token.is_cancelled() {
                return size;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                // Skip symlinks in size calculation
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                pending.push(path);
            } else if let Ok(metadata) = fs::symlink_metadata(&path) {
                size = size.saturating_add(metadata.len());
            }
        }
    }
    size
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn cancelled_staged_overwrite_restores_source_and_preserves_destination() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"old").unwrap();
        let mut journal =
            crate::recovery::OverwriteJournal::begin(&destination, Some(&source)).unwrap();
        move_to(&source, &journal.payload(), &CancellationToken::new()).unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let error = publish_staged_move(&mut journal, &source, &token).unwrap_err();
        assert!(error.contains(crate::copy::CANCELLED_MESSAGE));
        assert!(error.contains("source rollback: Ok"));
        assert_eq!(fs::read(&source).unwrap(), b"new");
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        assert!(!journal.payload().exists());
    }

    #[test]
    fn cancelled_staged_overwrite_retains_payload_when_source_is_occupied() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"old").unwrap();
        let mut journal =
            crate::recovery::OverwriteJournal::begin(&destination, Some(&source)).unwrap();
        move_to(&source, &journal.payload(), &CancellationToken::new()).unwrap();
        fs::write(&source, b"concurrent writer").unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let error = publish_staged_move(&mut journal, &source, &token).unwrap_err();
        assert!(error.contains("source rollback: Err"));
        assert!(error.contains(&journal.path().display().to_string()));
        assert_eq!(fs::read(&source).unwrap(), b"concurrent writer");
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        assert_eq!(fs::read(journal.payload()).unwrap(), b"new");
        let transaction = journal.path().to_path_buf();
        drop(journal);
        let record = crate::recovery::inspect_recovery(&transaction).unwrap();
        assert!(record.payload_available);
        assert!(!record.published);
        let recovered = directory.path().join("recovered");
        crate::recovery::restore_payload(&transaction, &recovered).unwrap();
        assert_eq!(fs::read(recovered).unwrap(), b"new");
        assert_eq!(fs::read(&source).unwrap(), b"concurrent writer");
        assert_eq!(fs::read(&destination).unwrap(), b"old");
    }

    #[test]
    fn precancelled_overwrite_does_not_create_a_journal() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"old").unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(move_item(&source, &destination, true, &token).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"new");
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn cancelled_same_filesystem_move_does_not_rename() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"preserve").unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            move_to(&source, &destination, &token).unwrap_err(),
            crate::copy::CANCELLED_MESSAGE
        );
        assert_eq!(fs::read(source).unwrap(), b"preserve");
        assert!(!destination.exists());
    }

    #[test]
    fn cancelled_cross_device_move_preserves_source_and_publishes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let dest = root.path().join("dest");
        fs::write(&source, b"source").unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(
            cross_device_move(&source, &dest, &token, |_| panic!("must not remove source"))
                .is_err()
        );
        assert_eq!(fs::read(source).unwrap(), b"source");
        assert!(!dest.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn partial_source_removal_retains_complete_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let dest = root.path().join("dest");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("first"), b"first").unwrap();
        fs::write(source.join("second"), b"second").unwrap();
        let error = cross_device_move(&source, &dest, &CancellationToken::new(), |path| {
            fs::remove_file(path.join("first"))?;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected failure",
            ))
        })
        .unwrap_err();
        assert!(error.contains("retained for recovery"));
        assert_eq!(fs::read(dest.join("first")).unwrap(), b"first");
        assert_eq!(fs::read(dest.join("second")).unwrap(), b"second");
        assert!(!source.join("first").exists());
    }

    #[test]
    fn cross_device_copy_never_clobbers_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let dest = root.path().join("dest");
        fs::write(&source, b"source").unwrap();
        fs::write(&dest, b"destination").unwrap();
        assert!(
            cross_device_move(&source, &dest, &CancellationToken::new(), |_| panic!(
                "must not remove source"
            ))
            .is_err()
        );
        assert_eq!(fs::read(source).unwrap(), b"source");
        assert_eq!(fs::read(dest).unwrap(), b"destination");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
