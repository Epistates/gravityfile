# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.1]: https://github.com/epistates/gravityfile/releases/tag/v0.2.1
[0.2.0]: https://github.com/epistates/gravityfile/releases/tag/v0.2.0
[0.1.2]: https://github.com/epistates/gravityfile/releases/tag/v0.1.2
[0.1.1]: https://github.com/epistates/gravityfile/releases/tag/v0.1.1
[0.1.0]: https://github.com/epistates/gravityfile/releases/tag/v0.1.0
