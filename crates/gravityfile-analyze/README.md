# gravityfile-analyze

Analyses of scanned trees for [gravityfile](https://github.com/epistates/gravityfile):
duplicates, file age, the largest files, space by type and the difference
between two scans.

This crate exists for the `grav` binary and its sibling crates. Its API
follows their needs and can change in any minor release.

- `DuplicateFinder` groups files with identical content. It narrows
  candidates by size, then by a hash of each file's first and last bytes, and
  only then hashes whole files with BLAKE3, in parallel. Hard links to one
  file are not reported as duplicates of each other.
- `AgeAnalyzer` sorts files into age buckets and finds stale directories,
  where nothing has changed for a long time.
- `largest_files` and `by_type` look across the whole tree, wherever the
  files are.
- `diff` compares two trees of the same place: what grew, shrank, appeared
  or went, largest changes first in each directory.

Every analysis takes a `CancellationToken` and reports whether its result is
complete, so a partial scan never passes for a full one.

```rust
use gravityfile_analyze::{Change, DiffOptions, DuplicateConfig, DuplicateFinder, by_type, diff};
use gravityfile_scan::{JwalkScanner, ScanConfig};
use tokio_util::sync::CancellationToken;

let dir = tempfile::tempdir()?;
std::fs::write(dir.path().join("a.txt"), "same text")?;
std::fs::write(dir.path().join("b.txt"), "same text")?;
let mut config = ScanConfig::new(dir.path());
config.apparent_size = true;
let scan = || JwalkScanner::new().scan(&config, CancellationToken::new());
let before = scan()?;

let finder = DuplicateFinder::with_config(DuplicateConfig::builder().min_size(0u64).build()?)?;
let duplicates = finder.find_duplicates(&before, CancellationToken::new());
assert_eq!(duplicates.groups.len(), 1);
assert_eq!(duplicates.total_wasted_space, 9);

let types = by_type(&before, &CancellationToken::new()).expect("not cancelled");
assert_eq!(types[0].extension.as_deref(), Some("txt"));

std::fs::write(dir.path().join("c.bin"), vec![0; 1_000])?;
let after = scan()?;
let options = DiffOptions { depth: 1, limit: 0 };
let changes = diff(&before.root, &after.root, options, &CancellationToken::new()).expect("not cancelled");
let added = &changes.entries[0];
assert_eq!(added.path, std::path::Path::new("c.bin"));
assert_eq!(added.change, Change::Added);
assert_eq!(added.delta, 1_000);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Licence

MIT or Apache-2.0, at your option.
