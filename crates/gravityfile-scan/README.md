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

let config = ScanConfig::new("/path/to/scan");
let scanner = JwalkScanner::new();
let tree = scanner.scan(&config)?;

println!("Total size: {} bytes", tree.total_size());
println!("Total files: {}", tree.total_files());
println!("Total directories: {}", tree.total_dirs());
```

### With Progress Updates

```rust
use gravityfile_scan::{JwalkScanner, ScanConfig, ScanProgress};

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
let tree = scanner.scan(&config)?;
```

### Custom Configuration

```rust
use gravityfile_scan::{JwalkScanner, ScanConfig};

let config = ScanConfig::builder()
    .root("/path/to/scan")
    .max_depth(Some(5))           // Limit depth
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
let tree = scanner.scan(&config)?;
```

## Output Structure

The scanner produces a `FileTree` containing:

- **root** - Root `FileNode` with nested children
- **root_path** - Canonical path that was scanned
- **stats** - Summary statistics (total size, file count, etc.)
- **warnings** - Non-fatal issues encountered during scan
- **scan_duration** - How long the scan took
- **config** - Configuration used for the scan

Children are automatically sorted by size (largest first) for efficient display.

## Performance

- Uses rayon thread pool via jwalk for parallel directory traversal
- Hardlink tracking uses DashMap for lock-free concurrent access
- Progress updates are batched (every 1000 files) to reduce overhead
- Memory-efficient: only stores aggregated tree, not individual paths

## Re-exports

This crate re-exports common types from `gravityfile-core` for convenience:

```rust
use gravityfile_scan::{
    FileNode, FileTree, NodeId, NodeKind,
    ScanConfig, ScanError, ScanWarning,
    Timestamps, TreeStats, WarningKind,
};
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
