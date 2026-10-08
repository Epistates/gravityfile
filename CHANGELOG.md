# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Add identity-checked, coordinated bulk-rename recovery with synced batch plans,
  commit markers and resumable no-replace restoration. `recover --rollback-rename`
  refuses committed batches, replaced objects and occupied original paths.

- Journal archive extraction before destination publication. Failed publication
  retains owned output instead of deleting it; `recover --retain-extraction`
  moves interrupted output into retained storage and preserves unrelated data.
  Recovery JSON adds `archive_records` and optional rename object identities.
  Neither protocol provides snapshot isolation or qualified power-loss durability.

- Record bounded, immutable bulk-rename progress before and after installation
  and rollback moves. Recovery inspection exposes the last historical phase;
  invalid histories preserve valid intents and report incomplete inspection.
  Failed final recording retains an installed batch and warns before retrying.

- Cap each process's diagnostic log at 8 MiB, sharing the budget across logging
  threads and recording truncation once. Failed writes stop further log output.

- Prevent failed stderr diagnostic writes from aborting the CLI; preserve report
  output and the operation’s success or failure status.

- Distinguish completed recovery restores from failed output acknowledgements,
  including flush failures, and advise inspection before retrying.

- Keep failed recovery JSON writes unsuccessful, including closed output pipes;
  restrict non-strict broken-pipe success handling to observation commands.

- Add portable `error_kind` categories to recovery JSON issues so consumers can
  distinguish busy, malformed and inaccessible records without parsing messages.

- Preserve recovery lock error kinds and OS diagnostics; report active journals
  as `WouldBlock` rather than flattening every lock failure to `Other`.

- Separate plugin IPC framing, wire types and log budgets from worker supervision
  into a private protocol module, retaining protocol compatibility.

- Include plugin request serialization in the reply timeout budget and retire
  workers before dispatch when encoding has exhausted that budget.

- Abort queued plugin blocking tasks when their async caller times out or is
  dropped, preventing deferred dispatch after worker termination.

- Preserve extracted paths with changed or unrecorded Unix filesystem identities
  during rollback; report incomplete cleanup instead of removing replacements.

- Prevent extraction rollback from recursively deleting a directory that another
  writer substituted for an extracted file; report incomplete cleanup instead.

- Report incomplete archive extraction rollback with publication and cleanup
  errors; refresh the TUI to show remaining output while preserving selection.

- Refresh LuaJIT sources and zerocopy dependencies; update the pinned CI installer
  action to 2.87.22, retaining Rust 1.90 compatibility.

- Add cooperative archive extraction cancellation to the API and TUI. Cancel
  decoding/preflight without publishing; finish or roll back a merge already begun.

- Add cooperative archive creation cancellation and connect TUI cancel/quit
  requests to it. Cancelled staging is removed; already published archives remain.

- Isolate TUI archive admission, destination and completion handling in a dedicated
  module so its publication and refresh policy can be reviewed together.

- Create archive output files with owner-only Unix permissions that persist
  after publication; sharing now requires an explicit permission change.

- Remove completed archive sources directly from the marked-path hash set,
  avoiding quadratic selection cleanup while preserving unrelated marks.

- Include project licenses and generated dependency notices in binary release
  archives, with a pinned generator, checksum-pinned source texts, explicit
  text-source limitations, and required-document verification.

- Refresh Cargo dependencies, including jwalk 0.9, dirs 7 and Rhai 1.26;
  migrate scanner pruning to jwalk's current API. Retain verified Rust 1.90 support.
- Refresh pinned GitHub Actions releases and enable weekly Cargo dependency PRs.

### Fixed

- Refresh the TUI after an archive worker loses its completion reply, clearing
  stale progress and reporting uncertain output without replaying the operation.

- Preserve significant whitespace in ZIP symlink targets and reject invalid
  target bytes instead of silently trimming or replacing them.

- Preserve ordinary Unix permissions for regular files in ZIP archives, avoiding
  broadened private-file modes and lost executable bits during a round trip.

- Bound ZIP source reads to the inspected length and fail on detected growth,
  truncation or metadata changes instead of publishing an unstable input.

- Open ZIP file inputs without following replacement symlinks or blocking on
  FIFOs on Unix, and reject inode/type changes between inspection and opening.

- Append default archive extensions without converting native paths to display
  text, preventing output in a different directory with a lossy look-alike name.

- Refresh the TUI after an archive is published with uncertain durability,
  preserving its warning and source selection instead of reporting creation failure.

- Sync the destination directory after archive publication on Unix. New
  `ArchiveError::PublishedDurability` distinguishes a complete visible archive
  with uncertain persistence from failure before publication.

- Preserve non-UTF-8 top-level names when creating TAR archives, including
  compressed variants, directories and symlinks. ZIP retains its UTF-8 requirement.

- Attempt both journal and parent-directory synchronization after overwrite or
  copy-undo moves, reporting all failures instead of skipping the second sync.

- Preserve directory-sync failures in overwrite rollback errors, explicitly
  reporting uncertain durability and retaining recovery data for inspection.

- Include blocking-executor queue time in async plugin deadlines and abort queued
  tasks when calls time out or are dropped, preventing unbounded admission waits.

- Terminate plugin workers and release their permits when request dispatch finds
  a disconnected or unexpectedly full I/O channel, preventing stale live handles.

- Verify the complete release notice body against its generated checksum, rejecting
  truncation or alteration even when its dependency provenance header is intact.

- Reject release notices generated for a different target, lockfile, notice
  configuration or workspace manifests, including feature-only changes.

- TAR previews validate compression trailers and concatenated members before
  reporting a successful inventory; reject hidden trailing data and excessive
  padding under the existing read budgets.

- Bound TAR parser metadata reads before GNU/PAX extensions and sparse maps can
  grow without the existing entry limits taking effect; reject excessive
  cumulative metadata while preserving separate payload budgets.

- Unix exports anchor staging, replacement, and cleanup to an open parent directory,
  preventing later parent-path substitution from redirecting those operations.

- Unix file exports sync their parent directory after atomic replacement and
  report post-publication sync failures without deleting the completed report.

- Toggling a single duplicate file no longer clones every path in its group.

- Duplicate filtering and visible-header counts share a single-directory cache
  keyed by immutable report identity and directory scope. Repeated rendering and
  navigation reuse counts; report or scope changes rebuild them in one pass.

- Duplicate-view rendering computes the visible row range before constructing
  file rows, avoiding allocations for every expanded duplicate. Heat-map scale
  is computed once per frame instead of once per visible group header.

- TUI final-warning merge uses hash-based lookup instead of quadratic path
  scans. Distinct warning kinds/messages on the same path remain visible, exact
  repeated final warnings are deduplicated, and encounter order is preserved.

- Background scan completion no longer closes active dialogs or clears unrelated
  errors. Escape in normal mode acknowledges an error before clearing marks,
  clipboard state, or cancelling background work.

- Updated `lru` to 0.18.2 for RUSTSEC-2026-0253 and made the local release gate
  reject unsoundness advisories. Package assembly excludes local agent state,
  including `.claude`, `.codex`, and `.agents` directories.

- ZIP preflight checks entry count before metadata traversal, propagates entry
  read errors, and rejects per-entry/remaining-budget violations and nonempty
  output claims with zero compressed bytes before decoding.

- XZ extraction enforces a 256 MiB decoder-memory limit instead of allowing
  unbounded dictionary allocation, while retaining concatenated XZ support.

- Gzip, bzip2, and xz TAR inputs decode concatenated compression members.
  A TAR stream split across members extracts correctly, while hidden decoded
  content or truncated trailers in later members fail before publication.

- TAR extraction finishes reading the decoder after the TAR end marker so
  truncated compression trailers and checksum errors fail before publication.
  Decoded trailing data must be zero padding of at most 1 MiB.

- Archive extraction opens and validates one regular-file handle before creating
  staging paths, then passes that handle to the decoder. Unix FIFO inputs cannot
  wait for a writer; regular-file symlinks remain supported. Non-missing input
  errors are no longer mislabeled as missing files.

- TAR preflight uses effective entry sizes, including sparse logical extents
  and PAX overrides, and checks the remaining extraction budget before unpacking.
  FIFO/device headers are rejected instead of silently becoming ordinary files.

- TAR extraction resolves hard links relative to the archive staging root,
  using `unpack_in` instead of cwd-relative `unpack`. Targets must be existing
  extracted regular files inside staging; traversal and symlink targets fail.

- Archive extraction skips root-directory headers so TAR metadata cannot change
  staging-root permissions. Non-directory entries resolving to the extraction
  root are rejected; ordinary archives containing `.`/`./` directories work.

- Copy staging, archive staging, and overwrite recovery directories now request
  Unix `0700` permissions at creation instead of inheriting potentially readable
  directory modes from the process umask. All operation staging uses one helper.

- Bulk-rename editor lists use owned private temporary directories instead of
  predictable shared paths. Reads enforce regular UTF-8 files, a 1 MiB cap,
  exact line counts, and valid names; Unix symlinks/FIFOs are rejected. Source
  ordering is deterministic, and failed editor commands stop processing.

- Bulk rename exposes `rename_paths_with_cancellation`. Cancellation before
  final installation attempts uncancelled rollback, reporting retained paths
  when restoration is blocked. TUI cancel/quit requests cancellation and waits
  for the worker result before accepting further commands.

- TUI bulk rename runs on a blocking worker instead of the event-loop thread.
  A running state prevents overlapping commands until completion or rollback;
  successful completion clears marks and failures preserve them.

- Bulk-rename preflight resolves parent-directory aliases, validates no-op
  entries, and rejects duplicate or nested source/destination batches before
  staging. Leaf symlinks remain renameable without following their targets.

- Bulk-rename rollback no longer restores an occupied staging path after
  restaging an installed target fails. Missing staged payloads are reported as
  rollback failures, and independent recoverable entries still restore.

- Overwrite moves check cancellation before publication and restore the staged
  source when possible. A blocked rollback retains a discoverable, recoverable
  payload without overwriting a concurrent writer. Pre-cancelled overwrite moves
  do not create journals.

- Copy/move progress delivery is best-effort under queue backpressure. Dropped
  result receivers cancel work through executor/TUI adapters, while in-flight
  filesystem work is awaited for cooperative cleanup. Conflicts and completion
  retain reliable delivery.

- Copy/move completion carries an explicit `cancelled` flag through executor
  and TUI adapters. Cancellation and conflict aborts no longer report success
  or count the aborted item as a user skip. Completed work retains undo records.
  Callers constructing `OperationComplete` must now provide `cancelled`.

- Startup surfaces settings read/parse failures while using defaults. Settings
  reads and saves enforce a 1 MiB limit; Unix FIFO reads do not block startup.
  Image-preview detection uses the application's loaded settings.

- Failed settings-dialog saves retain dirty edits for retry and leave active
  settings unchanged; successful saves apply the persisted settings.

- Settings saves sync a private temporary file before replacing the live file,
  preserving existing data on publication failure and replacing leaf symlinks.
  Bookmark changes now surface persistence errors instead of ignoring them.

- Background analysis emits explicit cancellation/failure results and cancels
  its shared work token when the UI receiver closes. Join failures are surfaced
  instead of appearing only as an unexpected channel closure.

- TUI scan progress recovers after broadcast lag, stops promptly on cancellation
  even with a full UI queue, and cancels scans when their receiver is dropped.

- Recovery lock guards explicitly unlock at the end of ownership so duplicated
  descriptors cannot prolong completed transactions’ locks.

- TUI quick-listing failures at startup and requested navigation populate the
  error state instead of silently appearing empty. Failed lazy loads retain
  the current tree; parent-cache refresh clears stale Miller-column data.

- Hidden/ignored descendants now suppress stale-directory recommendations, as
  depth limits already did. Age reports expose recommendation availability;
  legacy reports default to unknown, and CLI/TUI labels show suppression.

- Discovery and explicit plugin loading share one validated implementation;
  discovery logs individual failures and continues with subsequent candidates.

- Plugin discovery sorts candidate paths for stable identity selection and hook
  order. Directory-enumeration errors are surfaced instead of silently omitted.

- Explicit plugin loading honors global/per-plugin disable settings. Both load
  paths prevent duplicate loaded identities and repeated discovery side effects.

- Plugin managers reject duplicate runtime registration, retain handles after
  failed unloads, and attempt every runtime during shutdown even after an error.

- Plugin manifests use a bounded 64 KiB reader in both discovery and direct
  loading. Unix manifest reads reject symlinks and nonregular files without
  blocking on FIFOs; oversized manifests no longer allocate unbounded host memory.

- Human-readable CLI reports and progress lines escape control characters and
  Unicode direction overrides from filenames and messages; JSON retains the
  original values.

- Duplicate analysis hashes a file once when partial ranges would cover its
  entire contents, preserving canonical full hashes and identity validation.

- Stale-directory analysis retains at most `max_stale_dirs` candidates during
  traversal, with deterministic path ordering for equal-size results.

- Average file age preserves nanosecond precision and avoids understating the
  result when accumulated seconds exceed a 64-bit counter.

- Age analysis rejects empty, ambiguous, and non-exhaustive bucket sets instead
  of silently omitting files from the distribution. `AgeAnalyzer::with_config`
  now returns `Result`; builders and direct configurations share validation.

- ZIP creation preserves symlinks using explicit link entries and rejects
  non-UTF-8 nested filenames/link targets rather than silently changing them.
- Archive creation rejects selected sources with duplicate top-level names
  before writing, preventing ambiguous entries and extraction-time data loss.

- Archive creation explicitly finishes compressed TAR streams and propagates
  final write errors. Synced staged archives publish with atomic no-replace
  rename; unsupported filesystems no longer fall back to visible partial copies.

- Observation CLI commands share signal-driven cancellation across scanning and
  analysis; a second interrupt forces exit. Interrupted commands fail even when
  strict mode is disabled. See the report contract for interruption and export behavior.

- Standard CLI/TUI plugin execution uses independent supervised processes with
  parent-enforced deadlines, cancellation, bounded IPC/logs and worker counts.
  Embedded TUI users must configure a worker executable to enable plugins.
- Rhai isolated globals now persist instead of silently ignoring assignments.
- Plugin source loading rejects files larger than 1 MiB and nonregular files.

- Duplicate analysis reports unreadable, missing, or changing candidates instead
  of silently dropping them. Partial and full reads validate the same file
  observation; Unix reads reject substituted leaf symlinks and nonregular files.
- Analysis cancellation and source scan warnings are explicit report states.
  Incomplete age analysis suppresses stale-directory recommendations.
- Scanner cancellation is checked at traversal and completion boundaries, including
  when cancellation prunes the final directory batch without yielding another entry.
- CLI output propagates broken-pipe errors without panicking. Ordinary mode accepts
  early pipe closure; strict mode requires delivery of the report.

- **Undo of a move no longer overwrites an unrelated file** recreated at the original
  path. `rename_noreplace` now reports *why* it failed, and the copy+delete fallback is
  reached only for a genuine cross-device rename.
- **Move no longer clobbers a destination that appears mid-flight.** Previously any rename
  failure fell through to `fs::copy`, which truncates.
- **Copy/move/rename of files with names near the 255-byte limit** no longer fail with
  `ENAMETOOLONG`: the staging name's prefix is accounted for.
- Filesystems without an atomic no-replace rename now return an unsupported-operation
  error instead of using a racy check followed by a potentially overwriting rename.
- Windows no-replace rename uses `MoveFileExW` with replacement disabled, instead
  of `std::fs::rename`, which can replace existing files.
- Cross-filesystem moves and move undo share private staging and exclusive publication.
  A failed source removal retains the complete destination for recovery, even when
  source removal has already deleted some children.
- Cancelling a copy or move interrupts unanswered conflict requests and blocked
  conflict delivery. Rollback is not disabled by a cancelled operation token.
- JSON exports stream through a private temporary file before replacement, preserving
  existing output on serialization failures and avoiding writes through final symlinks.
- CLI size parsing preserves integer precision, rejects repeated suffixes and overflow;
  duration overflow returns an error instead of panicking. `m` and `mo` both denote months.
- Help and version no longer require writable log storage; Unix logs use mode 0600.
- Cancelling a large directory copy takes effect immediately instead of only between
  top-level items, and leaves no partial destination or staging directory behind.
- The copy pre-scan no longer runs blocking filesystem walks on an async worker thread.
- Git status annotation no longer costs one `canonicalize` syscall per node.
- **The TUI no longer aborts on a narrow terminal.** The right-anchored size bar in both
  the tree and Miller views computed an out-of-bounds rect once its column was narrower
  than the bar, panicking the process. Miller columns are percentage splits, so this hit
  at terminal widths well above the point where the tree view was affected. Narrow columns
  now drop the bar and give the row its columns back.

### Changed

- Scan pattern compilation/matching APIs and `DuplicateFinder::with_config` now
  return `Result`. Invalid patterns and inverted duplicate-size bounds are errors,
  including configurations built through public fields rather than builders.
- Compiled ignore filters are tied to their exact inputs, so public pattern
  mutation cannot leave stale filters active. Quick listings record their actual
  root and depth; depth-limited analysis does not recommend stale directories.

- **Minimum supported Rust version raised from 1.88 to 1.90.** Required by the sixel
  encoder behind the new image previews (`ratatui-image` → `icy_sixel` → `quantette`,
  which pins rustc 1.90). No source in this workspace needs it.

### Added

- Copy and move overwrites record recovery intent before moving paths, retain
  payload/original data on interruption, and coordinate recovery with file locks.
- `grav recover DIR` discovers interrupted overwrite journals. Explicit restore
  options recover an original or staged payload only to an unoccupied path.
- Process-exit tests cover journal discovery and preservation at overwrite
  publication boundaries, alongside malformed-record and CLI recovery tests.

- `--envelope` emits schema-versioned JSON with report kind, root, scan scope,
  completeness, and data. `--strict` emits incomplete reports then fails for automation.
- Analysis diagnostics retain total failure counts with at most 100 path-ordered
  details. Duplicate group/path ordering is deterministic for equal waste.
- A reproducible benchmark harness records whole-command time, Unix peak RSS,
  binary identity, workload, and successful file-count verification.

- **Inline image previews** using the terminal's graphics protocol (Kitty, iTerm2, Sixel,
  halfblocks fallback), with a new `image` preview mode. Decoding and encoding happen off
  the render thread. Disable with `image_previews = false` in settings.
- **Interactive conflict resolution.** Copy and move now pause on each conflict and wait
  for an answer, so resolving one item no longer restarts the whole operation with a
  blanket policy. "Apply to all" answers are honoured for the remainder.
- Skipped items are reported separately from failures.
- CI now runs the test suite on macOS and Windows, checks the declared MSRV, and enforces
  `cargo deny` and `cargo machete`.
- TUI test coverage for the previously untested navigation, layout, and rendering code:
  every view and mode is now rendered at 33 terminal sizes, tree/Miller/treemap navigation
  is checked against its bounds, the key map is pinned against its help text, and the
  conflict prompt is driven end to end against a real colliding paste.

### Security

- **Plugin Lua sandbox now whitelists standard libraries** (`io`, `os`, `debug` and
  `package` are never loaded) instead of nil-ing globals after the fact, which left them
  reachable via captured references.
- `print` is redirected to the log; it previously wrote straight to the alternate screen.
- Plugin execution runs on a blocking thread so the wall-clock timeout can actually
  preempt a host call. It previously wrapped a future with no await points and could
  never fire.
- Filesystem existence probes no longer leak through a symlink pointing out of the
  sandbox.
- ZIP symlink targets are read with a bound; a crafted entry could previously allocate
  gigabytes.


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
