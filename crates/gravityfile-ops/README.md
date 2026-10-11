# gravityfile-ops

File operations for [gravityfile](https://github.com/epistates/gravityfile):
copy, move, rename, create, delete, undo and archives.

This crate exists for the `grav` binary and its sibling crates. Its API
follows their needs and can change in any minor release.

Long-running operations run on blocking threads and report through a
channel: best-effort `OperationProgress` snapshots, interactive
`ConflictRequest`s, and one reliable completion message. Dropping the
receiver cancels the operation.

## Safety model

- New content is assembled in a private staging directory beside its
  destination and published with a single no-clobber rename, so a failed
  or cancelled operation never leaves a partial item at a visible path and
  never silently replaces anything.
- Overwrites the user approves move the replaced item to the system Trash
  (or delete it permanently, if configured) only after the replacement is
  complete.
- Copies skip entries they cannot read and report each one instead of
  discarding everything that did copy; cross-filesystem moves keep the
  source unless every entry arrived.
- Completed operations return what an undo needs as an `UndoEntry`.
  `execute_undo` moves and renames items back, sends copies to the Trash,
  and removes created files and directories only while they are still
  empty.

```rust
use gravityfile_ops::{CancellationToken, TransferOptions, TransferResult, start_copy};

let dir = tempfile::tempdir()?;
let source = dir.path().join("notes.txt");
let backup = dir.path().join("backup");
std::fs::write(&source, "hello")?;
std::fs::create_dir(&backup)?;

let runtime = tokio::runtime::Runtime::new()?;
let complete = runtime.block_on(async {
    let mut events = start_copy(
        vec![source],
        backup.clone(),
        TransferOptions::default(),
        CancellationToken::new(),
    );
    while let Some(event) = events.recv().await {
        match event {
            TransferResult::Progress(_) => {}
            // Dropping a request skips that item.
            TransferResult::Conflict(_) => {}
            TransferResult::Complete(complete) => return Some(complete),
        }
    }
    None
});

let complete = complete.expect("copy completes");
assert_eq!(complete.inner.succeeded, 1);
assert_eq!(std::fs::read_to_string(backup.join("notes.txt"))?, "hello");
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Licence

MIT or Apache-2.0, at your option.
