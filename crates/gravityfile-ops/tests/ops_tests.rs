use std::fs;
use std::path::PathBuf;

use gravityfile_ops::{
    ConflictResolution, CopyOptions, CopyResult, MoveOptions, MoveResult, OperationComplete,
    OperationResult, OperationType, UndoEntry, UndoableOperation, execute_undo, start_copy,
    start_move,
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

    let rx = start_copy(
        vec![source.clone()],
        destination.clone(),
        CopyOptions::default(),
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

    assert_eq!(conflicts, 1);
    assert_eq!(complete.succeeded, 0);
    assert_eq!(complete.failed, 1);
    assert!(moved_pairs.is_empty());
    assert_eq!(fs::read_to_string(&source).unwrap(), "source");
    assert_eq!(fs::read_to_string(&existing).unwrap(), "destination");
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
async fn undo_created_directory_removes_nested_contents() {
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
    assert_eq!(complete.succeeded, 1);
    assert_eq!(complete.failed, 0);
    assert!(!created.exists());
}
