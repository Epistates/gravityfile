# gravityfile Architecture

gravityfile is designed as a modular, high-performance system for file system exploration and analysis.

## Core Design Principles

1.  **Concurrency First**: Every core module is built with parallelization as a first-class citizen using `tokio`, `rayon`, and `jwalk`.
2.  **Memory Efficiency**: Massive file systems are handled by minimizing heap allocations. `CompactString` and boxed optional fields (like `content_hash`) ensure low memory pressure.
3.  **Unified State**: A single, source-of-truth `FileTree` structure powers all views (Usage, Duplicates, Age, Treemap).
4.  **Asynchronous TUI**: The UI never blocks on I/O. Background tasks (scanning, hashing, analysis) communicate with the main loop via MPSC channels.

---

## 🏗️ Crate Overview

### `gravityfile-core`
The heartbeat of the system.
- **`FileNode`**: The fundamental unit of the tree. Features `id`-based lookups and lazy-loaded attributes.
- **`FileTree`**: A high-performance container for nodes. Includes advanced statistics tracking.
- **Error Types**: Unified error handling across all crates.

### `gravityfile-scan`
The high-speed traversal engine.
- **`JwalkScanner`**: Implements parallel directory traversal with customizable ignore rules (e.g., `.gitignore`).
- **`InodeTracker`**: Prevents double-counting hardlinks across a single device by tracking (inode, device) pairs.
- **Git Integration**: Uses `libgit2` (via `git2-rs`) to detect repository status in real-time.

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
- **Undo System**: Maintains a rolling log of reversible operations (excluding permanent deletions).

### `gravityfile-tui`
The interactive user interface.
- **Layouts**:
  - **Tree**: Classical hierarchical view.
  - **Miller Columns**: Ranger-style navigation for deep drill-downs.
  - **Treemap**: Squarified space-filling visualization for rapid disk usage identification.
- **Rendering**: Highly optimized `ratatui` integration with diff-based drawing and deferred updates.

---

## 🚀 Performance Benchmarks (Typical)

- **Traversal**: ~500k files/sec (SSD, 12-core CPU).
- **Memory Consumption**: ~120MB per 1M nodes.
- **Duplicate Detection**: Scans 100GB of data for duplicates in <15s (I/O limited).

## 🛡️ Security Model

- **Sandboxed Plugins**: Third-party extensions run in isolated environments with restricted FS access.
- **Safe Operations**: Destructive operations default to **Trash** (via the `trash` crate) rather than permanent deletion.
- **No-Root Execution**: gravityfile is designed to run with user-level permissions, gracefully handling permission errors without crashing.
