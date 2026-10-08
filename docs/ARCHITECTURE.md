# Architecture

A Cargo workspace: one binary crate (`grav`) over six libraries.

```
gravityfile (bin: grav)
├── gravityfile-tui      explorer
│   ├── gravityfile-ops      file operations
│   └── gravityfile-plugin   Lua plugin host and workers
├── gravityfile-analyze  duplicates, ages
├── gravityfile-scan     parallel scanner, Git status
└── gravityfile-core     shared types: FileNode, FileTree, ScanConfig, units
```

## `gravityfile-core`

`FileNode` is one entry: name (with the exact native name when it is not valid
UTF-8), kind, size, allocated blocks, modification time, identity of multiply
linked files, Git status, and children sorted by size. Nodes are kept small
(a test bounds them) because a scan holds millions. `FileTree` adds the root
path, the configuration, statistics and warnings. `units` formats and parses
sizes and ages for every frontend. Paths in JSON use `path_serde`, which keeps
non-Unicode paths exact.

## `gravityfile-scan`

`JwalkScanner` walks directories in parallel with jwalk on a dedicated Rayon
pool. Metadata is read on the walker's threads inside `process_read_dir`, along
with ignore filtering, filesystem-boundary pruning and symlink resolution, so
the consumer makes no per-entry system calls. jwalk yields entries in
depth-first order, which lets the consumer build the tree in one pass with a
stack of open directories, aggregating sizes and counts as each directory
closes.

Sizes follow `du`: allocated blocks by default, directories include their own
allocation, and an `InodeTracker` counts each hard-linked file once (and, when
following symlinks, each aliased object once). `max_depth` limits which nodes
are kept, not what is counted. Unreadable directories become warnings, so a
partial result is never silent. Git status is applied afterwards with libgit2
when requested.

`quick_list` reads a single directory for immediate display before a full scan.

## `gravityfile-analyze`

- **Duplicates:** group by size, then hash the head and tail of candidates, then
  hash full contents with BLAKE3. Files are re-checked around reads so a file
  changing mid-analysis is reported rather than misgrouped.
- **Ages:** a distribution over configurable buckets and the outermost
  directories whose newest file is older than a threshold.

Both report a status that says whether anything in scope could not be read.

## `gravityfile-ops`

Copy, move, rename, create, archive and delete, each as a background task
reporting progress over a channel and honouring a cancellation token.

Nothing is ever overwritten in place. Results are built in a private staging
directory beside the destination and published with a single no-clobber rename
(`renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS,
`MoveFileExW` on Windows). An item the user chooses to replace goes to the
system Trash first. Copies use reflinks where the filesystem supports them,
otherwise `copy_file_range` on Linux or a chunked copy elsewhere, preserving
sparse regions, hard links, extended attributes and permissions. Bulk renames run in two phases through temporary
names, so swaps and cycles work and a failure rolls back. Archive extraction
enforces size, entry-count and compression-ratio limits and rejects entries or
links that would escape the destination.

`UndoLog` records how to reverse moves, copies, renames, new files and (on
Linux and Windows) deletions to the Trash.

## `gravityfile-plugin`

Lua 5.4 plugins run in a separate worker process per runtime, started by
re-executing `grav` with a private entry point. Calls have deadlines and
messages are size-bounded; a stuck worker is killed. This isolates the explorer
from plugin failures but is not an OS sandbox. See [plugins](PLUGINS.md).

## `gravityfile-tui`

The explorer, on Ratatui. Tree, Miller-column and treemap layouts share one
`FileTree`; duplicate and age views read the same tree. Scans, previews,
archives and file operations run on background tasks and report back through
channels, so the render loop never blocks on the filesystem.

## `grav`

`src/cli.rs` defines the command line; each subcommand lives in
`src/commands/`. Report commands share one scan path with a progress spinner,
one envelope and completeness check (`src/report.rs`), and atomic publication
for exports. Text output is escaped for the terminal and styled according to
`--color`, `NO_COLOR` and whether the stream is a terminal. See
[reports](REPORTS.md).
