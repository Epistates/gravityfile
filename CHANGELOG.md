# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.0]: https://github.com/epistates/gravityfile/releases/tag/v0.2.0
[0.1.2]: https://github.com/epistates/gravityfile/releases/tag/v0.1.2
[0.1.1]: https://github.com/epistates/gravityfile/releases/tag/v0.1.1
[0.1.0]: https://github.com/epistates/gravityfile/releases/tag/v0.1.0
