# gravityfile-scan

The directory scanner behind [gravityfile](https://github.com/epistates/gravityfile),
plus readers and writers for saved scans.

This crate exists for the `grav` binary and its sibling crates. Its API
follows their needs and can change in any minor release.

- `JwalkScanner` walks a directory in parallel and builds a
  `gravityfile_core::FileTree`. Sizes are allocated blocks by default or
  apparent lengths on request, hard links count once, and a directory's size
  covers its whole subtree even when a depth limit drops its children.
  Unreadable entries become warnings instead of errors. Progress is
  broadcast to subscribers, and a `CancellationToken` stops the scan.
- `snapshot::read` opens a saved scan: a `grav export` snapshot or an
  `ncdu -o` / `gdu -o` dump, either optionally zstd-compressed.
- `ncdu::write` writes a tree as an ncdu dump, so `ncdu -f` can browse it.
  Both ncdu directions stream and handle names that are not UTF-8.
- `quick_list` lists one directory without descending, for interactive use.

```rust
use gravityfile_scan::snapshot::{self, ImportOptions};
use gravityfile_scan::{JwalkScanner, ScanConfig, ncdu};
use tokio_util::sync::CancellationToken;

let dir = tempfile::tempdir()?;
std::fs::write(dir.path().join("app.log"), vec![0; 10_000])?;

let mut config = ScanConfig::new(dir.path());
config.apparent_size = true;
let tree = JwalkScanner::new().scan(&config, CancellationToken::new())?;
assert_eq!(tree.total_files(), 1);
assert_eq!(tree.root.children[0].size, 10_000);

let mut dump = Vec::new();
ncdu::write(&tree, &mut dump)?;
let options = ImportOptions { apparent: true, ..Default::default() };
let restored = snapshot::read_from(dump.as_slice(), &options)?;
assert_eq!(restored.root.children[0].size, 10_000);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Licence

MIT or Apache-2.0, at your option.
