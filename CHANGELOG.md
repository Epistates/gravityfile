# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Configurable keys.** `keymap.toml` in the config directory rebinds or
  unbinds explorer keys using names like `<C-u>` and actions like
  `move_down`; the README lists them. The help overlay and footer hints are
  generated from the bindings in effect, so they always match, and the help
  now scrolls instead of cutting off what did not fit. Uppercase bindings
  work whether or not the terminal reports Shift.
- **Snapshots.** `grav export` saves a complete scan as JSON or, with
  `-f ncdu`, as an ncdu dump; a `.zst` output is compressed. `scan`, `age`,
  `duplicates` and `export` accept a snapshot in place of a directory, including
  dumps from `ncdu -o`, and `-A`/`-l` re-measure it without rescanning.
- `grav init bash|zsh|fish` prints the shell function that follows the
  explorer's directory on exit, `grav completions <shell>` prints completions,
  and `grav man` prints a manual page.
- Scan options on every report command: `-A/--apparent-size`,
  `-l/--count-links`, `-L/--follow-symlinks`, `--cross-fs`, `--no-hidden`,
  `-e/--exclude`, `-j/--threads` and `--git`.
- `--color auto|always|never` (honouring `NO_COLOR` and `CLICOLOR_FORCE`), a
  progress spinner while scanning, and `--binary` for KiB/MiB units.
- Copies use reflinks where the filesystem supports them and `copy_file_range`
  on Linux, and preserve sparse files, hard links and extended attributes.
- Archive extraction limits total size, entry count and compression ratio, and
  supports `.tar.zst`.

### Changed

- **Sizes now match `du`.** Directories include their own allocation, hard
  links count once in apparent-size mode too (`-l` counts every link), and
  `max_depth` limits only what is listed, not what is counted. FIFOs, sockets
  and devices are listed instead of silently skipped.
- **The scanner is about 2.5× faster and uses less than half the memory.**
  Metadata is read in parallel and the tree is built in a single pass.
- **Sizes display in decimal units** (`kB`, `MB`, like Finder). Size arguments
  accept both: `MB` is 10^6 bytes and `MiB` is 2^20.
- **One binary, `grav`.** The `gravityfile` executable is gone; the package is
  still `gravityfile`.
- **Report commands:** `-f/--format text|json` replaces `--format`/`--envelope`;
  JSON always uses the versioned envelope, now `schema_version` 2. `grav scan`
  lists the largest entries with `-d/--depth` (default 1) and `-n/--limit`
  (default 20) in place of `--max-depth`/`--top`, and its JSON is a flat list of
  entries. `duplicates` and `age` take `-n/--limit`. Ages need a unit, and a
  bare `m` is rejected as ambiguous: use `min` or `mo`.
- **Exit status:** 3 for incomplete results with `--strict`, 130/143 when
  interrupted (was 1 for both).
- **Settings and plugins moved to `~/.config/gravityfile` on macOS**, matching
  Linux, from `~/Library/Application Support/gravityfile`; the old directory is
  moved automatically on first run. `GRAVITYFILE_HOME` puts all of
  gravityfile's files under one directory.
- Git status is computed only when asked for (`--git`; always in the explorer).
- Overwriting a file on paste moves the old one to the Trash. Copies, moves,
  renames and archive extraction are staged and published with a single
  no-clobber rename.
- Plugins are Lua only.
- Logging is off unless `--log-file` or `RUST_LOG` is given.

### Removed

- `grav recover` and the recovery journals: operations are staged and
  published atomically instead, and replaced files go to the Trash.
- Rhai plugins.
- Report JSON node ids, content hashes, and access and creation times, and the
  scanner's content-hashing option.

### Fixed

- Unreadable directories are reported as warnings instead of silently
  producing a smaller total.
- Delete and marking can no longer target the scan root or the directory being
  viewed; declining a delete keeps the marks you made yourself; marking a
  duplicate group keeps one copy.
- Mouse clicks select the row under the pointer after scrolling.
- `grav scan`'s total is the full disk usage of the root, not just its files.
- **Deleting, copying, moving, renaming or creating anything in the explorer
  no longer throws away the scan.** Each operation used to rescan the directory
  being viewed and replace the whole tree with it, so a delete inside a
  subdirectory of a ten-minute scan of `~` left only that subdirectory. Now only
  the changed entries are scanned again and updated in place, with every
  parent's size recomputed. `R` likewise rescans just the directory being
  viewed.
- Starting a copy, move, delete or other operation cancelled a scan in
  progress. Operations now run alongside a scan; only duplicate and age
  analysis, which they would make stale, is cancelled.
- Explorer search found nothing when the query had a capital letter. It now
  uses smart case (a capital makes the query case-sensitive) and fzf syntax:
  space-separated terms, `^prefix`, `suffix$`, `!exclude`, `'exact`.
- The chosen sort order is kept after a rescan or when moving to a new
  directory; directory listings no longer briefly appear in name order.
- Going to an ancestor of the scanned directory (Ctrl-g) showed an empty pane.
  Go-to now behaves like jumping to a bookmark: Backspace returns, and marks and
  the clipboard are kept.
- Closing a directory tab returned to the closed tab's directory.
- Each directory tab keeps its own selection and back/forward history.
  Switching tabs used to reset the selection, and every tab shared one
  history, so Backspace could jump to a place visited in another tab. A tab
  whose directory was deleted from another tab opens on what remains.
- Going back selects the entry you left, even after re-sorting, a refresh or
  switching between the tree and Miller layouts; it used to restore a row
  number, which could land on a different entry. Backspace now goes back in
  the treemap too, and the Miller preview and parent column follow every way
  of moving (`:cd`, going to the root, create-and-enter), not just some.
- The `default_layout` setting was ignored at startup.
- Directory sizes in the tree were shifted out of their column.
- Double-click enters a directory, as the help says.
- **The explorer stays responsive on huge trees.** Marking files made every
  frame walk the whole tree once per mark (over two seconds per frame with 100
  marks in a million-file scan), and every frame rebuilt every visible row and
  every entry of each Miller column. A frame now builds only the rows on screen:
  under 3 ms at any scroll position, with or without marks.
- The Miller parent column scrolls to keep the current directory in view.
- An invalid name in the rename or create prompt keeps the prompt open with
  the reason, instead of closing it and losing what was typed. Rename and
  create now apply the same name rules, so a backslash is accepted on Unix.
- The bulk rename confirmation scrolls with the selection. It used to show
  only the renames that fit, so the rest could not be reviewed before
  confirming.
- Copy and move report how much data they transferred.
- The treemap dims cut items and italicises copied ones, as the tree does.
- Text previews show how many lines the file has, so a truncated preview says
  so.

## [0.3.1] - 2026-07-24

### Security

- Updated the lockfile to remove the vulnerable `crossbeam-epoch` 0.9.18 release and upgraded `git2` away from its unsound 0.20 line
- Removed BLAKE3's unused memory-mapping feature, eliminating the vulnerable `memmap2` dependency path
- Enforced plugin read-size limits even when Lua callers request a larger limit, and rejected non-regular files to avoid blocking reads from special files
- Made archive creation transactional and rejected archive destinations inside their own source trees

### Fixed

- Duplicate detection now groups by logical file length instead of scan-time allocated size
- Copy and move reject self/descendant destinations before creating output and preserve pre-existing destinations on failed overwrites
- Undo records no longer delete destinations that existed before an overwrite
- New copies are staged before publication so failures do not leave partial visible outputs
- Bulk renames now stage the complete set, support swaps and cycles, and roll back on failure without overwriting unrelated paths
- Archive extraction validates the complete merge and publishes staged entries without replacing existing files
- Interactive copies now preserve file, directory, and symlink timestamps
- Preview and archive work no longer blocks the terminal event loop; terminal event errors and end-of-stream are handled explicitly
- Terminal and mouse state restoration now runs on application errors and external-command failures
- Scanner hashing and Git annotation settings now perform their documented work
- Git status cache lookups now normalize equivalent filesystem path aliases
- Ignore globs match root-relative paths as well as basenames
- Non-Unix scans no longer assign the same fabricated inode identity to every file
- CLI size/duration parsers reject negative, non-finite, and overflowing values without panicking

### Changed

- Updated direct dependencies to current stable releases, refreshed the full lockfile, and set the workspace MSRV to Rust 1.88
- Reworked recursive copy sizing/traversal to use iterative stacks and saturating counters
- Expanded CI to enforce formatting, strict Clippy, locked all-feature tests, and current GitHub Action major versions
- Split shared CLI implementation into a library while preserving both `grav` and `gravityfile` binaries
- Removed unsupported benchmark and plugin-isolation claims from the documentation

## [0.3.0] - 2026-06-14

First release since 0.2.4; this entry consolidates all changes shipped since
that release.

### Security

- **Enforced plugin sandbox permissions** - Declared `PluginPermissions` are now bridged into an enforced `SandboxConfig` scoped to the scan root, so a plugin's filesystem/network/env/command grants are actually applied rather than advisory; filesystem access defaults to deny-all via the new `fs_read_roots` configuration
- **Hardened hook dispatch** - Added a Tower-based middleware pipeline (timeout/util) around hook dispatch to bound plugin execution
- **WASM runtime removed** - Removed the Extism/WASM plugin runtime because the latest Extism release pins a vulnerable Wasmtime line; Lua and Rhai plugin runtimes remain supported

### Fixed

- **Data-loss-safe overwrites** - `Overwrite`/`OverwriteAll` copy and move operations now stage into a temporary sibling and atomically swap into place, so a mid-operation failure can never destroy the existing destination
- **Directory recursion guard** - Added a maximum directory recursion depth to prevent stack overflow on adversarially deep nesting during recursive copy/move
- **Lua value conversion drift** - Centralized Lua↔`Value` conversions in `lua/convert.rs` so the main runtime and the isolated context can no longer disagree on array-vs-object handling (the isolate previously flattened Lua arrays into objects)
- **Quit unresponsive during mouse activity** - Replaced sync `crossterm::event::poll`/`read` drain loop with async `EventStream::now_or_never()`, fixing key events (`q`, Ctrl-C) being dropped when mouse motion floods the event queue
- **Slow quit** - Skip event drain, navigation check, and suspend handling once quit is triggered; reduced runtime shutdown timeout from 100ms to 10ms
- **Terminal cleanup on quit** - Drain buffered events after disabling mouse capture before restoring terminal, preventing raw escape sequences from leaking to the shell
- **Crossterm backend alignment** - Updated Ratatui/Crossterm integration to use a single Crossterm 0.29 backend stack
- **Unused `treemd` dependency** - Removed the optional Rust `treemd` crate and feature while preserving external `treemd` command integration
- **Robustness coverage** - Added integration tests for copy/move/undo file workflows, symlink-preserving copies, duplicate hardlink handling, and age-analysis bucket/stale-directory reporting

### Changed

- **Ops internals deduplicated** - Extracted shared copy/symlink/remove helpers across the copy and move paths
- **TUI/scan/core refinements** - Rendering, scanning, tree, and configuration cleanups
- **Dependencies updated** - Bumped direct workspace dependencies to current releases, including `zip` 8, `bzip2` 0.6, `rayon` 1.12, `compact_str` 0.9, `which` 8, and `crossterm` 0.29
- **Latest dependency refresh** - Updated direct dependencies to latest available releases, including `mlua` 0.12.0-rc.1 and the direct Unix `libc` dependency to 1.0.0-alpha.3
- **Lockfile refreshed** - Pruned stale transitive dependencies after removing the unsafe WASM runtime path and unused optional dependencies
- **Clippy cleanups** - Kept the workspace warning-free under `cargo clippy --workspace --all-features -- -D warnings`

## [0.2.4] - 2026-03-11

### Added

- **WASM Plugin Runtime** - Full WebAssembly plugin support via Extism:
  - Load `.wasm` plugins alongside Lua and Rhai scripts
  - Hook dispatch, method invocation, and isolated execution contexts
  - Automatic hook discovery by inspecting WASM exports
- **Plugin System Integration** - TUI now initializes and dispatches lifecycle hooks:
  - `OnStartup`, `OnShutdown`, `OnNavigate`, `OnScanComplete` hooks
  - Plugin manager with Lua, Rhai, and WASM runtimes registered at startup
  - Non-blocking hook dispatch via `tokio::spawn` for scan-complete events
- **Git Status Integration** - Real-time git status indicators in file listings:
  - Modified (`M`), Staged (`A`), Untracked (`?`), Ignored (`!`), Conflict (`C`) indicators
  - Color-coded status using theme colors (modified=yellow, staged=green, etc.)
  - Automatic detection of git repositories
  - Works in both Tree and Miller column views
- **Treemap Visualization** - Squarified treemap view for disk usage analysis:
  - Space-filling rectangular layout showing relative file/directory sizes
  - Color intensity based on size ratio
  - Keyboard navigation (arrow keys, Enter to drill down, Backspace to go up)
  - Directory/file indicators with different border styles
  - Size labels displayed within rectangles
- **Visual Mode** - Vim-style range selection:
  - Press `V` to enter visual mode
  - Use `j`/`k` to extend selection up/down
  - Selected range highlighted
  - `Esc` exits and marks the selected range
  - Works seamlessly with existing mark system
- **Archive Support** - Create and extract archives with full format support:
  - **Supported formats:** ZIP, TAR, TAR.GZ, TAR.BZ2, TAR.XZ
  - **Commands:** `:extract [destination]`, `:compress <name.zip|tar|tar.gz|...>`
  - **Archive preview:** Shows file listing with sizes, compression ratios, and symlink indicators
  - **Symlink support:** Preserves symlinks in archives (ZIP via Unix mode, TAR via link headers)
  - **Loop detection:** Prevents infinite recursion when archiving circular symlinks

### Security

- **CRIT: Rhai code injection** - Replaced string interpolation in `call_function` with `Engine::call_fn` using typed args and stored AST, eliminating arbitrary code execution via plugin args
- **CRIT: TAR decompression bomb bypass** - Size limits now check actual bytes written to disk via `symlink_metadata` instead of trusting attacker-controlled header sizes
- **CRIT: Cross-filesystem move data loss** - Added cleanup on partial copy failure and warning on source removal failure to prevent duplicate/orphaned content
- **HIGH: Plugin sandbox bypass (Rhai)** - All `fs_*` functions now gate on `SandboxConfig.can_read()` via `Arc`-shared config
- **HIGH: Plugin sandbox bypass (Lua)** - `LuaRuntime` now stores and passes `SandboxConfig` to `create_fs_api` instead of `None`
- **HIGH: Plugin sandbox bypass (WASM)** - Manifest now maps sandbox config to `allowed_paths`, memory limits, timeout; WASI disabled by default
- **MED: ZIP symlink escape** - Deferred symlink extraction to second pass preventing symlink-redirected `create_dir_all`
- **MED: Chained symlink attack** - Always validate symlink targets against canonical destination, not only when `..` present
- **MED: Empty sandbox = full access** - Empty `allowed_read_paths`/`allowed_write_paths` now deny by default instead of granting unrestricted access
- **MED: Lua `fs.read_bytes` unbounded read** - Uses `File::open().take(limit).read_to_end()` instead of full allocation
- **MED: Lua instruction timeout bypass** - Wrapped isolate execution in `tokio::time::timeout` for wall-clock enforcement alongside instruction counter
- Archive extraction hardened with path traversal prevention, ZIP bomb detection, and symlink escape prevention

### Fixed

- **Windows build** - Replaced unstable `windows_by_handle` APIs (`file_index()`, `number_of_links()`) with stable fallbacks; hardlink dedup skipped on Windows
- **Move undo pipeline** - Added `MoveComplete` struct carrying `moved_pairs` from move operations, enabling proper undo recording
- **Cross-filesystem undo** - Undo now falls back to copy+delete when `fs::rename` fails across filesystems
- **Windows symlink** - Choose `symlink_dir` vs `symlink_file` based on target type; log failures instead of silent discard
- **Fuzzy search highlights** - Fixed misalignment on non-ASCII filenames by using character positions instead of byte offsets to match nucleo's UTF-32 indices
- **Viewport height** - Correctly subtracts 5 rows when directory tabs are visible instead of always 4
- **Git pathspec** - Empty relative path at repo root now produces `**` instead of `/**`
- **Filename validation** - Uses byte length (`name.len() > 255`) matching OS enforcement instead of char count
- **Archive symlink icon** - Symlinks in archive preview now show distinct `@ ` icon instead of directory icon
- **Doc link warnings** - Escaped `[PATH]` in doc comments to prevent rustdoc treating them as links
- Broken doc comment continuation in plugin crate module docs
- Test assertions updated to match actual `ScanConfig` defaults
- `ContentHash` test helpers updated for `Box<ContentHash>` optimization
- Duplicate finder tests fixed for correct file count assertions

### Changed

- **API Modernization** - ~25 function signatures changed from `&PathBuf` to `&Path` across all crates, following Rust idioms for borrowed path parameters
- **Clippy Clean** - Eliminated all clippy warnings across the workspace (let-chains, derive Default, redundant closures, clamp, strip_prefix, etc.)
- **InodeTracker** - Replaced `DashMap` with `HashMap` since access is single-threaded; ~15% faster inode tracking
- **GlobSet reuse** - Scanner reuses `ScanConfig`'s pre-compiled `GlobSet` with fallback compilation for builder-created configs
- **Progress counter** - Replaced `Arc<AtomicU64>` with plain `u64` in sequential scan loop
- **`truncate_to_width`** - Extracted shared function to `ui/mod.rs`, eliminating 3-way duplication
- **`TuiConfig`** - Added `#[non_exhaustive]` to prevent future breakage from field additions
- **Cross-device filter** - Extended to non-directory entries (files and symlinks), not just directory pruning
- **Undo System Simplification** - `FilesDeleted` variant now stores `paths: Vec<PathBuf>`; `can_undo()` always returns `false` for deletions
- **HookResult Serialization** - Added `Serialize`/`Deserialize` derives for WASM interop
- **Dependencies Updated** - All workspace dependencies brought to latest versions

## [0.2.2] - 2026-01-16

### Fixed

- **Critical: Safe Deletion with Trash** - All deletions now move files to system trash instead of permanent deletion, allowing recovery of accidentally deleted files
- **Critical: Symlink Safety** - Fixed symlink handling across all file operations to prevent accidental deletion of symlink targets:
  - Uses `symlink_metadata()` instead of `is_dir()` to avoid following symlinks
  - Symlinks are always removed as files, never following their targets
  - Applies to delete, copy, move, and undo operations
- **Critical: Errors View Delete Behavior** - Pressing `d` in Errors view now correctly deletes the selected broken symlink, not items from the Explorer view
- **Delete Confirmation Modal** - Now shows full paths (truncated from left if needed) instead of just filenames, with type indicators (🔗 for symlinks, 📁 for directories)
- **Errors View Toggle Mark** - Space key now properly marks/unmarks broken symlinks in Errors view

### Added

- **Errors View Footer Hints** - Shows available actions (`Spc` to select, `d` to delete, `Esc` to clear)
- **Errors View Delete Hint** - Selected broken symlinks show `[d to delete]` hint
- **Errors View Mark Indicator** - Marked items show `[x]` prefix
- **Comprehensive Symlink Tests** - Added tests for deleting symlinks to files, directories, broken symlinks, symlink chains, and directories containing symlinks

### Changed

- Delete confirmation now says "Move to trash?" instead of "cannot be undone"
- Success messages now say "Moved to trash" instead of "Deleted"

## [0.2.1] - 2025-12-14

### Added

- **Settings Modal** - Persistent user settings with `,` keybinding:
  - Scan on startup (enabled by default)
  - Show hidden files
  - Default layout mode
  - Settings saved to `~/.config/gravityfile/settings.toml`
- **Duplicates View Heat Graph** - Visual heat bars showing relative wasted space per group
- **Duplicates View Help Section** - Dedicated help section explaining duplicates-specific keybindings
- **Context-Aware Footer** - Footer hints change based on selection (group vs file)
- **Parent Directory Navigation** - Navigate beyond scan root all the way to `/`:
  - `h`/`←`/`Backspace` now navigate to parent directory in both Tree and Miller modes
  - Tree view: `h` collapses if expanded, navigates back if collapsed or on file
  - Automatically lazy-loads parent directories when navigating above scan root
  - Miller columns parent pane updates correctly when navigating beyond original scan root

### Changed

- **Duplicates View UX Overhaul**:
  - `Space` on group header now toggles all files in group
  - `d` on group header marks all duplicates (keeps first as original)
  - Files labeled as "keep" (green) vs "dup" (muted) for clarity
  - Marked files shown in red for visibility
  - Header shows "reclaimable" instead of "wasted" (positive framing)
  - Heat bars use color gradient (red → orange → yellow → green) based on impact
- **Polished Footer Styling** - Keybinding hints now use pill/badge style with background colors
- **Scan on Startup** - Now enabled by default (can be disabled in settings)

### Fixed

- Duplicates view marking behavior - `Space` and `d` now work intuitively on both headers and individual files
- **Layout Toggle Sync** - Switching between Tree and Miller views preserves exact selection:
  - Selecting a file in tree and pressing `v` now shows that same file selected in Miller
  - For nested items, view_root adjusts automatically so the selected item is visible
- **Rescan Current Directory** - `R` (refresh) now scans the current directory, not the original startup path
  - Navigate anywhere, press `R` to scan that location
  - Parent navigation uses quick_list (no auto-scan)

## [0.2.0] - 2025-12-13

### Added

- **Miller Columns Layout** - Ranger-style three-pane view (Parent | Current | Preview), toggle with `v` or `:layout miller`
- **File Operations** - Full clipboard support with vim-style keybindings:
  - `y` - Yank (copy) to clipboard
  - `x` - Cut to clipboard
  - `p` - Paste from clipboard
  - `r` - Rename file/directory
  - `a` - Create new file
  - `A` - Create new directory
  - `T` - Take (mkdir + cd into new directory)
- **Conflict Resolution** - Interactive handling for file conflicts during copy/move operations
- **Cross-Platform Releases** - GitHub Actions workflow for automated releases:
  - Linux x86_64 and ARM64
  - macOS Intel and Apple Silicon (with optional code signing/notarization)
  - Windows x86_64
- **CI Pipeline** - Automated build and test on push/PR to main

### Changed

- Improved navigation with history preservation across drill-down operations
- Enhanced details panel with more file metadata

## [0.1.2] - 2025-12-09

### Changed

- Renamed binary from `gf` to `gravityfile` (with `grav` alias) to avoid conflict with git-fetch

## [0.1.1] - 2025-12-09

### Added

- **Interactive TUI** - Terminal interface with vim-style navigation (`j`/`k`, `h`/`l`, `g`/`G`)
- **Parallel Scanning** - Fast directory traversal using `jwalk` for efficient multi-threaded scanning
- **Duplicate Detection** - Three-phase algorithm using BLAKE3 hashing:
  - Phase 1: Group files by size
  - Phase 2: Partial hash (first + last 4KB) for quick filtering
  - Phase 3: Full hash for candidate verification
- **Age Analysis** - Identify stale directories and analyze file age distribution
- **Drill-Down Navigation** - Explore directories without rescanning; maintains expansion state
- **Command Palette** - Vim-style `:` commands (`:cd`, `:root`, `:theme`, `:help`, `:q`)
- **Multiple Themes** - Dark and light theme support with semantic color palette
- **Export Support** - Export scan results to JSON format
- **Library-First Design** - Modular crate structure for use as a library:
  - `gravityfile-core` - Core types (FileNode, FileTree)
  - `gravityfile-scan` - File system scanning engine
  - `gravityfile-analyze` - Analysis algorithms (duplicates, age)
  - `gravityfile-tui` - Terminal user interface

### CLI Commands

- `gravityfile [PATH]` - Launch interactive TUI (default)
- `gravityfile scan [PATH]` - Quick disk usage summary with tree output
- `gravityfile duplicates [PATH]` - Find duplicate files
- `gravityfile age [PATH]` - Analyze file ages
- `gravityfile export [PATH]` - Export scan results to JSON

[0.2.6]: https://github.com/epistates/gravityfile/releases/tag/v0.2.6
[0.2.5]: https://github.com/epistates/gravityfile/releases/tag/v0.2.5
[0.2.4]: https://github.com/epistates/gravityfile/releases/tag/v0.2.4
[0.2.2]: https://github.com/epistates/gravityfile/releases/tag/v0.2.2
[0.2.1]: https://github.com/epistates/gravityfile/releases/tag/v0.2.1
[0.2.0]: https://github.com/epistates/gravityfile/releases/tag/v0.2.0
[0.1.2]: https://github.com/epistates/gravityfile/releases/tag/v0.1.2
[0.1.1]: https://github.com/epistates/gravityfile/releases/tag/v0.1.1
[0.1.0]: https://github.com/epistates/gravityfile/releases/tag/v0.1.0
