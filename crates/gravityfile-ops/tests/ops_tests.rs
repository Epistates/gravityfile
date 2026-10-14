use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use filetime::FileTime;
use gravityfile_ops::{
    ConflictResolution, CreateResult, OperationComplete, OperationType, RenameResult,
    ReplacePolicy, TransferOptions, TransferResult, UndoComplete, UndoEntry, UndoResult,
    UndoableOperation, execute_undo, start_copy, start_create_directory, start_move, start_rename,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Never touch the real Trash from tests.
fn options() -> TransferOptions {
    TransferOptions {
        replace_policy: ReplacePolicy::Permanent,
        ..TransferOptions::default()
    }
}

fn resolving(resolution: ConflictResolution) -> TransferOptions {
    TransferOptions {
        conflict_resolution: Some(resolution),
        ..options()
    }
}

/// Drain a copy/move, leaving conflicts unanswered (an implicit skip).
async fn wait_for_transfer(
    mut rx: mpsc::Receiver<TransferResult>,
) -> (OperationComplete, Vec<(PathBuf, PathBuf)>, usize) {
    let mut conflicts = 0;
    while let Some(result) = rx.recv().await {
        match result {
            TransferResult::Progress(_) => {}
            TransferResult::Conflict(_) => conflicts += 1,
            TransferResult::Complete(complete) => {
                return (complete.inner, complete.completed, conflicts);
            }
        }
    }
    panic!("operation ended without a completion event");
}

async fn answer_all(
    mut rx: mpsc::Receiver<TransferResult>,
    answer: ConflictResolution,
) -> (OperationComplete, usize) {
    let mut conflicts = 0;
    while let Some(result) = rx.recv().await {
        match result {
            TransferResult::Progress(_) => {}
            TransferResult::Conflict(request) => {
                conflicts += 1;
                request.resolve(answer);
            }
            TransferResult::Complete(complete) => return (complete.inner, conflicts),
        }
    }
    panic!("operation ended without a completion event");
}

async fn wait_for_undo(mut rx: mpsc::Receiver<UndoResult>) -> UndoComplete {
    while let Some(result) = rx.recv().await {
        if let UndoResult::Complete(complete) = result {
            return complete;
        }
    }
    panic!("undo ended without a completion event");
}

async fn undo(operation: UndoableOperation) -> OperationComplete {
    wait_for_undo(execute_undo(
        UndoEntry::new(0, operation, "test"),
        CancellationToken::new(),
    ))
    .await
    .inner
}

async fn wait_for_create(mut rx: mpsc::Receiver<CreateResult>) -> OperationComplete {
    while let Some(result) = rx.recv().await {
        if let CreateResult::Complete(complete) = result {
            return complete;
        }
    }
    panic!("create ended without a completion event");
}

async fn wait_for_rename(mut rx: mpsc::Receiver<RenameResult>) -> OperationComplete {
    while let Some(result) = rx.recv().await {
        if let RenameResult::Complete(complete) = result {
            return complete;
        }
    }
    panic!("rename ended without a completion event");
}

fn destinations(completed: &[(PathBuf, PathBuf)]) -> Vec<PathBuf> {
    completed
        .iter()
        .map(|(_, destination)| destination.clone())
        .collect()
}

fn staging_leftovers(directory: &std::path::Path) -> Vec<String> {
    fs::read_dir(directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(gravityfile_ops::STAGING_PREFIX))
        .collect()
}

// ---------------------------------------------------------------------------
// Copy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn copy_directory_preserves_nested_contents_and_reports_created_path() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("root.txt"), "root file").unwrap();
    fs::write(source.join("nested/child.txt"), "child file").unwrap();
    let destination = tmp.path().join("destination");

    let (complete, completed, conflicts) = wait_for_transfer(start_copy(
        vec![source.clone()],
        destination.clone(),
        options(),
        CancellationToken::new(),
    ))
    .await;
    let copied = destination.join("source");

    assert_eq!(complete.operation_type, OperationType::Copy);
    assert_eq!((complete.succeeded, complete.failed, conflicts), (1, 0, 0));
    assert_eq!(completed, vec![(source, copied.clone())]);
    assert_eq!(
        fs::read_to_string(copied.join("root.txt")).unwrap(),
        "root file"
    );
    assert_eq!(
        fs::read_to_string(copied.join("nested/child.txt")).unwrap(),
        "child file"
    );
    assert_eq!(complete.bytes_processed, 19);
}

#[tokio::test]
async fn copy_preserves_file_and_directory_modification_times() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("dated.txt"), "dated").unwrap();
    let file_time = FileTime::from_unix_time(1_000_000, 0);
    let dir_time = FileTime::from_unix_time(1_000_100, 0);
    filetime::set_file_mtime(source.join("dated.txt"), file_time).unwrap();
    filetime::set_file_mtime(&source, dir_time).unwrap();
    let destination = tmp.path().join("destination");

    let (complete, _, _) = wait_for_transfer(start_copy(
        vec![source],
        destination.clone(),
        options(),
        CancellationToken::new(),
    ))
    .await;

    assert!(complete.is_success(), "{complete:?}");
    let mtime = |path: PathBuf| FileTime::from_last_modification_time(&fs::metadata(path).unwrap());
    assert_eq!(mtime(destination.join("source/dated.txt")), file_time);
    assert_eq!(mtime(destination.join("source")), dir_time);
}

#[cfg(unix)]
#[tokio::test]
async fn copy_directory_recreates_symlink_without_following_target() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let external = tmp.path().join("external.txt");
    fs::create_dir_all(&source).unwrap();
    fs::write(&external, "outside the copied tree").unwrap();
    std::os::unix::fs::symlink(&external, source.join("link")).unwrap();
    let destination = tmp.path().join("destination");

    let (complete, _, _) = wait_for_transfer(start_copy(
        vec![source],
        destination.clone(),
        options(),
        CancellationToken::new(),
    ))
    .await;

    assert!(complete.is_success());
    let copied = destination.join("source/link");
    assert!(
        fs::symlink_metadata(&copied)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(copied).unwrap(), external);
}

#[tokio::test]
async fn copy_overwrite_replaces_destination_and_is_not_undoable() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("data.txt");
    let destination = tmp.path().join("destination");
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "new content").unwrap();
    fs::write(destination.join("data.txt"), "old content").unwrap();

    let (complete, completed, conflicts) = wait_for_transfer(start_copy(
        vec![source],
        destination.clone(),
        resolving(ConflictResolution::Overwrite),
        CancellationToken::new(),
    ))
    .await;

    assert_eq!((complete.succeeded, complete.failed, conflicts), (1, 0, 0));
    assert!(
        completed.is_empty(),
        "overwritten paths must not be recorded as safe-to-remove undo targets"
    );
    assert_eq!(
        fs::read_to_string(destination.join("data.txt")).unwrap(),
        "new content"
    );
    assert!(staging_leftovers(&destination).is_empty());
}

#[tokio::test]
async fn copy_overwrite_failure_preserves_existing_destination() {
    let tmp = TempDir::new().unwrap();
    let destination = tmp.path().join("destination");
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join("ghost.txt"), "precious").unwrap();

    let (complete, _, _) = wait_for_transfer(start_copy(
        vec![tmp.path().join("missing/ghost.txt")],
        destination.clone(),
        resolving(ConflictResolution::Overwrite),
        CancellationToken::new(),
    ))
    .await;

    assert_eq!((complete.succeeded, complete.failed), (0, 1));
    assert_eq!(
        fs::read_to_string(destination.join("ghost.txt")).unwrap(),
        "precious"
    );
}

#[tokio::test]
async fn copy_or_move_into_own_descendant_is_rejected_before_mutation() {
    for moving in [false, true] {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let destination = source.join("nested/destination");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("data.txt"), "data").unwrap();
        let start = if moving { start_move } else { start_copy };

        let (complete, completed, _) = wait_for_transfer(start(
            vec![source.clone()],
            destination.clone(),
            options(),
            CancellationToken::new(),
        ))
        .await;

        assert_eq!((complete.succeeded, complete.failed), (0, 1));
        assert!(completed.is_empty());
        assert!(!destination.exists());
        assert_eq!(fs::read_to_string(source.join("data.txt")).unwrap(), "data");
    }
}

#[tokio::test]
async fn names_at_the_filesystem_length_limit_copy_and_move() {
    for moving in [false, true] {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("src");
        let destination = tmp.path().join("dst");
        fs::create_dir_all(&source_dir).unwrap();
        let long_name = "a".repeat(250);
        fs::write(source_dir.join(&long_name), "payload").unwrap();
        let start = if moving { start_move } else { start_copy };

        let (complete, _, _) = wait_for_transfer(start(
            vec![source_dir.join(&long_name)],
            destination.clone(),
            options(),
            CancellationToken::new(),
        ))
        .await;

        assert_eq!(complete.succeeded, 1, "{:?}", complete.errors);
        assert_eq!(
            fs::read_to_string(destination.join(&long_name)).unwrap(),
            "payload"
        );
    }
}

#[tokio::test]
async fn cancelled_directory_copy_leaves_no_partial_destination() {
    let tmp = TempDir::new().unwrap();
    let tree = tmp.path().join("src/tree");
    let destination = tmp.path().join("dst");
    fs::create_dir_all(&tree).unwrap();
    fs::create_dir_all(&destination).unwrap();
    for index in 0..200 {
        fs::write(tree.join(format!("file{index}.bin")), vec![0u8; 4096]).unwrap();
    }
    let token = CancellationToken::new();
    token.cancel();

    let (complete, _, _) = wait_for_transfer(start_copy(
        vec![tree],
        destination.clone(),
        options(),
        token,
    ))
    .await;

    assert!(complete.cancelled && !complete.is_success());
    assert!(complete.summary().contains("cancelled"));
    assert_eq!(complete.succeeded, 0);
    assert!(!destination.join("tree").exists());
    assert!(staging_leftovers(&destination).is_empty());
}

// ---------------------------------------------------------------------------
// Move
// ---------------------------------------------------------------------------

#[tokio::test]
async fn move_auto_rename_keeps_existing_destination_and_records_final_pair() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("report.txt");
    let destination = tmp.path().join("destination");
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "new report").unwrap();
    fs::write(destination.join("report.txt"), "existing report").unwrap();

    let (complete, completed, _) = wait_for_transfer(start_move(
        vec![source.clone()],
        destination.clone(),
        resolving(ConflictResolution::AutoRename),
        CancellationToken::new(),
    ))
    .await;
    let renamed = destination.join("report (1).txt");

    assert_eq!(complete.operation_type, OperationType::Move);
    assert_eq!((complete.succeeded, complete.failed), (1, 0));
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(destination.join("report.txt")).unwrap(),
        "existing report"
    );
    assert_eq!(fs::read_to_string(&renamed).unwrap(), "new report");
    assert_eq!(completed, vec![(source, renamed)]);
}

#[tokio::test]
async fn unanswered_conflict_is_a_skip_not_a_failure() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("same.txt");
    let destination = tmp.path().join("destination");
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "source").unwrap();
    fs::write(destination.join("same.txt"), "destination").unwrap();

    let (complete, completed, conflicts) = wait_for_transfer(start_move(
        vec![source.clone()],
        destination.clone(),
        options(),
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(conflicts, 1);
    assert_eq!(
        (complete.succeeded, complete.failed, complete.skipped),
        (0, 0, 1)
    );
    assert!(completed.is_empty());
    assert_eq!(fs::read_to_string(&source).unwrap(), "source");
    assert_eq!(
        fs::read_to_string(destination.join("same.txt")).unwrap(),
        "destination"
    );
}

#[tokio::test]
async fn move_overwrite_replaces_and_does_not_offer_lossy_undo() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("same.txt");
    let destination = tmp.path().join("destination");
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "replacement").unwrap();
    fs::write(destination.join("same.txt"), "old destination").unwrap();

    let (complete, completed, _) = wait_for_transfer(start_move(
        vec![source.clone()],
        destination.clone(),
        resolving(ConflictResolution::Overwrite),
        CancellationToken::new(),
    ))
    .await;

    assert_eq!((complete.succeeded, complete.failed), (1, 0));
    assert!(completed.is_empty());
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(destination.join("same.txt")).unwrap(),
        "replacement"
    );
}

#[tokio::test]
async fn cancelled_move_reports_cancellation_and_preserves_source() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let destination = tmp.path().join("destination");
    fs::write(&source, b"keep").unwrap();
    fs::create_dir(&destination).unwrap();
    let token = CancellationToken::new();
    token.cancel();

    let (complete, completed, _) = wait_for_transfer(start_move(
        vec![source.clone()],
        destination.clone(),
        options(),
        token,
    ))
    .await;

    assert!(complete.cancelled && !complete.is_success());
    assert_eq!(complete.succeeded, 0);
    assert!(completed.is_empty());
    assert_eq!(fs::read(&source).unwrap(), b"keep");
    assert!(!destination.join("source").exists());
}

// ---------------------------------------------------------------------------
// Conflicts and batch control
// ---------------------------------------------------------------------------

#[tokio::test]
async fn answering_a_conflict_resolves_only_that_item() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let destination = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(source_dir.join("clash.txt"), "NEW").unwrap();
    fs::write(source_dir.join("fresh.txt"), "FRESH").unwrap();
    fs::write(destination.join("clash.txt"), "OLD").unwrap();

    let (complete, conflicts) = answer_all(
        start_copy(
            vec![source_dir.join("clash.txt"), source_dir.join("fresh.txt")],
            destination.clone(),
            options(),
            CancellationToken::new(),
        ),
        ConflictResolution::Overwrite,
    )
    .await;

    assert_eq!(conflicts, 1);
    assert_eq!((complete.succeeded, complete.failed), (2, 0));
    assert_eq!(
        fs::read_to_string(destination.join("clash.txt")).unwrap(),
        "NEW"
    );
    assert_eq!(
        fs::read_to_string(destination.join("fresh.txt")).unwrap(),
        "FRESH"
    );
}

#[tokio::test]
async fn an_apply_to_all_answer_suppresses_later_prompts() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let destination = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let names = ["a.txt", "b.txt", "c.txt"];
    for name in names {
        fs::write(source_dir.join(name), "NEW").unwrap();
        fs::write(destination.join(name), "OLD").unwrap();
    }

    let (complete, conflicts) = answer_all(
        start_copy(
            names.iter().map(|name| source_dir.join(name)).collect(),
            destination.clone(),
            options(),
            CancellationToken::new(),
        ),
        ConflictResolution::OverwriteAll,
    )
    .await;

    assert_eq!(conflicts, 1);
    assert_eq!(complete.succeeded, 3);
    for name in names {
        assert_eq!(fs::read_to_string(destination.join(name)).unwrap(), "NEW");
    }
}

#[tokio::test]
async fn aborting_copy_and_move_retains_completed_work_and_undo_records() {
    for moving in [false, true] {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let sources: Vec<_> = ["first", "conflict", "last"]
            .iter()
            .map(|name| {
                let path = tmp.path().join(name);
                fs::write(&path, name.as_bytes()).unwrap();
                path
            })
            .collect();
        fs::write(destination.join("conflict"), b"original").unwrap();
        let start = if moving { start_move } else { start_copy };

        let (complete, completed, _) = wait_for_transfer(start(
            sources.clone(),
            destination.clone(),
            resolving(ConflictResolution::Abort),
            CancellationToken::new(),
        ))
        .await;

        assert_eq!(destinations(&completed), vec![destination.join("first")]);
        assert!(complete.cancelled && !complete.is_success());
        assert_eq!(
            (complete.succeeded, complete.failed, complete.skipped),
            (1, 0, 0)
        );
        assert_eq!(fs::read(destination.join("first")).unwrap(), b"first");
        assert_eq!(fs::read(destination.join("conflict")).unwrap(), b"original");
        assert!(sources[1].exists() && sources[2].exists());
        assert!(!destination.join("last").exists());
    }
}

#[tokio::test]
async fn cancellation_while_a_conflict_is_pending_ends_the_operation() {
    for moving in [false, true] {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let destination = tmp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"source").unwrap();
        fs::write(destination.join("source"), b"existing").unwrap();
        let token = CancellationToken::new();
        let start = if moving { start_move } else { start_copy };
        let mut rx = start(
            vec![source.clone()],
            destination.clone(),
            options(),
            token.clone(),
        );

        let request = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await.expect("operation must ask") {
                    TransferResult::Conflict(request) => break request,
                    TransferResult::Progress(_) => {}
                    TransferResult::Complete(_) => panic!("completed before conflict"),
                }
            }
        })
        .await
        .unwrap();
        token.cancel();
        let (complete, _, _) = tokio::time::timeout(Duration::from_secs(5), wait_for_transfer(rx))
            .await
            .unwrap();

        assert!(complete.cancelled);
        assert_eq!(
            (complete.succeeded, complete.failed, complete.skipped),
            (0, 0, 0)
        );
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(destination.join("source")).unwrap(), b"existing");
        drop(request);
    }
}

#[tokio::test]
async fn stalled_progress_consumer_does_not_suspend_work() {
    for moving in [false, true] {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let sources: Vec<_> = (0..150)
            .map(|index| {
                let path = tmp.path().join(format!("item-{index}"));
                fs::write(&path, b"payload").unwrap();
                path
            })
            .collect();
        let start = if moving { start_move } else { start_copy };
        let rx = start(
            sources,
            destination.clone(),
            options(),
            CancellationToken::new(),
        );

        tokio::time::timeout(Duration::from_secs(10), async {
            while !destination.join("item-149").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("filesystem work must not wait for progress consumption");
        let (complete, _, _) = wait_for_transfer(rx).await;
        assert!(complete.is_success());
        assert_eq!(complete.succeeded, 150);
    }
}

#[tokio::test]
async fn dropping_the_receiver_cancels_pending_work() {
    for moving in [false, true] {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let destination = tmp.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("source"), b"existing").unwrap();
        let token = CancellationToken::new();
        let start = if moving { start_move } else { start_copy };
        drop(start(
            vec![source.clone()],
            destination.clone(),
            options(),
            token.clone(),
        ));

        tokio::time::timeout(Duration::from_secs(5), token.cancelled())
            .await
            .unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read(destination.join("source")).unwrap(), b"existing");
    }
}

// ---------------------------------------------------------------------------
// Create and rename
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn create_file_rejects_an_existing_broken_symlink() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("broken-link");
    std::os::unix::fs::symlink("missing-target", &path).unwrap();

    let complete = wait_for_create(gravityfile_ops::start_create_file(path.clone())).await;

    assert_eq!((complete.succeeded, complete.failed), (0, 1));
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!tmp.path().join("missing-target").exists());
}

#[tokio::test]
async fn create_directory_creates_missing_parents_but_owns_only_the_final_path() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("parent/created");

    let complete = wait_for_create(start_create_directory(path.clone())).await;
    assert_eq!((complete.succeeded, complete.failed), (1, 0));
    assert!(path.is_dir());

    let duplicate = wait_for_create(start_create_directory(path.clone())).await;
    assert_eq!((duplicate.succeeded, duplicate.failed), (0, 1));
}

#[tokio::test]
async fn rename_to_the_same_name_is_a_noop() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("same.txt");
    fs::write(&source, "unchanged").unwrap();

    let complete = wait_for_rename(start_rename(source.clone(), "same.txt".into())).await;

    assert_eq!((complete.succeeded, complete.failed), (0, 0));
    assert_eq!(fs::read_to_string(source).unwrap(), "unchanged");
}

#[cfg(unix)]
#[tokio::test]
async fn rename_does_not_replace_a_broken_symlink_destination() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source.txt");
    let destination = tmp.path().join("destination.txt");
    fs::write(&source, "source").unwrap();
    std::os::unix::fs::symlink("missing-target", &destination).unwrap();

    let complete = wait_for_rename(start_rename(source.clone(), "destination.txt".into())).await;

    assert_eq!((complete.succeeded, complete.failed), (0, 1));
    assert_eq!(fs::read_to_string(source).unwrap(), "source");
    assert!(
        fs::symlink_metadata(destination)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

// ---------------------------------------------------------------------------
// Undo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn undo_move_restores_each_original_parent() {
    let tmp = TempDir::new().unwrap();
    let original_a = tmp.path().join("a/a.txt");
    let original_b = tmp.path().join("b/b.txt");
    let current_a = tmp.path().join("moved/a.txt");
    let current_b = tmp.path().join("moved/b.txt");
    fs::create_dir_all(tmp.path().join("moved")).unwrap();
    fs::write(&current_a, "alpha").unwrap();
    fs::write(&current_b, "bravo").unwrap();

    let complete = undo(UndoableOperation::FilesMoved {
        moves: vec![
            (original_a.clone(), current_a.clone()),
            (original_b.clone(), current_b.clone()),
        ],
    })
    .await;

    assert_eq!(complete.operation_type, OperationType::Move);
    assert_eq!((complete.succeeded, complete.failed), (2, 0));
    assert_eq!(fs::read_to_string(&original_a).unwrap(), "alpha");
    assert_eq!(fs::read_to_string(&original_b).unwrap(), "bravo");
    assert!(!current_a.exists() && !current_b.exists());
}

#[tokio::test]
async fn undo_move_never_overwrites_a_file_recreated_at_the_original_path() {
    let tmp = TempDir::new().unwrap();
    let original = tmp.path().join("notes.txt");
    let moved_to = tmp.path().join("sub/notes.txt");
    fs::create_dir_all(moved_to.parent().unwrap()).unwrap();
    fs::write(&moved_to, "OLD MOVED CONTENT").unwrap();
    fs::write(&original, "PRECIOUS NEW USER DATA").unwrap();

    let complete = wait_for_undo(execute_undo(
        UndoEntry::new(
            9,
            UndoableOperation::FilesMoved {
                moves: vec![(original.clone(), moved_to.clone())],
            },
            "Moved 1 item(s)",
        ),
        CancellationToken::new(),
    ))
    .await;

    assert_eq!((complete.inner.succeeded, complete.inner.failed), (0, 1));
    assert_eq!(
        fs::read_to_string(&original).unwrap(),
        "PRECIOUS NEW USER DATA"
    );
    assert!(moved_to.exists());
    let retry = complete
        .remaining
        .expect("failed items are returned for retry");
    assert!(retry.description.ends_with("(undo incomplete)"));
}

#[tokio::test]
async fn undo_creation_removes_only_empty_items_of_the_expected_type() {
    for directory in [false, true] {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("created");
        let operation = if directory {
            fs::create_dir(&path).unwrap();
            UndoableOperation::DirectoryCreated { path: path.clone() }
        } else {
            fs::write(&path, []).unwrap();
            UndoableOperation::FileCreated { path: path.clone() }
        };
        let complete = undo(operation).await;
        assert!(complete.is_success());
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn undo_creation_preserves_later_content_and_changed_types() {
    for case in [
        "file-now-directory",
        "directory-now-file",
        "file-with-data",
        "dir-with-child",
    ] {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("created");
        match case {
            "file-now-directory" => fs::create_dir(&path).unwrap(),
            "dir-with-child" => {
                fs::create_dir(&path).unwrap();
                fs::write(path.join("child"), "x").unwrap();
            }
            _ => fs::write(&path, b"later content").unwrap(),
        }
        let operation = if matches!(case, "directory-now-file" | "dir-with-child") {
            UndoableOperation::DirectoryCreated { path: path.clone() }
        } else {
            UndoableOperation::FileCreated { path: path.clone() }
        };
        let complete = undo(operation).await;
        assert!(!complete.is_success(), "{case}");
        assert_eq!(complete.errors.len(), 1);
        assert_eq!(complete.errors[0].path, path);
        assert!(path.exists(), "{case}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn undo_creation_preserves_replacement_symlinks_and_their_targets() {
    for directory in [false, true] {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        let path = tmp.path().join("created");
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let operation = if directory {
            UndoableOperation::DirectoryCreated { path: path.clone() }
        } else {
            UndoableOperation::FileCreated { path: path.clone() }
        };
        assert!(!undo(operation).await.is_success());
        assert!(path.symlink_metadata().unwrap().is_symlink());
        assert_eq!(fs::read(target).unwrap(), b"keep");
    }
}

#[tokio::test]
async fn undo_reports_missing_copies_and_unrestorable_trash() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("missing");
    let complete = undo(UndoableOperation::FilesCopied {
        created: vec![missing.clone()],
    })
    .await;
    assert_eq!(complete.failed, 1);
    assert_eq!(complete.errors[0].path, missing);

    let complete = undo(UndoableOperation::FilesTrashed {
        paths: vec![missing],
        at: std::time::SystemTime::now(),
    })
    .await;
    assert!(!complete.is_success());
    assert_eq!(complete.failed, 1);
}

#[tokio::test]
async fn precancelled_undo_changes_nothing_and_returns_the_entry() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("current");
    let original = tmp.path().join("original");
    fs::write(&path, []).unwrap();
    let operations = [
        UndoableOperation::FileCreated { path: path.clone() },
        UndoableOperation::DirectoryCreated { path: path.clone() },
        UndoableOperation::FilesCopied {
            created: vec![path.clone()],
        },
        UndoableOperation::FilesMoved {
            moves: vec![(original.clone(), path.clone())],
        },
        UndoableOperation::FilesRenamed {
            renames: vec![(original.clone(), path.clone())],
        },
    ];
    for operation in operations {
        let token = CancellationToken::new();
        token.cancel();
        let complete = wait_for_undo(execute_undo(
            UndoEntry::new(0, operation.clone(), "undo"),
            token,
        ))
        .await;
        assert!(complete.inner.cancelled);
        assert_eq!((complete.inner.succeeded, complete.inner.failed), (0, 0));
        assert_eq!(complete.remaining.unwrap().operation, operation);
        assert!(path.exists() && !original.exists());
    }
}

#[tokio::test]
async fn undo_rename_restores_full_original_path_without_lossy_names() {
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt;
    let tmp = TempDir::new().unwrap();
    let original_parent = tmp.path().join("original-parent");
    fs::create_dir(&original_parent).unwrap();
    // Linux permits arbitrary filename bytes; APFS rejects invalid UTF-8.
    #[cfg(target_os = "linux")]
    let name = std::ffi::OsString::from_vec(b"original-\xff".to_vec());
    #[cfg(not(target_os = "linux"))]
    let name = std::ffi::OsString::from("original-é");
    let original = original_parent.join(name);
    let current = tmp.path().join("current");
    fs::write(&current, b"preserved").unwrap();

    let complete = undo(UndoableOperation::FilesRenamed {
        renames: vec![(original.clone(), current.clone())],
    })
    .await;

    assert!(complete.is_success(), "{:?}", complete.errors);
    assert_eq!(fs::read(original).unwrap(), b"preserved");
    assert!(!current.exists());
}
