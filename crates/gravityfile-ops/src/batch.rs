//! Copy and move of multiple items into a destination directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conflict::{
    Conflict, ConflictKind, ConflictRequest, ConflictResolution, auto_rename_path,
    request_resolution,
};
use crate::fs::{RenameError, is_within_directory, remove_path, rename_noreplace, same_object};
use crate::progress::{OperationComplete, OperationProgress, OperationType, publish_progress};
use crate::transfer::{Preserve, ReplacePolicy, Transfer, dispose};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// How often live byte counts are published while an item is transferring.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Options for copy and move.
#[derive(Debug, Clone, Copy)]
pub struct TransferOptions {
    /// Answer for every conflict; `None` asks through [`TransferResult::Conflict`].
    pub conflict_resolution: Option<ConflictResolution>,
    /// What happens to an existing item the user chose to overwrite.
    pub replace_policy: ReplacePolicy,
    /// Preserve access and modification times.
    pub preserve_timestamps: bool,
    /// Preserve extended attributes (Unix).
    pub preserve_xattrs: bool,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            conflict_resolution: None,
            replace_policy: ReplacePolicy::Trash,
            preserve_timestamps: true,
            preserve_xattrs: true,
        }
    }
}

/// Message sent while a copy or move runs.
#[derive(Debug)]
pub enum TransferResult {
    /// Progress update; best-effort and may be coalesced.
    Progress(OperationProgress),
    /// The operation is paused until the request is answered (dropping it skips).
    Conflict(ConflictRequest),
    /// The operation finished.
    Complete(TransferComplete),
}

/// Completion of a copy or move.
#[derive(Debug)]
pub struct TransferComplete {
    pub inner: OperationComplete,
    /// `(source, destination)` for every item published at a previously free
    /// destination. Overwrites are excluded: they cannot be undone by deleting
    /// or moving the new item back.
    pub completed: Vec<(PathBuf, PathBuf)>,
    /// `(source, destination)` for every item that replaced an existing one.
    pub replaced: Vec<(PathBuf, PathBuf)>,
}

impl TransferComplete {
    /// Every path whose contents the operation changed: destinations, and
    /// sources that a move took away.
    pub fn touched_paths(&self, moved: bool) -> Vec<PathBuf> {
        let mut touched = Vec::new();
        for (source, destination) in self.completed.iter().chain(&self.replaced) {
            if moved {
                touched.push(source.clone());
            }
            touched.push(destination.clone());
        }
        touched
    }
    /// Destinations created by the operation.
    pub fn created_paths(&self) -> impl Iterator<Item = &Path> {
        self.completed
            .iter()
            .map(|(_, destination)| destination.as_path())
    }
}

/// Copy `sources` into the `destination` directory.
pub fn start_copy(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: TransferOptions,
    token: CancellationToken,
) -> mpsc::Receiver<TransferResult> {
    start(Kind::Copy, sources, destination, options, token)
}

/// Move `sources` into the `destination` directory. Same-filesystem moves are
/// a single rename; cross-filesystem moves copy, then delete the source only
/// when every entry arrived.
pub fn start_move(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: TransferOptions,
    token: CancellationToken,
) -> mpsc::Receiver<TransferResult> {
    start(Kind::Move, sources, destination, options, token)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Copy,
    Move,
}

impl Kind {
    fn operation_type(self) -> OperationType {
        match self {
            Self::Copy => OperationType::Copy,
            Self::Move => OperationType::Move,
        }
    }
}

fn start(
    kind: Kind,
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: TransferOptions,
    token: CancellationToken,
) -> mpsc::Receiver<TransferResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);
    tokio::spawn(async move {
        let receiver = tx.clone();
        let work = Batch::new(kind, options, token.clone(), tx).run(sources, destination);
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = receiver.closed() => {
                token.cancel();
                // Let in-flight blocking work observe cancellation and clean up.
                work.await;
            }
            _ = &mut work => {}
        }
    });
    rx
}

struct Batch {
    kind: Kind,
    options: TransferOptions,
    token: CancellationToken,
    tx: mpsc::Sender<TransferResult>,
    progress: OperationProgress,
    global: Option<ConflictResolution>,
    succeeded: usize,
    failed: usize,
    skipped: usize,
    completed: Vec<(PathBuf, PathBuf)>,
    replaced: Vec<(PathBuf, PathBuf)>,
}

enum Plan {
    Skip,
    Abort,
    Publish { destination: PathBuf, replace: bool },
}

impl Batch {
    fn new(
        kind: Kind,
        options: TransferOptions,
        token: CancellationToken,
        tx: mpsc::Sender<TransferResult>,
    ) -> Self {
        Self {
            kind,
            global: options.conflict_resolution,
            options,
            token,
            tx,
            progress: OperationProgress::new(kind.operation_type(), 0, 0),
            succeeded: 0,
            failed: 0,
            skipped: 0,
            completed: Vec::new(),
            replaced: Vec::new(),
        }
    }

    async fn run(mut self, sources: Vec<PathBuf>, destination: PathBuf) {
        if let Err(error) = self.prepare(&sources, &destination).await {
            self.progress.add_error(error);
            self.failed = sources.len();
            return self.finish(false).await;
        }
        for source in sources {
            if self.token.is_cancelled() {
                break;
            }
            match self.plan(&source, &destination).await {
                Plan::Skip => self.skipped += 1,
                Plan::Abort => return self.finish(true).await,
                Plan::Publish {
                    destination,
                    replace,
                } => self.publish(source, destination, replace).await,
            }
        }
        self.finish(false).await
    }

    /// Validate the batch and compute totals before any mutation.
    async fn prepare(
        &mut self,
        sources: &[PathBuf],
        destination: &Path,
    ) -> Result<(), OperationError> {
        if let Some(source) = sources
            .iter()
            .find(|source| is_within_directory(source, destination))
        {
            return Err(OperationError::new(
                source.clone(),
                format!(
                    "cannot {} a directory into itself ('{}' is inside it)",
                    if self.kind == Kind::Copy {
                        "copy"
                    } else {
                        "move"
                    },
                    destination.display()
                ),
            ));
        }
        fs::create_dir_all(destination).map_err(|error| {
            OperationError::new(
                destination.to_path_buf(),
                format!("cannot create destination: {error}"),
            )
        })?;
        if !fs::metadata(destination).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(OperationError::new(
                destination.to_path_buf(),
                "destination is not a directory",
            ));
        }
        self.progress.files_total = sources.len();
        // Moves within a filesystem are renames; walking the sources first
        // would cost far more than the move itself.
        if self.kind == Kind::Copy {
            let sources = sources.to_vec();
            let token = self.token.clone();
            self.progress.bytes_total =
                tokio::task::spawn_blocking(move || total_bytes(&sources, &token))
                    .await
                    .unwrap_or(0);
        }
        Ok(())
    }

    async fn plan(&mut self, source: &Path, destination_dir: &Path) -> Plan {
        let Some(name) = source.file_name() else {
            self.fail(source, "path has no file name");
            return Plan::Skip;
        };
        let destination = destination_dir.join(name);
        if source == destination || same_object(source, &destination) {
            return match self
                .ask(Conflict::new(
                    source.into(),
                    destination,
                    ConflictKind::SameFile,
                ))
                .await
            {
                ConflictResolution::Abort => Plan::Abort,
                _ => Plan::Skip,
            };
        }
        let Ok(existing) = fs::symlink_metadata(&destination) else {
            return Plan::Publish {
                destination,
                replace: false,
            };
        };
        let kind = if existing.is_dir() {
            ConflictKind::DirectoryExists
        } else {
            ConflictKind::FileExists
        };
        let answer = match self.global {
            Some(answer) => answer,
            None => {
                let answer = self
                    .ask(Conflict::new(source.into(), destination.clone(), kind))
                    .await;
                if answer.is_global() {
                    self.global = Some(answer);
                }
                answer
            }
        };
        match answer.to_single() {
            ConflictResolution::Abort => Plan::Abort,
            ConflictResolution::Overwrite => Plan::Publish {
                destination,
                replace: true,
            },
            ConflictResolution::AutoRename => Plan::Publish {
                destination: auto_rename_path(&destination),
                replace: false,
            },
            _ => Plan::Skip,
        }
    }

    async fn ask(&mut self, conflict: Conflict) -> ConflictResolution {
        request_resolution(&self.tx, conflict, TransferResult::Conflict, &self.token).await
    }

    async fn publish(&mut self, source: PathBuf, destination: PathBuf, replace: bool) {
        self.progress.set_current_file(Some(source.clone()));
        self.report();
        let kind = self.kind;
        let options = self.options;
        let token = self.token.clone();
        let bytes = Arc::new(AtomicU64::new(0));
        let job = {
            let (source, destination, bytes) = (source.clone(), destination.clone(), bytes.clone());
            tokio::task::spawn_blocking(move || {
                transfer_item(
                    kind,
                    &source,
                    &destination,
                    replace,
                    options,
                    &token,
                    &bytes,
                )
            })
        };
        let base = self.progress.bytes_processed;
        let outcome = self.wait(job, base, &bytes).await;
        self.progress.bytes_processed = base.saturating_add(bytes.load(Ordering::Relaxed));
        self.progress.files_completed += 1;
        match outcome {
            Ok(ItemOutcome { errors }) => {
                self.succeeded += 1;
                for error in errors {
                    self.progress.add_error(error);
                }
                if replace {
                    self.replaced.push((source, destination));
                } else {
                    self.completed.push((source, destination));
                }
            }
            Err(error) => {
                self.failed += 1;
                self.progress.add_error(error);
            }
        }
        self.report();
    }

    /// Await blocking work, publishing live byte counts meanwhile.
    async fn wait(
        &mut self,
        mut job: tokio::task::JoinHandle<Result<ItemOutcome, OperationError>>,
        base: u64,
        bytes: &AtomicU64,
    ) -> Result<ItemOutcome, OperationError> {
        let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
        ticker.tick().await;
        loop {
            tokio::select! {
                result = &mut job => {
                    return result.unwrap_or_else(|error| {
                        Err(OperationError::new(PathBuf::new(), format!("worker failed: {error}")))
                    });
                }
                _ = ticker.tick() => {
                    self.progress.bytes_processed = base.saturating_add(bytes.load(Ordering::Relaxed));
                    self.report();
                }
            }
        }
    }

    fn report(&self) {
        publish_progress(
            &self.tx,
            || TransferResult::Progress(self.progress.clone()),
            &self.token,
        );
    }

    fn fail(&mut self, path: &Path, message: impl Into<String>) {
        self.failed += 1;
        self.progress
            .add_error(OperationError::new(path.to_path_buf(), message));
    }

    async fn finish(self, aborted: bool) {
        let complete = TransferComplete {
            inner: OperationComplete {
                cancelled: aborted || self.token.is_cancelled(),
                operation_type: self.kind.operation_type(),
                succeeded: self.succeeded,
                failed: self.failed,
                skipped: self.skipped,
                bytes_processed: self.progress.bytes_processed,
                errors: self.progress.errors,
            },
            completed: self.completed,
            replaced: self.replaced,
        };
        let _ = self.tx.send(TransferResult::Complete(complete)).await;
    }
}

struct ItemOutcome {
    /// Non-fatal problems inside the item (skipped entries).
    errors: Vec<OperationError>,
}

fn transfer_item(
    kind: Kind,
    source: &Path,
    destination: &Path,
    replace: bool,
    options: TransferOptions,
    token: &CancellationToken,
    bytes: &AtomicU64,
) -> Result<ItemOutcome, OperationError> {
    let error = |message: String| OperationError::new(source.to_path_buf(), message);
    let preserve = Preserve {
        times: options.preserve_timestamps,
        xattrs: options.preserve_xattrs,
    };
    let replace = replace.then_some(options.replace_policy);

    if kind == Kind::Move {
        if let Some(policy) = replace
            && fs::symlink_metadata(destination).is_ok()
            && !moves_across_devices(source, destination)
        {
            dispose(destination, policy).map_err(|e| error(e.to_string()))?;
        }
        match rename_noreplace(source, destination) {
            Ok(()) => return Ok(ItemOutcome { errors: Vec::new() }),
            Err(RenameError::CrossDevice) => {}
            Err(RenameError::DestinationExists) => {
                return Err(error(format!("'{}' already exists", destination.display())));
            }
            Err(RenameError::Io(e)) => return Err(error(format!("move failed: {e}"))),
        }
    }

    let mut transfer = Transfer::new(token, bytes, preserve);
    transfer
        .copy_staged(source, destination, replace)
        .map_err(|e| error(e.to_string()))?;
    let errors = transfer.into_errors();
    if kind == Kind::Move {
        if !errors.is_empty() {
            let mut errors = errors;
            errors.push(error(
                "some entries could not be copied; the source was kept".to_string(),
            ));
            return Ok(ItemOutcome { errors });
        }
        remove_path(source).map_err(|e| {
            error(format!(
                "copied to '{}', but removing the source failed: {e}",
                destination.display()
            ))
        })?;
    }
    Ok(ItemOutcome { errors })
}

#[cfg(unix)]
fn moves_across_devices(source: &Path, destination: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let parent = crate::fs::parent_or_current(destination);
    match (fs::symlink_metadata(source), fs::metadata(parent)) {
        (Ok(source), Ok(parent)) => source.dev() != parent.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn moves_across_devices(_source: &Path, _destination: &Path) -> bool {
    false
}

/// Total regular-file bytes beneath `sources`, without following symlinks.
fn total_bytes(sources: &[PathBuf], token: &CancellationToken) -> u64 {
    let mut total = 0u64;
    let mut pending: Vec<PathBuf> = sources.to_vec();
    while let Some(path) = pending.pop() {
        if token.is_cancelled() {
            break;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if let Ok(entries) = fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn drain(
        mut rx: mpsc::Receiver<TransferResult>,
        answer: ConflictResolution,
    ) -> TransferComplete {
        while let Some(message) = rx.recv().await {
            match message {
                TransferResult::Progress(_) => {}
                TransferResult::Conflict(request) => request.resolve(answer),
                TransferResult::Complete(complete) => return complete,
            }
        }
        panic!("no completion");
    }

    fn options() -> TransferOptions {
        TransferOptions {
            replace_policy: ReplacePolicy::Permanent,
            ..TransferOptions::default()
        }
    }

    #[tokio::test]
    async fn copy_reports_created_paths_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        fs::write(&source, "hello").unwrap();
        let destination = dir.path().join("out");
        let complete = drain(
            start_copy(
                vec![source.clone()],
                destination.clone(),
                options(),
                CancellationToken::new(),
            ),
            ConflictResolution::Skip,
        )
        .await;
        assert!(complete.inner.is_success(), "{:?}", complete.inner);
        assert_eq!(complete.inner.bytes_processed, 5);
        assert_eq!(
            complete.completed,
            vec![(source.clone(), destination.join("a.txt"))]
        );
        assert!(source.exists());
    }

    #[tokio::test]
    async fn move_renames_and_skips_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("out");
        fs::create_dir(&destination).unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        fs::write(destination.join("b"), "existing").unwrap();
        let complete = drain(
            start_move(
                vec![a.clone(), b.clone()],
                destination.clone(),
                options(),
                CancellationToken::new(),
            ),
            ConflictResolution::Skip,
        )
        .await;
        assert_eq!((complete.inner.succeeded, complete.inner.skipped), (1, 1));
        assert!(!a.exists() && b.exists());
        assert_eq!(
            fs::read_to_string(destination.join("b")).unwrap(),
            "existing"
        );
    }

    #[tokio::test]
    async fn overwrite_and_auto_rename() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("out");
        fs::create_dir(&destination).unwrap();
        let source = dir.path().join("f");
        fs::write(&source, "new").unwrap();
        fs::write(destination.join("f"), "old").unwrap();
        let complete = drain(
            start_copy(
                vec![source.clone()],
                destination.clone(),
                options(),
                CancellationToken::new(),
            ),
            ConflictResolution::AutoRename,
        )
        .await;
        assert_eq!(complete.completed[0].1, destination.join("f (1)"));
        let complete = drain(
            start_copy(
                vec![source.clone()],
                destination.clone(),
                options(),
                CancellationToken::new(),
            ),
            ConflictResolution::Overwrite,
        )
        .await;
        assert!(complete.completed.is_empty());
        assert_eq!(complete.replaced, [(source.clone(), destination.join("f"))]);
        assert_eq!(complete.touched_paths(false), [destination.join("f")]);
        assert_eq!(
            complete.touched_paths(true),
            [source, destination.join("f")]
        );
        assert_eq!(fs::read_to_string(destination.join("f")).unwrap(), "new");
    }

    #[tokio::test]
    async fn refuses_to_copy_a_directory_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("tree");
        fs::create_dir(&source).unwrap();
        let complete = drain(
            start_copy(
                vec![source.clone()],
                source.join("inner"),
                options(),
                CancellationToken::new(),
            ),
            ConflictResolution::Skip,
        )
        .await;
        assert_eq!(complete.inner.failed, 1);
        assert!(!source.join("inner").exists());
    }
}
