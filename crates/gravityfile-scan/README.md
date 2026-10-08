# gravityfile-scan

High-performance parallel file system scanner for gravityfile.

This crate provides the scanning engine that traverses directories and builds the file tree structure used by other gravityfile components.

## Features

- **Parallel Traversal** - Uses jwalk for multi-threaded directory scanning
- **Progress Updates** - Subscribe to real-time scan progress via broadcast channels
- **Hardlink Detection** - Tracks inodes to avoid double-counting hardlinked files
- **Cross-Filesystem Support** - Optionally traverse mount points
- **Ignore Patterns** - Skip directories matching configured patterns
- **Size Aggregation** - Automatically calculates directory sizes from contents

## Usage

### Basic Scan

```rust
use gravityfile_scan::{JwalkScanner, ScanConfig};
use tokio_util::sync::CancellationToken;

let config = ScanConfig::new("/path/to/scan");
let scanner = JwalkScanner::new();
let tree = scanner.scan(&config, CancellationToken::new())?;

println!("Total size: {} bytes", tree.total_size());
println!("Total files: {}", tree.total_files());
println!("Total directories: {}", tree.total_dirs());
```

### With Progress Updates

```rust
use gravityfile_scan::{JwalkScanner, ScanConfig, ScanProgress};
use tokio_util::sync::CancellationToken;

let scanner = JwalkScanner::new();
let mut progress_rx = scanner.subscribe();

// Spawn task to handle progress
tokio::spawn(async move {
    while let Ok(progress) = progress_rx.recv().await {
        println!(
            "Scanned {} files, {} dirs ({} bytes)",
            progress.files_scanned,
            progress.dirs_scanned,
            progress.bytes_scanned
        );
    }
});

let config = ScanConfig::new("/path/to/scan");
let tree = scanner.scan(&config, CancellationToken::new())?;
```

### Custom Configuration

```rust
use gravityfile_scan::{JwalkScanner, ScanConfig};
use tokio_util::sync::CancellationToken;

let config = ScanConfig::builder()
    .root("/path/to/scan")
    .max_depth(Some(5))           // Limit depth
    .max_entries(Some(100_000))    // Fail if the scope exceeds this budget
    .include_hidden(false)         // Skip hidden files
    .follow_symlinks(false)        // Don't follow symlinks
    .cross_filesystems(false)      // Stay on same filesystem
    .ignore_patterns(vec![
        ".git".into(),
        "node_modules".into(),
        "target".into(),
    ])
    .threads(0)                    // 0 = auto-detect CPU count
    .apparent_size(false)          // Use disk usage, not apparent size
    .build()?;

let scanner = JwalkScanner::new();
let tree = scanner.scan(&config, CancellationToken::new())?;
```

`max_entries` applies to descendant entries yielded after hidden/ignore
filtering, at every depth (`max_depth` does not reduce it); the root is excluded. Metadata failures and unsupported file types still consume
the budget. `None` (the default) is unlimited; zero permits an empty scope.
Both recursive scans and `quick_list` fail with `ScanError::EntryLimitExceeded`
on the first excess entry, without returning a partial tree. The CLI report commands expose this as `--max-entries`; the TUI uses the saved
`max_scan_entries` setting. It does not cap walker prefetching,
Git status work, total memory, or filesystem-call duration.

## Output Structure

The scanner produces a `FileTree` containing:

- **root** - Root `FileNode` with nested children
- **root_path** - Canonical path that was scanned
- **stats** - Summary statistics (total size, file count, etc.)
- **warnings** - Non-fatal issues encountered during scan
- **scan_duration** - How long the scan took
- **config** - Configuration used for the scan

Children are automatically sorted by size (largest first) for efficient display.

## Size accounting

- Sizes are allocated disk usage (`blocks × 512`) unless `apparent_size` is set.
- Directories include their own allocation, like `du`.
- A file with several hard links counts once, in both size modes, unless
  `count_hard_links` is set. When following symlinks, an object reached through
  several aliases also counts once.
- `max_depth` limits which nodes are kept, not what is counted: directories at
  the deepest kept level still report the full size of their contents.
- FIFOs, sockets and device files are listed as `NodeKind::Other`.
- Unreadable directories and entries are reported as `PermissionDenied`
  warnings, so a partial size is never silent.
- Git status is only computed when `git_status` is set.

## Performance

- File metadata is read on jwalk's worker threads, so the consumer thread does
  no system calls; it builds the tree in one pass from jwalk's depth-first
  order, with no intermediate index of paths.
- Scans share a dedicated thread pool, separate from rayon's global pool.
- Hard-linked identities are retained for the scan's lifetime.
- Progress updates are sent every 1024 entries.

## Re-exports

This crate re-exports common types from `gravityfile-core` for convenience:

```rust
use gravityfile_scan::{
    FileNode, FileTree, NodeKind,
    ScanConfig, ScanError, ScanWarning,
    Timestamps, TreeStats, WarningKind,
};
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
