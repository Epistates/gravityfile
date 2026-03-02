# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-03-02

### Added

- **WASM Plugin Runtime** - Full WebAssembly plugin support via Extism:
  - Load `.wasm` plugins alongside Lua and Rhai scripts
  - Hook dispatch, method invocation, and isolated execution contexts
  - Automatic hook discovery by inspecting WASM exports
- **Plugin System Integration** - TUI now initializes and dispatches lifecycle hooks:
  - `OnStartup`, `OnShutdown`, `OnNavigate`, `OnScanComplete` hooks
  - Plugin manager with Lua, Rhai, and WASM runtimes registered at startup
  - Non-blocking hook dispatch via `tokio::spawn` for scan-complete events
- **Lua Sandbox Enforcement** - Filesystem API now respects sandbox read permissions:
  - `fs.read`, `fs.read_bytes`, `fs.exists`, `fs.is_dir`, `fs.is_file`, `fs.metadata` all check `SandboxConfig.can_read()`
  - Sandboxed contexts hide file existence for disallowed paths

### Changed

- **API Modernization** - ~25 function signatures changed from `&PathBuf` to `&Path` across all crates, following Rust idioms for borrowed path parameters
- **Clippy Clean** - Eliminated all 84 clippy warnings across the workspace:
  - Collapsed ~35 nested `if` statements using Rust let-chains
  - Replaced 3 manual `Default` impls with `#[derive(Default)]`
  - Removed ~6 redundant closures in `map_err` chains
  - Replaced `.min().max()` chains with `.clamp()`
  - Replaced manual prefix stripping with `strip_prefix()`
  - Replaced `iter().any(|&b| b == 0)` with `.contains(&0)`
  - Removed `trim()` before `split_whitespace()` (redundant)
  - Replaced `vec![]; push()` patterns with `vec![initial]`
  - Used `.is_some_and()` over `.map_or(false, ...)`
  - Used `.is_multiple_of()` over `% N == 0`
  - Used `.flatten()` on fallible iterators
- **Undo System Simplification** - `FilesDeleted` variant simplified:
  - Now stores `paths: Vec<PathBuf>` instead of `trash_entries: Vec<(PathBuf, PathBuf)>`
  - `can_undo()` always returns `false` for deletions (trash restore removed)
- **HookResult Serialization** - Added `Serialize`/`Deserialize` derives for WASM interop
- **README Rewrite** - Modernized with SOTA performance metrics, plugin system showcase, and condensed keybinding reference
- **Dependencies Updated** - All workspace dependencies brought to latest versions

### Fixed

- Broken doc comment continuation in plugin crate module docs
- Test assertions updated to match actual `ScanConfig` defaults
- `ContentHash` test helpers updated for `Box<ContentHash>` optimization
- Duplicate finder tests fixed for correct file count assertions

## [0.3.0] - 2026-01-29

### Added

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
  - **Security hardening:**
    - Path traversal prevention (rejects `..` and absolute paths)
    - ZIP bomb detection (compression ratio and size limits)
    - Symlink escape attack prevention
    - Permission stripping for safe extraction
  - **Loop detection:** Prevents infinite recursion when archiving circular symlinks

### Changed

- Archive preview now shows 🔗 icon for symlinks with target path display
- Improved error messages with sanitized output (no path disclosure)

### Security

- Added comprehensive validation for archive extraction to prevent:
  - Path traversal attacks via `../` sequences
  - Absolute path extraction attempts
  - Symlink-based directory escape attacks
  - ZIP bomb decompression attacks (100:1 ratio limit, 10GB size limit)

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

[0.3.1]: https://github.com/epistates/gravityfile/releases/tag/v0.3.1
[0.3.0]: https://github.com/epistates/gravityfile/releases/tag/v0.3.0
[0.2.2]: https://github.com/epistates/gravityfile/releases/tag/v0.2.2
[0.2.1]: https://github.com/epistates/gravityfile/releases/tag/v0.2.1
[0.2.0]: https://github.com/epistates/gravityfile/releases/tag/v0.2.0
[0.1.2]: https://github.com/epistates/gravityfile/releases/tag/v0.1.2
[0.1.1]: https://github.com/epistates/gravityfile/releases/tag/v0.1.1
[0.1.0]: https://github.com/epistates/gravityfile/releases/tag/v0.1.0
