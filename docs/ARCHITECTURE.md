# gravityfile Architecture

gravityfile is designed as a modular, high-performance system for file system exploration and analysis.

## Core Design Principles

1.  **Targeted Concurrency**: Directory traversal uses `jwalk`/Rayon, while `tokio` coordinates scans, previews, archives, and file operations without putting filesystem I/O in the input/render loop.
2.  **Memory Efficiency**: Large file trees minimize repeated allocations through `CompactString` and compact node metadata.
3.  **Unified State**: A single, source-of-truth `FileTree` structure powers all views (Usage, Duplicates, Age, Treemap).
4.  **Asynchronous TUI**: Long-running host filesystem work is scheduled on background tasks and reports completion through channels.

---

## 🏗️ Crate Overview

### `gravityfile-core`
The heartbeat of the system.
- **`FileNode`**: The fundamental tree unit, with a stable scan-local ID and optional hash/Git metadata.
- **`FileTree`**: A high-performance container for nodes. Includes advanced statistics tracking.
- **Error Types**: Unified error handling across all crates.

### `gravityfile-scan`
The high-speed traversal engine.
- **`JwalkScanner`**: Implements parallel directory traversal with glob-based ignore rules matched against names and root-relative paths.
- **`InodeTracker`**: Prevents double-counting hardlinks across a single device by tracking (inode, device) pairs.
- **Git Integration**: Uses `libgit2` (via `git2-rs`) to annotate nodes after a full scan.

### `gravityfile-analyze`
The intelligence layer.
- **Duplicate Detection**: Uses a 3-phase algorithm for efficiency:
  1.  **Size Matching**: Instant filtering of unique-sized files.
  2.  **Partial Hashing**: Reads only the first and last 4KB of size-matched candidates.
  3.  **Full BLAKE3**: Only performs full cryptographic hashes on final candidate groups.
- **Age Analysis**: Computes the distribution of file ages and identifies "stale" directories.

### `gravityfile-ops`
The file operation and safety engine.
- **`OperationExecutor`**: Orchestrates copy, move, rename, and delete tasks.
- **Conflict Resolution**: Implements interactive policies (Overwrite, Rename, Skip) for destination collisions.
- **Undo System**: Maintains a rolling log for operations whose prior state can be represented safely. Overwrites and deletions are intentionally not recorded as undoable.

Bulk rename writes all native-path intents and a shared batch plan before source
mutation. `rename_batch.rs` locks participant journals in sorted order, validates
the recorded object and parent identities, and restores uncommitted batches using
no-replace moves and resumable checkpoints. Archive extraction prepares identities
and a publication plan in `archive_journal.rs`; failed or interrupted publication
retains decoded staging and moves owned output into recovery storage. Inspection
and explicit recovery remain separate APIs. These protocols preserve uncertain
data but do not provide snapshot isolation, atomic multi-file publication, or
qualified power-loss durability. See [the recovery contract](RECOVERY.md).

### `gravityfile-tui`

`app/archive.rs` owns archive task admission, destination construction and
completion policy. It keeps successful creation, published-but-uncertain
persistence, and failure before publication distinct, and coordinates refresh
with active mutations through the application state. The operations crate owns
format handling and filesystem publication.
The interactive user interface.
- **Layouts**:
  - **Tree**: Classical hierarchical view.
  - **Miller Columns**: Ranger-style navigation for deep drill-downs.
  - **Treemap**: Squarified space-filling visualization for rapid disk usage identification.
- **Rendering**: Uses Ratatui's buffered, diff-based terminal drawing and explicit application state.

---

## 🛡️ Security Model

- **Supervised Plugins**: The explorer runs Lua plugins in separate persistent
  workers with parent-enforced deadlines and process cleanup. VM permission
  controls and process supervision are not OS filesystem/network confinement.
  `gravityfile-plugin/src/process.rs` owns lifecycle, admission and cancellation;
  its private `process/protocol.rs` module owns wire types, bounded framing and logs.
- **Safe Operations**: Destructive operations default to **Trash** (via the `trash` crate) rather than permanent deletion.
- **No-Root Execution**: gravityfile is designed to run with user-level permissions, gracefully handling permission errors without crashing.
