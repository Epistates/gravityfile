# gravityfile-core

Core types and traits for the gravityfile ecosystem.

This crate provides the fundamental data structures used throughout gravityfile, including file nodes, trees, configuration, and error types.

## Features

- **FileNode** - Represents files, directories, and symlinks with metadata
- **FileTree** - Container for scanned directory trees with statistics
- **ScanConfig** - Configuration for directory scanning operations
- **Content Hashing** - BLAKE3-based content hashing for duplicate detection
- **Full Serialization** - All types implement Serde for JSON export

## Types

### FileNode

Represents a single file system entry:

```rust
use gravityfile_core::{FileNode, Timestamps};
use std::time::SystemTime;

// Create a file node
let file = FileNode::new_file(
    "example.txt",
    1024,  // size in bytes
    2,     // 512-byte blocks allocated
    Timestamps::with_modified(SystemTime::now()),
    false, // not executable
);

// Create a directory node
let dir = FileNode::new_directory("my_folder", Timestamps::with_modified(SystemTime::now()));
```

### FileTree

Container for scan results:

```rust
use gravityfile_core::FileTree;

// FileTree is typically created by gravityfile-scan
// It contains:
// - root: FileNode - the root directory node
// - root_path: PathBuf - absolute path that was scanned
// - stats: TreeStats - summary statistics
// - warnings: Vec<ScanWarning> - issues encountered during scan
```

### ScanConfig

Configuration for scanning operations:

```rust
use gravityfile_core::ScanConfig;

// Simple configuration
let config = ScanConfig::new("/path/to/scan");

// Advanced configuration with builder
let config = ScanConfig::builder()
    .root("/path/to/scan")
    .max_depth(Some(10))
    .include_hidden(false)
    .follow_symlinks(false)
    .cross_filesystems(false)
    .ignore_patterns(vec![".git".into(), "node_modules".into()])
    .build()
    .unwrap();
```

### TreeStats

Summary statistics for a scanned tree:

```rust
use gravityfile_core::TreeStats;

// TreeStats provides:
// - total_size: u64 - total bytes
// - total_files: u64 - file count
// - total_dirs: u64 - directory count
// - total_symlinks: u64 - symlink count
// - max_depth: u32 - deepest nesting level
// - largest_file: Option<(PathBuf, u64)>
// - oldest_file: Option<(PathBuf, SystemTime)>
// - newest_file: Option<(PathBuf, SystemTime)>
```

## Error Handling

```rust
use gravityfile_core::{ScanError, ScanWarning, WarningKind};

// ScanError variants:
// - Io { path, source } - I/O errors
// - NotADirectory { path } - path is not a directory
// - Config { message } - invalid configuration

// ScanWarning for non-fatal issues:
// - ReadError - couldn't read a file/directory
// - MetadataError - couldn't get metadata
// - BrokenSymlink - symlink target doesn't exist
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.


### Native filename components

`FileNode.name` is a Unicode display label. Use `FileNode::file_name()` for
filesystem path construction, and `set_file_name(&OsStr)` when creating nodes
from filesystem entries. Non-Unicode components are retained in the optional
`native_name` field. Its serialized representation is platform-specific; legacy
nodes without it continue to use `name`. Direct Rust struct literals must include
the new field. Display labels may collide even when native components differ.
Scan and analysis report paths use `path_serde` adapters to retain Unicode
strings and encode non-Unicode paths as tagged native values; see
[report formats](../../docs/REPORTS.md).


Use `FileNode::display_name()` for terminal-facing labels. It preserves ordinary
Unicode and escapes invalid platform-encoded bytes, controls, and backslashes
distinctly. `name` remains the compatibility label; `file_name()` remains the
exact filesystem component. The display string must never be parsed back into
a path.

### Deep tree lifecycle and field ownership

`FileNode` uses iterative traversal for cloning, count updates, child sorting,
and destruction. The destructor drains descendants through heap-backed iterator
frames; memory for those frames grows with depth. Recursive Serde remains a
separate depth limit.

`FileNode` debug formatting prints local metadata and `child_count`, omitting
descendant contents in both compact and pretty modes. Logging a node therefore
does not traverse the tree or consume stack proportional to tree depth. This
changes diagnostic text, not serialization. Local metadata such as names and
symlink targets can still be large; there is no fixed byte limit on a debug line.

`FileNode` now implements `Drop`, which changes Rust source-level field moves.
Borrow fields when possible. To transfer owned children without cloning, use
`let children = std::mem::take(&mut node.children);` with a mutable node. Use
`Option::take` or `std::mem::replace` as appropriate for other owned fields.
The serialized field layout is unchanged by the destructor.
