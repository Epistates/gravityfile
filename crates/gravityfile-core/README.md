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
use gravityfile_core::{FileNode, NodeId, Timestamps, NodeKind};
use std::time::SystemTime;

// Create a file node
let file = FileNode::new_file(
    NodeId::new(1),
    "example.txt",
    1024,  // size in bytes
    2,     // disk blocks
    Timestamps::with_modified(SystemTime::now()),
    false, // not executable
);

// Create a directory node
let dir = FileNode::new_directory(
    NodeId::new(2),
    "my_folder",
    Timestamps::with_modified(SystemTime::now()),
);
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
