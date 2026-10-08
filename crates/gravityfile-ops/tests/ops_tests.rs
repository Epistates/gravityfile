use std::fs;
use std::path::PathBuf;

use filetime::FileTime;
use gravityfile_ops::{
    ConflictResolution, CopyOptions, CopyResult, CreateResult, MoveOptions, MoveResult,
    OperationComplete, OperationResult, OperationType, RenameResult, UndoEntry, UndoableOperation,
    execute_undo, start_copy, start_create_directory, start_create_file, start_move, start_rename,
};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn wait_for_copy_complete(
    mut rx: mpsc::Receiver<CopyResult>,
) -> (OperationComplete, Vec<PathBuf>, usize) {
    let mut conflicts = 0;
    while let Some(result) = rx.recv().await {
        match result {
            CopyResult::Progress(_) => {}
            CopyResult::Conflict(_) => conflicts += 1,
            CopyResult::Complete(complete) => {
                return (complete.inner, complete.created_paths, conflicts);
            }
        }
    }
    panic!("copy operation ended without a completion event");
}

async fn wait_for_move_complete(
    mut rx: mpsc::Receiver<MoveResult>,
) -> (OperationComplete, Vec<(PathBuf, PathBuf)>, usize) {
    let mut conflicts = 0;
    while let Some(result) = rx.recv().await {
        match result {
            MoveResult::Progress(_) => {}
            MoveResult::Conflict(_) => conflicts += 1,
            MoveResult::Complete(complete) => {
                return (complete.inner, complete.moved_pairs, conflicts);
            }
        }
    }
    panic!("move operation ended without a completion event");
}

async fn wait_for_operation_complete(mut rx: mpsc::Receiver<OperationResult>) -> OperationComplete {
    while let Some(result) = rx.recv().await {
        if let OperationResult::Complete(complete) = result {
            return complete;
        }
    }
    panic!("operation ended without a completion event");
}

async fn wait_for_create_complete(mut rx: mpsc::Receiver<CreateResult>) -> OperationComplete {
    while let Some(result) = rx.recv().await {
        if let CreateResult::Complete(complete) = result {
            return complete;
        }
    }
    panic!("create operation ended without a completion event");
}

async fn wait_for_rename_complete(mut rx: mpsc::Receiver<RenameResult>) -> OperationComplete {
    while let Some(result) = rx.recv().await {
        if let RenameResult::Complete(complete) = result {
            return complete;
        }
    }
    panic!("rename operation ended without a completion event");
}

#[tokio::test]
async fn copy_directory_preserves_nested_contents_and_reports_created_path() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let nested = source.join("nested");
    let destination = tmp.path().join("destination");

    fs::create_dir_all(&nested).unwrap();
    fs::write(source.join("root.txt"), "root file").unwrap();
    fs::write(nested.join("child.txt"), "child file").unwrap();

    let rx = start_copy(
        vec![source.clone()],
        destination.clone(),
        CopyOptions::default(),
        CancellationToken::new(),
    );

    let (complete, created_paths, conflicts) = wait_for_copy_complete(rx).await;
    let copied_source = destination.join("source");

    assert_eq!(complete.operation_type, OperationType::Copy);
    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert_eq!(conflicts, 0);
    assert_eq!(created_paths, vec![copied_source.clone()]);
    assert_eq!(
        fs::read_to_string(copied_source.join("root.txt")).unwrap(),
        "root file"
    );
    assert_eq!(
        fs::read_to_string(copied_source.join("nested/child.txt")).unwrap(),
        "child file"
    );
}

#[tokio::test]
async fn copy_preserves_file_modification_time_when_requested() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("source");
    let destination = tmp.path().join("destination");
    let source = source_dir.join("dated.txt");

    fs::create_dir_all(&source_dir).unwrap();
    fs::write(&source, "dated content").unwrap();
    let expected_mtime = FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(&source, expected_mtime).unwrap();

    let rx = start_copy(
        vec![source],
        destination.clone(),
        CopyOptions {
            conflict_resolution: None,
            preserve_timestamps: true,
        },
        CancellationToken::new(),
    );
    let (complete, _, conflicts) = wait_for_copy_complete(rx).await;

    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert_eq!(conflicts, 0);
    let copied_metadata = fs::metadata(destination.join("dated.txt")).unwrap();
    assert_eq!(
        FileTime::from_last_modification_time(&copied_metadata),
        expected_mtime
    );
}

#[tokio::test]
async fn copy_preserves_directory_modification_time_after_copying_children() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let destination = tmp.path().join("destination");

    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("child.txt"), "child").unwrap();
    let expected_mtime = FileTime::from_unix_time(1_000_100, 0);
    filetime::set_file_mtime(&source, expected_mtime).unwrap();

    let rx = start_copy(
        vec![source],
        destination.clone(),
        CopyOptions {
            conflict_resolution: None,
            preserve_timestamps: true,
        },
        CancellationToken::new(),
    );
    let (complete, _, conflicts) = wait_for_copy_complete(rx).await;

    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert_eq!(conflicts, 0);
    let copied_metadata = fs::metadata(destination.join("source")).unwrap();
    assert_eq!(
        FileTime::from_last_modification_time(&copied_metadata),
        expected_mtime
    );
}

#[cfg(unix)]
#[tokio::test]
async fn copy_directory_recreates_symlink_without_following_target() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let destination = tmp.path().join("destination");
    let external = tmp.path().join("external.txt");
    let link = source.join("external-link");

    fs::create_dir_all(&source).unwrap();
    fs::write(&external, "outside the copied tree").unwrap();
    std::os::unix::fs::symlink(&external, &link).unwrap();

    let expected_mtime = FileTime::from_unix_time(1_000_200, 0);
    filetime::set_symlink_file_times(&link, expected_mtime, expected_mtime).unwrap();

    let rx = start_copy(
        vec![source.clone()],
        destination.clone(),
        CopyOptions {
            conflict_resolution: None,
            preserve_timestamps: true,
        },
        CancellationToken::new(),
    );

    let (complete, _, conflicts) = wait_for_copy_complete(rx).await;
    let copied_link = destination.join("source/external-link");

    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert_eq!(conflicts, 0);
    assert!(
        fs::symlink_metadata(&copied_link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "copied entry must remain a symlink"
    );
    assert_eq!(fs::read_link(copied_link).unwrap(), external);
    let copied_link_metadata = fs::symlink_metadata(destination.join("source/external-link"))
        .expect("copied link metadata");
    assert_eq!(
        FileTime::from_last_modification_time(&copied_link_metadata),
        expected_mtime
    );
}

#[tokio::test]
async fn copy_overwrite_replaces_destination_content() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("source");
    let destination = tmp.path().join("destination");
    let source = source_dir.join("data.txt");
    let existing = destination.join("data.txt");

    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "new content").unwrap();
    fs::write(&existing, "old content").unwrap();

    let options = CopyOptions {
        conflict_resolution: Some(ConflictResolution::Overwrite),
        preserve_timestamps: false,
    };
    let rx = start_copy(
        vec![source.clone()],
        destination.clone(),
        options,
        CancellationToken::new(),
    );

    let (complete, created, conflicts) = wait_for_copy_complete(rx).await;

    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert_eq!(conflicts, 0);
    assert!(
        created.is_empty(),
        "overwritten paths must not be recorded as safe-to-delete undo targets"
    );
    assert_eq!(fs::read_to_string(&existing).unwrap(), "new content");
    // No staging temp files should be left behind.
    let leftovers: Vec<_> = fs::read_dir(&destination)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".gravityfile-tmp")
        })
        .collect();
    assert!(leftovers.is_empty(), "overwrite left a staging temp behind");
}

#[tokio::test]
async fn copy_directory_into_its_descendant_is_rejected_before_mutation() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let destination = source.join("nested/destination");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("data.txt"), "data").unwrap();

    let rx = start_copy(
        vec![source.clone()],
        destination.clone(),
        CopyOptions::default(),
        CancellationToken::new(),
    );
    let (complete, created, _) = wait_for_copy_complete(rx).await;

    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 1);
    assert!(!complete.errors.is_empty());
    assert!(created.is_empty());
    assert!(
        !destination.exists(),
        "validation must happen before creating the nested destination"
    );
    assert_eq!(fs::read_to_string(source.join("data.txt")).unwrap(), "data");
}

#[tokio::test]
async fn copy_overwrite_failure_preserves_existing_destination() {
    // When the staged copy fails (here: the source no longer exists), the
    // pre-existing destination must be left intact — no data loss.
    let tmp = TempDir::new().unwrap();
    let destination = tmp.path().join("destination");
    let existing = destination.join("ghost.txt");
    let missing_source = tmp.path().join("source").join("ghost.txt");

    fs::create_dir_all(&destination).unwrap();
    fs::write(&existing, "precious").unwrap();

    let options = CopyOptions {
        conflict_resolution: Some(ConflictResolution::Overwrite),
        preserve_timestamps: false,
    };
    let rx = start_copy(
        vec![missing_source],
        destination.clone(),
        options,
        CancellationToken::new(),
    );

    let (complete, _created, _conflicts) = wait_for_copy_complete(rx).await;

    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 1);
    assert_eq!(
        fs::read_to_string(&existing).unwrap(),
        "precious",
        "failed overwrite must not destroy the existing destination"
    );
}

#[tokio::test]
async fn move_auto_rename_keeps_existing_destination_and_records_final_pair() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("source");
    let destination = tmp.path().join("destination");
    let source = source_dir.join("report.txt");
    let existing = destination.join("report.txt");
    let renamed = destination.join("report (1).txt");

    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "new report").unwrap();
    fs::write(&existing, "existing report").unwrap();

    let options = MoveOptions {
        conflict_resolution: Some(ConflictResolution::AutoRename),
    };
    let rx = start_move(
        vec![source.clone()],
        destination.clone(),
        options,
        CancellationToken::new(),
    );

    let (complete, moved_pairs, conflicts) = wait_for_move_complete(rx).await;

    assert_eq!(complete.operation_type, OperationType::Move);
    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert_eq!(conflicts, 0);
    assert!(
        !source.exists(),
        "source should be removed after a successful move"
    );
    assert_eq!(fs::read_to_string(&existing).unwrap(), "existing report");
    assert_eq!(fs::read_to_string(&renamed).unwrap(), "new report");
    assert_eq!(moved_pairs, vec![(source, renamed)]);
}

#[tokio::test]
async fn move_directory_into_its_descendant_is_rejected_before_mutation() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let destination = source.join("nested/destination");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("data.txt"), "data").unwrap();

    let rx = start_move(
        vec![source.clone()],
        destination.clone(),
        MoveOptions::default(),
        CancellationToken::new(),
    );
    let (complete, moved_pairs, conflicts) = wait_for_move_complete(rx).await;

    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 1);
    assert_eq!(conflicts, 0);
    assert!(moved_pairs.is_empty());
    assert!(!destination.exists());
    assert_eq!(fs::read_to_string(source.join("data.txt")).unwrap(), "data");
}

#[cfg(unix)]
#[tokio::test]
async fn create_file_rejects_an_existing_broken_symlink() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("broken-link");
    std::os::unix::fs::symlink("missing-target", &path).unwrap();

    let complete = wait_for_create_complete(start_create_file(path.clone())).await;

    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 1);
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

    let complete = wait_for_create_complete(start_create_directory(path.clone())).await;

    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert!(path.is_dir());

    let duplicate = wait_for_create_complete(start_create_directory(path.clone())).await;
    assert_eq!(duplicate.succeeded, 0);
    assert_eq!(duplicate.failed, 1);
    assert!(path.is_dir());
}

#[tokio::test]
async fn rename_to_the_same_name_is_a_noop() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().join("same.txt");
    fs::write(&source, "unchanged").unwrap();

    let complete = wait_for_rename_complete(start_rename(source.clone(), "same.txt".into())).await;

    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 0);
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

    let complete =
        wait_for_rename_complete(start_rename(source.clone(), "destination.txt".into())).await;

    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 1);
    assert_eq!(fs::read_to_string(source).unwrap(), "source");
    assert!(
        fs::symlink_metadata(destination)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn move_without_conflict_resolution_emits_conflict_and_skips_item() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("source");
    let destination = tmp.path().join("destination");
    let source = source_dir.join("same.txt");
    let existing = destination.join("same.txt");

    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "source").unwrap();
    fs::write(&existing, "destination").unwrap();

    let rx = start_move(
        vec![source.clone()],
        destination,
        MoveOptions::default(),
        CancellationToken::new(),
    );

    let (complete, moved_pairs, conflicts) = wait_for_move_complete(rx).await;

    // Dropping the request unanswered is an implicit skip. A skip is not a
    // failure: nothing went wrong, the user (or the harness) declined the item.
    assert_eq!(conflicts, 1);
    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 0);
    assert_eq!(complete.skipped, 1);
    assert!(moved_pairs.is_empty());
    assert_eq!(fs::read_to_string(&source).unwrap(), "source");
    assert_eq!(fs::read_to_string(&existing).unwrap(), "destination");
}

#[tokio::test]
async fn move_overwrite_does_not_offer_lossy_undo() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("source");
    let destination = tmp.path().join("destination");
    let source = source_dir.join("same.txt");
    let existing = destination.join("same.txt");

    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&destination).unwrap();
    fs::write(&source, "replacement").unwrap();
    fs::write(&existing, "old destination").unwrap();

    let rx = start_move(
        vec![source.clone()],
        destination,
        MoveOptions {
            conflict_resolution: Some(ConflictResolution::Overwrite),
        },
        CancellationToken::new(),
    );
    let (complete, moved_pairs, conflicts) = wait_for_move_complete(rx).await;

    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert_eq!(conflicts, 0);
    assert!(moved_pairs.is_empty());
    assert!(!source.exists());
    assert_eq!(fs::read_to_string(existing).unwrap(), "replacement");
}

#[tokio::test]
async fn undo_files_moved_restores_each_original_parent() {
    let tmp = TempDir::new().unwrap();
    let original_a_dir = tmp.path().join("a");
    let original_b_dir = tmp.path().join("b");
    let current_dir = tmp.path().join("moved");

    fs::create_dir_all(&original_a_dir).unwrap();
    fs::create_dir_all(&original_b_dir).unwrap();
    fs::create_dir_all(&current_dir).unwrap();

    let original_a = original_a_dir.join("a.txt");
    let original_b = original_b_dir.join("b.txt");
    let current_a = current_dir.join("a.txt");
    let current_b = current_dir.join("b.txt");

    fs::write(&current_a, "alpha").unwrap();
    fs::write(&current_b, "bravo").unwrap();

    let entry = UndoEntry::new(
        7,
        UndoableOperation::FilesMoved {
            moves: vec![
                (original_a.clone(), current_a.clone()),
                (original_b.clone(), current_b.clone()),
            ],
        },
        "Moved files",
    );

    let complete = wait_for_operation_complete(execute_undo(entry)).await;

    assert_eq!(complete.operation_type, OperationType::Move);
    assert_eq!(complete.succeeded, 2);
    assert_eq!(complete.failed, 0);
    assert_eq!(fs::read_to_string(&original_a).unwrap(), "alpha");
    assert_eq!(fs::read_to_string(&original_b).unwrap(), "bravo");
    assert!(!current_a.exists());
    assert!(!current_b.exists());
}

#[tokio::test]
async fn undo_created_directory_preserves_later_contents() {
    let tmp = TempDir::new().unwrap();
    let created = tmp.path().join("created");

    fs::create_dir_all(created.join("nested")).unwrap();
    fs::write(created.join("nested/file.txt"), "temporary").unwrap();

    let entry = UndoEntry::new(
        8,
        UndoableOperation::DirectoryCreated {
            path: created.clone(),
        },
        "Created directory",
    );

    let complete = wait_for_operation_complete(execute_undo(entry)).await;

    assert_eq!(complete.operation_type, OperationType::Delete);
    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 1);
    assert_eq!(complete.errors.len(), 1);
    assert_eq!(complete.errors[0].path, created);
    assert_eq!(
        fs::read_to_string(created.join("nested/file.txt")).unwrap(),
        "temporary"
    );
}

// ---------------------------------------------------------------------------
// No-clobber guarantees
//
// These cover the failure mode where a rename that fails for a reason *other*
// than "different filesystem" falls through to a copy, which then truncates
// whatever already occupies the destination.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn undo_move_refuses_to_overwrite_a_file_recreated_at_the_original_path() {
    let tmp = TempDir::new().unwrap();
    let original = tmp.path().join("notes.txt");
    let moved_to = tmp.path().join("sub").join("notes.txt");
    fs::create_dir_all(moved_to.parent().unwrap()).unwrap();
    fs::write(&moved_to, "OLD MOVED CONTENT").unwrap();

    // The user created an unrelated file where the moved item used to live.
    fs::write(&original, "PRECIOUS NEW USER DATA").unwrap();

    let entry = UndoEntry::new(
        9,
        UndoableOperation::FilesMoved {
            moves: vec![(original.clone(), moved_to.clone())],
        },
        "Moved 1 items",
    );

    let complete = wait_for_operation_complete(execute_undo(entry)).await;

    assert_eq!(complete.succeeded, 0, "undo must not report success here");
    assert_eq!(complete.failed, 1);
    assert_eq!(
        fs::read_to_string(&original).unwrap(),
        "PRECIOUS NEW USER DATA",
        "undo overwrote an unrelated file"
    );
    assert!(moved_to.exists(), "the moved item must be left in place");
}

#[tokio::test]
async fn move_reports_an_error_when_the_destination_appears_mid_flight() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();
    fs::write(source_dir.join("f.txt"), "incoming").unwrap();
    fs::write(dest_dir.join("f.txt"), "EXISTING").unwrap();

    // Overwrite is not requested, so the existing destination must survive.
    let (complete, _pairs, _conflicts) = wait_for_move_complete(start_move(
        vec![source_dir.join("f.txt")],
        dest_dir.clone(),
        MoveOptions {
            conflict_resolution: Some(ConflictResolution::Skip),
        },
        CancellationToken::new(),
    ))
    .await;

    assert_eq!(complete.succeeded, 0);
    assert_eq!(
        fs::read_to_string(dest_dir.join("f.txt")).unwrap(),
        "EXISTING"
    );
}

// ---------------------------------------------------------------------------
// Staging-name length
// ---------------------------------------------------------------------------

#[tokio::test]
async fn copy_succeeds_for_names_at_the_filesystem_length_limit() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();

    // 250 bytes: accepted by validate_filename, and long enough that a naive
    // staging prefix would push the temp name past NAME_MAX.
    let long_name = "a".repeat(250);
    fs::write(source_dir.join(&long_name), "payload").unwrap();

    let (complete, _created, _conflicts) = wait_for_copy_complete(start_copy(
        vec![source_dir.join(&long_name)],
        dest_dir.clone(),
        CopyOptions::default(),
        CancellationToken::new(),
    ))
    .await;

    let errors: Vec<String> = complete.errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(complete.succeeded, 1, "copy failed: {errors:?}");
    assert_eq!(
        fs::read_to_string(dest_dir.join(&long_name)).unwrap(),
        "payload"
    );
}

#[tokio::test]
async fn move_succeeds_for_names_at_the_filesystem_length_limit() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();

    let long_name = "b".repeat(250);
    fs::write(source_dir.join(&long_name), "payload").unwrap();

    let (complete, _pairs, _conflicts) = wait_for_move_complete(start_move(
        vec![source_dir.join(&long_name)],
        dest_dir.clone(),
        MoveOptions::default(),
        CancellationToken::new(),
    ))
    .await;

    let errors: Vec<String> = complete.errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(complete.succeeded, 1, "move failed: {errors:?}");
    assert!(dest_dir.join(&long_name).exists());
}

// ---------------------------------------------------------------------------
// Cancellation reaches inside a single recursive item
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_a_directory_copy_leaves_no_partial_destination() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    let tree = source_dir.join("tree");
    fs::create_dir_all(&tree).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();
    for i in 0..200 {
        fs::write(tree.join(format!("file{i}.bin")), vec![0u8; 4096]).unwrap();
    }

    let token = CancellationToken::new();
    token.cancel(); // pre-cancelled: the item must not be copied at all

    let (complete, _created, _conflicts) = wait_for_copy_complete(start_copy(
        vec![tree.clone()],
        dest_dir.clone(),
        CopyOptions::default(),
        token,
    ))
    .await;

    assert!(complete.cancelled);
    assert!(!complete.is_success());
    assert!(complete.summary().contains("cancelled"));
    assert_eq!(complete.succeeded, 0);
    assert!(
        !dest_dir.join("tree").exists(),
        "a cancelled copy must not leave a partial destination"
    );
    // And no staging directories are left behind.
    let leftovers: Vec<_> = fs::read_dir(&dest_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".gravityfile-tmp")
        })
        .collect();
    assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
}

// ---------------------------------------------------------------------------
// Interactive conflict resolution
//
// The operation stays alive while a conflict is outstanding, so answering one
// item resolves only that item and the rest of the batch proceeds.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn answering_a_conflict_resolves_only_that_item_and_the_batch_continues() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();

    // `clash.txt` conflicts; `fresh.txt` does not.
    fs::write(source_dir.join("clash.txt"), "NEW").unwrap();
    fs::write(source_dir.join("fresh.txt"), "FRESH").unwrap();
    fs::write(dest_dir.join("clash.txt"), "OLD").unwrap();

    let mut rx = start_copy(
        vec![source_dir.join("clash.txt"), source_dir.join("fresh.txt")],
        dest_dir.clone(),
        CopyOptions::default(),
        CancellationToken::new(),
    );

    let mut complete = None;
    let mut conflicts = 0;
    while let Some(result) = rx.recv().await {
        match result {
            CopyResult::Conflict(request) => {
                conflicts += 1;
                request.resolve(ConflictResolution::Overwrite);
            }
            CopyResult::Complete(c) => complete = Some(c),
            CopyResult::Progress(_) => {}
        }
    }

    let complete = complete.expect("copy ended without completing");
    assert_eq!(conflicts, 1, "only the clashing item should prompt");
    assert_eq!(complete.inner.succeeded, 2, "the batch must not be aborted");
    assert_eq!(complete.inner.failed, 0);
    assert_eq!(
        fs::read_to_string(dest_dir.join("clash.txt")).unwrap(),
        "NEW"
    );
    assert_eq!(
        fs::read_to_string(dest_dir.join("fresh.txt")).unwrap(),
        "FRESH"
    );
}

#[tokio::test]
async fn an_apply_to_all_answer_suppresses_later_prompts() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();

    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(source_dir.join(name), "NEW").unwrap();
        fs::write(dest_dir.join(name), "OLD").unwrap();
    }

    let mut rx = start_copy(
        ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|n| source_dir.join(n))
            .collect(),
        dest_dir.clone(),
        CopyOptions::default(),
        CancellationToken::new(),
    );

    let mut complete = None;
    let mut conflicts = 0;
    while let Some(result) = rx.recv().await {
        match result {
            CopyResult::Conflict(request) => {
                conflicts += 1;
                request.resolve(ConflictResolution::OverwriteAll);
            }
            CopyResult::Complete(c) => complete = Some(c),
            CopyResult::Progress(_) => {}
        }
    }

    let complete = complete.expect("copy ended without completing");
    assert_eq!(conflicts, 1, "'overwrite all' must prompt only once");
    assert_eq!(complete.inner.succeeded, 3);
    for name in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(fs::read_to_string(dest_dir.join(name)).unwrap(), "NEW");
    }
}

#[tokio::test]
async fn skipping_one_conflict_still_copies_the_remaining_items() {
    let tmp = TempDir::new().unwrap();
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();
    fs::write(source_dir.join("clash.txt"), "NEW").unwrap();
    fs::write(source_dir.join("fresh.txt"), "FRESH").unwrap();
    fs::write(dest_dir.join("clash.txt"), "OLD").unwrap();

    let mut rx = start_copy(
        vec![source_dir.join("clash.txt"), source_dir.join("fresh.txt")],
        dest_dir.clone(),
        CopyOptions::default(),
        CancellationToken::new(),
    );

    let mut complete = None;
    while let Some(result) = rx.recv().await {
        match result {
            CopyResult::Conflict(request) => request.resolve(ConflictResolution::Skip),
            CopyResult::Complete(c) => complete = Some(c),
            CopyResult::Progress(_) => {}
        }
    }

    let complete = complete.expect("copy ended without completing");
    assert_eq!(complete.inner.succeeded, 1);
    assert_eq!(complete.inner.skipped, 1);
    assert_eq!(complete.inner.failed, 0, "a skip is not a failure");
    assert_eq!(
        fs::read_to_string(dest_dir.join("clash.txt")).unwrap(),
        "OLD"
    );
    assert!(dest_dir.join("fresh.txt").exists());
}

#[tokio::test]
async fn cancelled_move_reports_cancellation_and_preserves_source() {
    let directory = TempDir::new().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    fs::write(&source, b"keep").unwrap();
    fs::create_dir(&destination).unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let (complete, moved, _) = wait_for_move_complete(start_move(
        vec![source.clone()],
        destination.clone(),
        MoveOptions::default(),
        token,
    ))
    .await;
    assert!(complete.cancelled);
    assert!(!complete.is_success());
    assert_eq!(complete.succeeded, 0);
    assert!(moved.is_empty());
    assert_eq!(fs::read(&source).unwrap(), b"keep");
    assert!(!destination.join("source").exists());
}

#[tokio::test]
async fn aborting_copy_and_move_retains_completed_work_and_undo_records() {
    for moving in [false, true] {
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let sources: Vec<_> = ["first", "conflict", "last"]
            .iter()
            .map(|name| {
                let path = directory.path().join(name);
                fs::write(&path, name.as_bytes()).unwrap();
                path
            })
            .collect();
        fs::write(destination.join("conflict"), b"original").unwrap();
        let complete = if moving {
            let (complete, pairs, _) = wait_for_move_complete(start_move(
                sources.clone(),
                destination.clone(),
                MoveOptions {
                    conflict_resolution: Some(ConflictResolution::Abort),
                },
                CancellationToken::new(),
            ))
            .await;
            assert_eq!(pairs, vec![(sources[0].clone(), destination.join("first"))]);
            complete
        } else {
            let (complete, created, _) = wait_for_copy_complete(start_copy(
                sources.clone(),
                destination.clone(),
                CopyOptions {
                    conflict_resolution: Some(ConflictResolution::Abort),
                    ..CopyOptions::default()
                },
                CancellationToken::new(),
            ))
            .await;
            assert_eq!(created, vec![destination.join("first")]);
            complete
        };
        assert!(complete.cancelled);
        assert!(!complete.is_success());
        assert_eq!(
            (complete.succeeded, complete.failed, complete.skipped),
            (1, 0, 0)
        );
        assert!(complete.summary().contains("cancelled"));
        assert_eq!(fs::read(destination.join("first")).unwrap(), b"first");
        assert_eq!(fs::read(destination.join("conflict")).unwrap(), b"original");
        assert!(sources[1].exists() && sources[2].exists());
        assert!(!destination.join("last").exists());
    }
}

#[tokio::test]
async fn executor_preserves_cancellation_while_conflict_request_is_retained() {
    for moving in [false, true] {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"source").unwrap();
        fs::write(destination.join("source"), b"existing").unwrap();
        let executor = gravityfile_ops::OperationExecutor::new();
        let token = CancellationToken::new();
        let mut rx = if moving {
            executor.move_to_with_cancellation(
                vec![source.clone()],
                destination.clone(),
                token.clone(),
            )
        } else {
            executor.copy_with_cancellation(
                vec![source.clone()],
                destination.clone(),
                token.clone(),
            )
        };
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match rx.recv().await.expect("operation must request resolution") {
                    OperationResult::Conflict(request) => break request,
                    OperationResult::Progress(_) => {}
                    OperationResult::Complete(_) => panic!("completed before conflict"),
                }
            }
        })
        .await
        .unwrap();
        token.cancel();
        let complete = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wait_for_operation_complete(rx),
        )
        .await
        .unwrap();
        assert!(complete.cancelled);
        assert!(!complete.is_success());
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
async fn stalled_progress_consumer_does_not_suspend_copy_or_move() {
    for moving in [false, true] {
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        // Exceed the raw operation channel's capacity, even for move's single
        // progress event per item. Do not read any progress until all items exist.
        let sources: Vec<_> = (0..150)
            .map(|index| {
                let path = directory.path().join(format!("item-{index}"));
                fs::write(&path, b"payload").unwrap();
                path
            })
            .collect();
        let copy_rx = if !moving {
            Some(start_copy(
                sources.clone(),
                destination.clone(),
                CopyOptions::default(),
                CancellationToken::new(),
            ))
        } else {
            None
        };
        let move_rx = if moving {
            Some(start_move(
                sources.clone(),
                destination.clone(),
                MoveOptions::default(),
                CancellationToken::new(),
            ))
        } else {
            None
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !destination.join("item-149").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("filesystem work must not wait for progress consumption");
        let complete = if let Some(rx) = copy_rx {
            wait_for_copy_complete(rx).await.0
        } else {
            wait_for_move_complete(move_rx.unwrap()).await.0
        };
        assert!(complete.is_success());
        assert_eq!(complete.succeeded, 150);
        assert_eq!(fs::read(destination.join("item-149")).unwrap(), b"payload");
    }
}

#[tokio::test]
async fn dropping_executor_receiver_cancels_pending_work() {
    for moving in [false, true] {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("source"), b"existing").unwrap();
        let token = CancellationToken::new();
        let executor = gravityfile_ops::OperationExecutor::new();
        let rx = if moving {
            executor.move_to_with_cancellation(
                vec![source.clone()],
                destination.clone(),
                token.clone(),
            )
        } else {
            executor.copy_with_cancellation(
                vec![source.clone()],
                destination.clone(),
                token.clone(),
            )
        };
        drop(rx);
        tokio::time::timeout(std::time::Duration::from_secs(5), token.cancelled())
            .await
            .unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read(destination.join("source")).unwrap(), b"existing");
    }
}

#[tokio::test]
async fn undo_creation_removes_only_empty_items_of_the_expected_type() {
    for directory in [false, true] {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("created");
        if directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, []).unwrap();
        }
        let operation = if directory {
            UndoableOperation::DirectoryCreated { path: path.clone() }
        } else {
            UndoableOperation::FileCreated { path: path.clone() }
        };
        let result =
            wait_for_operation_complete(execute_undo(UndoEntry::new(0, operation, "create"))).await;
        assert!(result.is_success());
        assert_eq!(result.succeeded, 1);
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn undo_creation_preserves_changed_types_and_nonempty_files() {
    for case in ["file-now-directory", "directory-now-file", "file-with-data"] {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("created");
        if case == "file-now-directory" {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"later content").unwrap();
        }
        let operation = if case == "directory-now-file" {
            UndoableOperation::DirectoryCreated { path: path.clone() }
        } else {
            UndoableOperation::FileCreated { path: path.clone() }
        };
        let result =
            wait_for_operation_complete(execute_undo(UndoEntry::new(0, operation, "create"))).await;
        assert!(!result.is_success(), "{case}");
        assert_eq!(result.failed, 1);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].path, path);
        if case == "file-now-directory" {
            assert!(path.is_dir());
        } else {
            assert_eq!(fs::read(&path).unwrap(), b"later content");
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn undo_creation_preserves_replacement_symlinks_and_their_targets() {
    for directory in [false, true] {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target");
        let path = temp.path().join("created");
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let operation = if directory {
            UndoableOperation::DirectoryCreated { path: path.clone() }
        } else {
            UndoableOperation::FileCreated { path: path.clone() }
        };
        let result =
            wait_for_operation_complete(execute_undo(UndoEntry::new(0, operation, "create"))).await;
        assert!(!result.is_success());
        assert!(path.symlink_metadata().unwrap().is_symlink());
        assert_eq!(fs::read(target).unwrap(), b"keep");
    }
}

#[tokio::test]
async fn undo_copy_reports_missing_paths_and_unsupported_delete_is_failure() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("missing");
    let operation = UndoableOperation::FilesCopied {
        created: vec![missing.clone()],
    };
    let result =
        wait_for_operation_complete(execute_undo(UndoEntry::new(0, operation, "copy"))).await;
    assert_eq!(result.failed, 1);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].path, missing);
    let operation = UndoableOperation::FilesDeleted {
        paths: vec![missing],
    };
    let result =
        wait_for_operation_complete(execute_undo(UndoEntry::new(1, operation, "delete"))).await;
    assert!(!result.is_success());
    assert_eq!(result.failed, 1);
    assert_eq!(result.errors.len(), 1);
}

#[tokio::test]
async fn precancelled_undo_preserves_all_operation_targets() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("current");
    let original = directory.path().join("original");
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
        UndoableOperation::FileRenamed {
            path: path.clone(),
            old_path: original.clone(),
            new_name: "current".into(),
        },
    ];
    for operation in operations {
        let token = CancellationToken::new();
        token.cancel();
        let result = wait_for_operation_complete(gravityfile_ops::execute_undo_with_cancellation(
            UndoEntry::new(0, operation, "undo"),
            token,
        ))
        .await;
        assert!(result.cancelled);
        assert_eq!((result.succeeded, result.failed), (0, 0));
        assert!(path.exists());
        assert!(!original.exists());
    }
}

#[tokio::test]
async fn undo_rename_restores_full_original_path_without_lossy_names() {
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt;
    let directory = TempDir::new().unwrap();
    let original_parent = directory.path().join("original-parent");
    fs::create_dir(&original_parent).unwrap();
    // Linux permits arbitrary filename bytes; APFS rejects invalid UTF-8.
    #[cfg(target_os = "linux")]
    let name = std::ffi::OsString::from_vec(b"original-\xff".to_vec());
    #[cfg(not(target_os = "linux"))]
    let name = std::ffi::OsString::from("original-é");
    let original = original_parent.join(name);
    let current = directory.path().join("current");
    fs::write(&current, b"preserved").unwrap();
    let entry = UndoEntry::new(
        0,
        UndoableOperation::FileRenamed {
            path: current.clone(),
            old_path: original.clone(),
            new_name: "current".into(),
        },
        "rename",
    );
    let result = wait_for_operation_complete(execute_undo(entry)).await;
    assert!(result.is_success(), "{:?}", result.errors);
    assert_eq!(fs::read(original).unwrap(), b"preserved");
    assert!(!current.exists());
}

#[tokio::test]
async fn undo_copy_retains_later_edits_and_children_and_restores_without_clobbering() {
    use gravityfile_ops::{RecoveryPurpose, discover_recovery, restore_original};
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(source.join("original"), b"copied bytes").unwrap();
    let (copy, created, _) = wait_for_copy_complete(start_copy(
        vec![source],
        destination.clone(),
        CopyOptions::default(),
        CancellationToken::new(),
    ))
    .await;
    assert!(copy.is_success());
    let copied = &created[0];
    fs::write(copied.join("original"), b"later edit").unwrap();
    fs::write(copied.join("new-child"), b"later addition").unwrap();
    let completion = wait_for_operation_complete(execute_undo(UndoEntry::new(
        0,
        UndoableOperation::FilesCopied {
            created: created.clone(),
        },
        "copy",
    )))
    .await;
    assert!(completion.is_success(), "{completion:?}");
    assert_eq!(completion.operation_type, OperationType::Retain);
    assert!(completion.summary().contains("Retained for recovery"));
    assert_eq!(completion.succeeded, 1);
    assert!(!copied.exists());
    let records = discover_recovery(&destination).unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.purpose, RecoveryPurpose::CopyUndo);
    assert!(!record.published);
    assert!(record.original_available);
    assert!(!record.payload_available);
    assert_eq!(
        fs::read(record.transaction.join("original/original")).unwrap(),
        b"later edit"
    );
    assert_eq!(
        fs::read(record.transaction.join("original/new-child")).unwrap(),
        b"later addition"
    );
    fs::write(copied, b"competing data").unwrap();
    assert!(restore_original(&record.transaction).is_err());
    assert_eq!(fs::read(copied).unwrap(), b"competing data");
    fs::remove_file(copied).unwrap();
    assert_eq!(
        restore_original(&record.transaction).unwrap(),
        copied.canonicalize().unwrap()
    );
    assert_eq!(fs::read(copied.join("original")).unwrap(), b"later edit");
    assert_eq!(
        fs::read(copied.join("new-child")).unwrap(),
        b"later addition"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn undo_copy_retains_symlink_itself_including_dangling_links() {
    use gravityfile_ops::{discover_recovery, restore_original};
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target");
    fs::write(&target, b"unrelated data").unwrap();
    for name in ["target", "missing"] {
        let path = temp.path().join(format!("link-{name}"));
        std::os::unix::fs::symlink(name, &path).unwrap();
        let completion = wait_for_operation_complete(execute_undo(UndoEntry::new(
            0,
            UndoableOperation::FilesCopied {
                created: vec![path.clone()],
            },
            "copy",
        )))
        .await;
        assert!(completion.is_success());
        assert!(fs::symlink_metadata(&path).is_err());
        let records = discover_recovery(temp.path()).unwrap();
        let record = records
            .iter()
            .find(|record| record.destination.file_name() == path.file_name())
            .unwrap();
        assert_eq!(
            fs::read_link(record.transaction.join("original")).unwrap(),
            PathBuf::from(name)
        );
        restore_original(&record.transaction).unwrap();
        assert_eq!(fs::read_link(&path).unwrap(), PathBuf::from(name));
        assert_eq!(fs::read(&target).unwrap(), b"unrelated data");
    }
}
