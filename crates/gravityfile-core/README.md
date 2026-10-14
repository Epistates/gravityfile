# gravityfile-core

The data model shared by [gravityfile](https://github.com/epistates/gravityfile),
a disk usage analyzer and file explorer: the scanned tree, scan settings,
size formatting, configuration paths and scan history.

This crate exists for the `grav` binary and its sibling crates. Its API
follows their needs and can change in any minor release.

- `FileTree` is a scanned directory: a `FileNode` root, the absolute
  `root_path`, `TreeStats` totals and the warnings collected on the way.
- `FileNode` is a file, directory or symlink. Directories hold their
  children sorted largest first and carry the size of their whole subtree.
- `ScanConfig` holds the scan settings: depth, hidden files, ignore
  patterns, symlinks, filesystem boundaries and hard-link counting.
- `units` formats and parses sizes and ages, with SI units by default.
- `paths` finds the configuration and data directories, honouring
  `GRAVITYFILE_HOME` and the platform conventions.
- `history` appends and reads the one-line scan summaries behind
  `grav history`.

```rust
use gravityfile_core::units::{Units, format_size, parse_size};
use gravityfile_core::{FileNode, FileTree, ScanConfig, Timestamps, TreeStats};
use std::path::Path;
use std::time::{Duration, SystemTime};

let modified = Timestamps::with_modified(SystemTime::now());
let mut tree = FileTree::new(
    FileNode::new_directory("project", modified),
    "/project".into(),
    ScanConfig::new("/project"),
    TreeStats::new(),
    Duration::ZERO,
    Vec::new(),
);
let file = FileNode::new_file("data.bin", 3_000_000, 5_864, modified, false);
assert!(tree.insert(Path::new("/project"), file));
tree.recount();
assert_eq!(tree.total_files(), 1);
assert_eq!(tree.total_size(), 3_000_000);

assert_eq!(format_size(3_000_000, Units::Si), "3.00 MB");
assert_eq!(format_size(3_000_000, Units::Binary), "2.86 MiB");
assert_eq!(parse_size("1.5G").unwrap(), 1_500_000_000);
```

Most code gets a `FileTree` from `gravityfile-scan` instead of building one.

## Licence

MIT or Apache-2.0, at your option.
