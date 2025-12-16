# gravityfile-ops

Async file operations engine for gravityfile with progress reporting and undo support.

This crate provides the file operations layer (copy, move, rename, create, delete) with conflict handling, progress updates via channels, and an undo log.

## Features

- **Async Operations** - All operations run asynchronously with tokio
- **Progress Reporting** - Real-time progress updates via mpsc channels
- **Conflict Detection** - Handles file/directory collisions with multiple resolution strategies
- **Undo Support** - Records operations for undo (with trash integration)
- **Safe Deletion** - Uses `trash` crate for recoverable deletions

## Usage

### Copy Files

```rust
use gravityfile_ops::{start_copy, CopyOptions, CopyResult};
use std::path::PathBuf;

let sources = vec![PathBuf::from("/path/to/file.txt")];
let destination = PathBuf::from("/path/to/destination");
let options = CopyOptions::default();

let mut rx = start_copy(sources, destination, options);

while let Some(result) = rx.recv().await {
    match result {
        CopyResult::Progress(progress) => {
            println!("{:.1}% complete", progress.percentage());
        }
        CopyResult::Conflict(conflict) => {
            println!("Conflict: {} at {}", conflict.kind, conflict.destination.display());
        }
        CopyResult::Complete(complete) => {
            println!("{}", complete.summary());
        }
    }
}
```

### Move Files

```rust
use gravityfile_ops::{start_move, MoveOptions, MoveResult};
use std::path::PathBuf;

let sources = vec![PathBuf::from("/path/to/file.txt")];
let destination = PathBuf::from("/path/to/destination");
let options = MoveOptions::default();

let mut rx = start_move(sources, destination, options);

while let Some(result) = rx.recv().await {
    match result {
        MoveResult::Progress(progress) => {
            println!("Moving: {:?}", progress.current_file);
        }
        MoveResult::Complete(complete) => {
            println!("{}", complete.summary());
        }
        _ => {}
    }
}
```

### Rename

```rust
use gravityfile_ops::{start_rename, RenameResult};
use std::path::PathBuf;

let source = PathBuf::from("/path/to/old_name.txt");
let new_name = "new_name.txt";

let mut rx = start_rename(source, new_name.to_string());

while let Some(result) = rx.recv().await {
    match result {
        RenameResult::Complete(complete) => {
            if complete.is_success() {
                println!("Renamed successfully");
            }
        }
        _ => {}
    }
}
```

### Create File/Directory

```rust
use gravityfile_ops::{start_create_file, start_create_directory, CreateResult};
use std::path::PathBuf;

// Create a file
let mut rx = start_create_file(PathBuf::from("/path/to/new_file.txt"));
while let Some(CreateResult::Complete(c)) = rx.recv().await {
    println!("{}", c.summary());
}

// Create a directory
let mut rx = start_create_directory(PathBuf::from("/path/to/new_dir"));
while let Some(CreateResult::Complete(c)) = rx.recv().await {
    println!("{}", c.summary());
}
```

### Undo Log

```rust
use gravityfile_ops::{UndoLog, UndoableOperation, execute_undo};
use std::path::PathBuf;

let mut undo_log = UndoLog::new(100); // Keep last 100 operations

// Record operations as they happen
undo_log.record_create_file(PathBuf::from("/path/to/file.txt"));
undo_log.record_rename(
    PathBuf::from("/path/to/new.txt"),
    "old.txt".to_string(),
    "new.txt".to_string(),
);

// Undo the most recent operation
if let Some(entry) = undo_log.pop() {
    let result = execute_undo(&entry.operation).await?;
    println!("Undone: {}", entry.description);
}
```

## Types

### FileOperation

Represents an operation to be executed:

```rust
pub enum FileOperation {
    Copy { sources: Vec<PathBuf>, destination: PathBuf },
    Move { sources: Vec<PathBuf>, destination: PathBuf },
    Rename { source: PathBuf, new_name: String },
    Delete { targets: Vec<PathBuf>, use_trash: bool },
    CreateFile { path: PathBuf },
    CreateDirectory { path: PathBuf },
}
```

### ConflictResolution

How to handle conflicts during operations:

```rust
pub enum ConflictResolution {
    Skip,          // Skip this item
    Overwrite,     // Replace existing
    AutoRename,    // Rename to "file (1).txt"
    SkipAll,       // Skip all remaining conflicts
    OverwriteAll,  // Overwrite all remaining conflicts
    Abort,         // Cancel the operation
}
```

### OperationProgress

Progress information for ongoing operations:

```rust
pub struct OperationProgress {
    pub operation_type: OperationType,
    pub files_completed: usize,
    pub files_total: usize,
    pub bytes_processed: u64,
    pub bytes_total: u64,
    pub current_file: Option<PathBuf>,
    pub errors: Vec<OperationError>,
}
```

### UndoableOperation

Operations that can be reversed:

```rust
pub enum UndoableOperation {
    FilesMoved { moves: Vec<(PathBuf, PathBuf)> },
    FilesCopied { created: Vec<PathBuf> },
    FilesDeleted { trash_entries: Vec<(PathBuf, PathBuf)> },
    FileRenamed { path: PathBuf, old_name: String, new_name: String },
    FileCreated { path: PathBuf },
    DirectoryCreated { path: PathBuf },
}
```

## Architecture

The crate is organized into focused modules:

- **operation.rs** - Core `FileOperation` enum and error types
- **copy.rs** - Async copy with recursive directory support
- **move_op.rs** - Move operations (try rename, fallback to copy+delete)
- **rename.rs** - Single item rename
- **create.rs** - File and directory creation
- **conflict.rs** - Conflict detection and resolution strategies
- **progress.rs** - Progress tracking types
- **undo.rs** - Undo log with configurable depth
- **executor.rs** - High-level operation execution and undo execution

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
