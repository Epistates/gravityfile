//! Session undo history.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::progress::{OperationComplete, OperationProgress, OperationType, publish_progress};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// An operation that can be reversed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoableOperation {
    /// `(original, current)` locations of moved items.
    FilesMoved { moves: Vec<(PathBuf, PathBuf)> },
    /// Items created by a copy; undo moves them to the Trash.
    FilesCopied { created: Vec<PathBuf> },
    /// Items moved to the Trash at `at`; restorable where the platform
    /// exposes the Trash (Linux, Windows).
    FilesTrashed { paths: Vec<PathBuf>, at: SystemTime },
    /// `(original, current)` paths of renamed items.
    FilesRenamed { renames: Vec<(PathBuf, PathBuf)> },
    /// A file that is removed on undo only while still empty.
    FileCreated { path: PathBuf },
    /// A directory that is removed on undo only while still empty.
    DirectoryCreated { path: PathBuf },
}

impl UndoableOperation {
    /// Number of top-level items in the record.
    pub fn item_count(&self) -> usize {
        match self {
            Self::FilesMoved { moves } => moves.len(),
            Self::FilesCopied { created } => created.len(),
            Self::FilesTrashed { paths, .. } => paths.len(),
            Self::FilesRenamed { renames } => renames.len(),
            Self::FileCreated { .. } | Self::DirectoryCreated { .. } => 1,
        }
    }

    /// Whether undo is possible on this platform.
    pub fn can_undo(&self) -> bool {
        match self {
            Self::FilesTrashed { .. } => TRASH_RESTORE_SUPPORTED,
            _ => true,
        }
    }

    /// What undo will do, for display.
    pub fn undo_description(&self) -> String {
        let count = self.item_count();
        match self {
            Self::FilesMoved { .. } => format!("Move {count} item(s) back"),
            Self::FilesCopied { .. } => format!("Move {count} copied item(s) to the Trash"),
            Self::FilesTrashed { .. } if TRASH_RESTORE_SUPPORTED => {
                format!("Restore {count} item(s) from the Trash")
            }
            Self::FilesTrashed { .. } => {
                format!("{count} item(s) are in the Trash; restore them from Finder")
            }
            Self::FilesRenamed { .. } => format!("Rename {count} item(s) back"),
            Self::FileCreated { .. } => "Remove the new file if still empty".into(),
            Self::DirectoryCreated { .. } => "Remove the new directory if still empty".into(),
        }
    }

    fn operation_type(&self) -> OperationType {
        match self {
            Self::FilesMoved { .. } => OperationType::Move,
            Self::FilesTrashed { .. } => OperationType::Restore,
            Self::FilesRenamed { .. } => OperationType::Rename,
            Self::FilesCopied { .. } | Self::FileCreated { .. } | Self::DirectoryCreated { .. } => {
                OperationType::Delete
            }
        }
    }
}

const TRASH_RESTORE_SUPPORTED: bool = cfg!(any(
    windows,
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
));

/// A recorded operation.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub id: u64,
    pub timestamp: SystemTime,
    pub operation: UndoableOperation,
    pub description: String,
}

impl UndoEntry {
    pub fn new(id: u64, operation: UndoableOperation, description: impl Into<String>) -> Self {
        Self {
            id,
            timestamp: SystemTime::now(),
            operation,
            description: description.into(),
        }
    }
}

/// Bounded, most-recent-last undo history.
#[derive(Debug)]
pub struct UndoLog {
    entries: VecDeque<UndoEntry>,
    max_entries: usize,
    next_id: u64,
}

impl Default for UndoLog {
    fn default() -> Self {
        Self::new(100)
    }
}

impl UndoLog {
    /// A log keeping at most `max_entries` operations; zero disables history.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(max_entries.min(128)),
            max_entries,
            next_id: 0,
        }
    }

    /// Record an operation; empty operations are ignored. Returns its id.
    pub fn record(&mut self, operation: UndoableOperation, description: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        if operation.item_count() > 0 {
            self.push(UndoEntry::new(id, operation, description));
        }
        id
    }

    /// Put an entry back, e.g. the remainder of a partially failed undo.
    pub fn push(&mut self, entry: UndoEntry) {
        if self.max_entries == 0 {
            return;
        }
        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    pub fn record_move(&mut self, moves: Vec<(PathBuf, PathBuf)>) -> u64 {
        let description = format!("Moved {} item(s)", moves.len());
        self.record(UndoableOperation::FilesMoved { moves }, description)
    }

    pub fn record_copy(&mut self, created: Vec<PathBuf>) -> u64 {
        let description = format!("Copied {} item(s)", created.len());
        self.record(UndoableOperation::FilesCopied { created }, description)
    }

    pub fn record_trash(&mut self, paths: Vec<PathBuf>) -> u64 {
        let description = format!("Moved {} item(s) to the Trash", paths.len());
        let at = SystemTime::now();
        self.record(UndoableOperation::FilesTrashed { paths, at }, description)
    }

    pub fn record_renames(&mut self, renames: Vec<(PathBuf, PathBuf)>) -> u64 {
        let description = match renames.as_slice() {
            [(from, to)] => format!("Renamed '{}' to '{}'", file_name(from), file_name(to)),
            _ => format!("Renamed {} item(s)", renames.len()),
        };
        self.record(UndoableOperation::FilesRenamed { renames }, description)
    }

    pub fn record_create_file(&mut self, path: PathBuf) -> u64 {
        let description = format!("Created file '{}'", file_name(&path));
        self.record(UndoableOperation::FileCreated { path }, description)
    }

    pub fn record_create_directory(&mut self, path: PathBuf) -> u64 {
        let description = format!("Created directory '{}'", file_name(&path));
        self.record(UndoableOperation::DirectoryCreated { path }, description)
    }

    /// Remove and return the most recent entry if it can be undone here.
    pub fn pop(&mut self) -> Option<UndoEntry> {
        if self.entries.back()?.operation.can_undo() {
            self.entries.pop_back()
        } else {
            None
        }
    }

    pub fn peek(&self) -> Option<&UndoEntry> {
        self.entries.back()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Entries, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &UndoEntry> {
        self.entries.iter()
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Message sent while an undo runs.
#[derive(Debug)]
pub enum UndoResult {
    Progress(OperationProgress),
    Complete(UndoComplete),
}

/// Completion of an undo.
#[derive(Debug)]
pub struct UndoComplete {
    pub inner: OperationComplete,
    /// The items that could not be undone, ready to [`UndoLog::push`] back so
    /// the user can retry after fixing the cause.
    pub remaining: Option<UndoEntry>,
}

/// Reverse `entry`. Items are processed independently; failures are reported
/// and returned in [`UndoComplete::remaining`].
pub fn execute_undo(entry: UndoEntry, token: CancellationToken) -> mpsc::Receiver<UndoResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);
    tokio::spawn(async move {
        let operation_type = entry.operation.operation_type();
        if token.is_cancelled() {
            // Nothing was attempted: report no failures and hand the whole
            // entry back for a later retry.
            let complete = UndoComplete {
                inner: OperationComplete {
                    cancelled: true,
                    operation_type,
                    succeeded: 0,
                    failed: 0,
                    skipped: 0,
                    bytes_processed: 0,
                    errors: Vec::new(),
                },
                remaining: Some(entry),
            };
            let _ = tx.send(UndoResult::Complete(complete)).await;
            return;
        }
        let mut progress = OperationProgress::new(operation_type, entry.operation.item_count(), 0);
        publish_progress(&tx, || UndoResult::Progress(progress.clone()), &token);
        let work_token = token.clone();
        let work = entry.clone();
        let (left, errors) = tokio::task::spawn_blocking(move || undo_blocking(&work, &work_token))
            .await
            .unwrap_or_else(|error| {
                (
                    Some(entry.operation.clone()),
                    vec![OperationError::new(
                        PathBuf::new(),
                        format!("worker failed: {error}"),
                    )],
                )
            });
        for error in errors {
            progress.add_error(error);
        }
        let failed = left.as_ref().map_or(0, UndoableOperation::item_count);
        const INCOMPLETE: &str = " (undo incomplete)";
        let remaining = left.map(|operation| UndoEntry {
            operation,
            description: format!(
                "{}{INCOMPLETE}",
                entry.description.trim_end_matches(INCOMPLETE)
            ),
            ..entry.clone()
        });
        let complete = UndoComplete {
            inner: OperationComplete {
                cancelled: token.is_cancelled(),
                operation_type,
                succeeded: entry.operation.item_count().saturating_sub(failed),
                failed,
                skipped: 0,
                bytes_processed: 0,
                errors: progress.errors,
            },
            remaining,
        };
        let _ = tx.send(UndoResult::Complete(complete)).await;
    });
    rx
}

/// Returns the part of `entry` that was not undone (if any), plus errors.
fn undo_blocking(
    entry: &UndoEntry,
    token: &CancellationToken,
) -> (Option<UndoableOperation>, Vec<OperationError>) {
    let mut errors = Vec::new();
    let mut fail = |path: &Path, message: String| {
        errors.push(OperationError::new(path.into(), message));
    };
    let all_or_nothing =
        |result: Result<(), String>, path: &Path, fail: &mut dyn FnMut(&Path, String)| {
            result.err().map(|error| {
                fail(path, error);
                entry.operation.clone()
            })
        };
    let remaining = match &entry.operation {
        UndoableOperation::FilesMoved { moves } => {
            let mut left = Vec::new();
            for (original, current) in moves {
                let result = if token.is_cancelled() {
                    Err(crate::fs::CANCELLED.to_string())
                } else {
                    move_back(current, original)
                };
                if let Err(error) = result {
                    fail(current, error);
                    left.push((original.clone(), current.clone()));
                }
            }
            (!left.is_empty()).then_some(UndoableOperation::FilesMoved { moves: left })
        }
        UndoableOperation::FilesCopied { created } => {
            let mut left = Vec::new();
            for path in created {
                let result = if token.is_cancelled() {
                    Err(crate::fs::CANCELLED.to_string())
                } else {
                    crate::delete::trash_context()
                        .delete(path)
                        .map_err(|error| format!("could not move to the Trash: {error}"))
                };
                if let Err(error) = result {
                    fail(path, error);
                    left.push(path.clone());
                }
            }
            (!left.is_empty()).then_some(UndoableOperation::FilesCopied { created: left })
        }
        UndoableOperation::FilesTrashed { paths, at } => all_or_nothing(
            restore_from_trash(paths, *at),
            paths.first().map_or(Path::new(""), PathBuf::as_path),
            &mut fail,
        ),
        UndoableOperation::FilesRenamed { renames } => {
            let reverse: Vec<_> = renames
                .iter()
                .map(|(original, current)| (current.clone(), original.clone()))
                .collect();
            all_or_nothing(
                crate::rename::rename_paths(&reverse, token),
                reverse.first().map_or(Path::new(""), |(path, _)| path),
                &mut fail,
            )
        }
        UndoableOperation::FileCreated { path } => {
            all_or_nothing(remove_created(path, false), path, &mut fail)
        }
        UndoableOperation::DirectoryCreated { path } => {
            all_or_nothing(remove_created(path, true), path, &mut fail)
        }
    };
    (remaining, errors)
}

fn move_back(current: &Path, original: &Path) -> Result<(), String> {
    if let Some(parent) = original.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("cannot recreate parent: {error}"))?;
    }
    match crate::fs::rename_noreplace(current, original) {
        Ok(()) => Ok(()),
        Err(crate::fs::RenameError::CrossDevice) => {
            // The original move crossed filesystems; reverse it the same way.
            let token = CancellationToken::new();
            let bytes = std::sync::atomic::AtomicU64::new(0);
            let mut transfer = crate::transfer::Transfer::new(
                &token,
                &bytes,
                crate::transfer::Preserve {
                    times: true,
                    xattrs: true,
                },
            );
            transfer
                .copy_staged(current, original, None)
                .map_err(|error| error.to_string())?;
            let errors = transfer.into_errors();
            if !errors.is_empty() {
                return Err(format!(
                    "copied back with {} problem(s); '{}' was kept",
                    errors.len(),
                    current.display()
                ));
            }
            crate::fs::remove_path(current).map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn remove_created(path: &Path, directory: bool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if directory {
        if !metadata.is_dir() {
            return Err("replaced by something that is not a directory".into());
        }
        // The filesystem refuses non-empty directories atomically.
        fs::remove_dir(path).map_err(|error| format!("not removed (no longer empty?): {error}"))
    } else {
        if !metadata.is_file() {
            return Err("replaced by something that is not a file".into());
        }
        if metadata.len() != 0 {
            return Err("now contains data; kept".into());
        }
        fs::remove_file(path).map_err(|error| error.to_string())
    }
}

#[cfg(any(
    windows,
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
fn restore_from_trash(paths: &[PathBuf], at: SystemTime) -> Result<(), String> {
    use std::collections::HashMap;
    let since = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64 - 2)
        .unwrap_or(0);
    let wanted: std::collections::HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
    // Newest matching trash item per original path.
    let mut matches: HashMap<PathBuf, trash::TrashItem> = HashMap::new();
    for item in trash::os_limited::list().map_err(|error| error.to_string())? {
        let original = item.original_path();
        if item.time_deleted >= since && wanted.contains(original.as_path()) {
            match matches.get(&original) {
                Some(existing) if existing.time_deleted >= item.time_deleted => {}
                _ => {
                    matches.insert(original, item);
                }
            }
        }
    }
    if matches.len() != paths.len() {
        return Err(format!(
            "found {} of {} item(s) in the Trash",
            matches.len(),
            paths.len()
        ));
    }
    trash::os_limited::restore_all(matches.into_values()).map_err(|error| error.to_string())
}

#[cfg(not(any(
    windows,
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
fn restore_from_trash(_paths: &[PathBuf], _at: SystemTime) -> Result<(), String> {
    Err("restoring from the Trash is not supported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn undo(entry: UndoEntry) -> UndoComplete {
        let mut rx = execute_undo(entry, CancellationToken::new());
        loop {
            if let Some(UndoResult::Complete(complete)) = rx.recv().await {
                return complete;
            }
        }
    }

    #[test]
    fn log_is_bounded_and_ignores_empty_records() {
        let mut log = UndoLog::new(2);
        log.record_copy(Vec::new());
        assert!(log.is_empty());
        for index in 0..3 {
            log.record_create_file(PathBuf::from(format!("f{index}")));
        }
        assert_eq!(log.len(), 2);
        assert_eq!(
            log.pop().unwrap().operation,
            UndoableOperation::FileCreated { path: "f2".into() }
        );
    }

    #[tokio::test]
    async fn partial_move_undo_requeues_only_failed_items() {
        let dir = tempfile::tempdir().unwrap();
        let (a_orig, a_now) = (dir.path().join("a"), dir.path().join("moved/a"));
        let (b_orig, b_now) = (dir.path().join("b"), dir.path().join("moved/b"));
        fs::create_dir(dir.path().join("moved")).unwrap();
        fs::write(&a_now, "a").unwrap();
        fs::write(&b_now, "b").unwrap();
        fs::write(&b_orig, "occupied").unwrap();
        let mut log = UndoLog::default();
        log.record_move(vec![
            (a_orig.clone(), a_now),
            (b_orig.clone(), b_now.clone()),
        ]);
        let complete = undo(log.pop().unwrap()).await;
        assert_eq!((complete.inner.succeeded, complete.inner.failed), (1, 1));
        assert!(a_orig.exists());
        let remaining = complete.remaining.unwrap();
        assert_eq!(
            remaining.operation,
            UndoableOperation::FilesMoved {
                moves: vec![(b_orig.clone(), b_now)]
            }
        );
        fs::remove_file(&b_orig).unwrap();
        let complete = undo(remaining).await;
        assert!(complete.inner.is_success() && complete.remaining.is_none());
        assert_eq!(fs::read_to_string(b_orig).unwrap(), "b");
    }

    #[tokio::test]
    async fn rename_undo_and_created_item_guards() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        fs::write(&new, "x").unwrap();
        let mut log = UndoLog::default();
        log.record_renames(vec![(old.clone(), new.clone())]);
        assert!(undo(log.pop().unwrap()).await.inner.is_success());
        assert!(old.exists() && !new.exists());

        let file = dir.path().join("created");
        fs::write(&file, "data").unwrap();
        log.record_create_file(file.clone());
        let complete = undo(log.pop().unwrap()).await;
        assert_eq!(complete.inner.failed, 1);
        assert!(file.exists());
    }
}
