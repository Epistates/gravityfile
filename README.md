[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/gravityfile.svg)](https://crates.io/crates/gravityfile)

# gravityfile

> "Where mass accumulates, attention should follow."

File system explorer and analyzer with an interactive TUI, built in Rust.

<img src="assets/video.gif" alt="gravityfile" style="width: 100%; max-width: 100%; margin: 20px 0;"/>

## Features

- **Interactive TUI** - Beautiful terminal interface with vim-style navigation
- **Miller Columns Layout** - Ranger-style three-pane view (toggle with `v`)
- **File Operations** - Copy, move, rename, create, delete with undo support
- **Parallel Scanning** - Fast directory traversal using `jwalk`
- **Duplicate Detection** - Find duplicate files using BLAKE3 hashing with partial-hash optimization
- **Age Analysis** - Identify stale directories and analyze file age distribution
- **Drill-Down Navigation** - Explore directories without rescanning
- **Conflict Resolution** - Interactive handling for file conflicts during copy/move
- **Command Palette** - Vim-style `:` commands for power users
- **Multiple Themes** - Dark and light theme support
- **Library-First Design** - Use as a library or standalone tool
- **Export Support** - Export scan results to JSON

## Installation

### From crates.io

```bash
cargo install gravityfile
```

### From source

```bash
git clone https://github.com/epistates/gravityfile
cd gravityfile
cargo install --path .
```

This installs two binaries: `gravityfile` and `grav` (short alias).

## Usage

### Interactive TUI (Default)

```bash
gravityfile [PATH]
# or use the short alias:
grav [PATH]
```

Launch the interactive terminal interface to explore disk usage.

### Quick Scan

```bash
gravityfile scan [PATH] [-d DEPTH] [-n TOP]
```

Quick summary of disk usage with tree output.

### Find Duplicates

```bash
gravityfile duplicates [PATH] [--min-size SIZE] [-n TOP]
```

Find duplicate files. Uses a three-phase algorithm:
1. Group files by size
2. Compute partial hash (first + last 4KB)
3. Full BLAKE3 hash for candidates

### Age Analysis

```bash
gravityfile age [PATH] [--stale DURATION]
```

Analyze file ages and find stale directories.

### Export

```bash
gravityfile export [PATH] [-o OUTPUT]
```

Export scan results to JSON.

## TUI Keybindings

### Navigation
| Key | Action |
|-----|--------|
| `j` / `k` | Move down / up |
| `h` / `l` | Collapse / expand directory |
| `g` / `G` | Jump to top / bottom |
| `Ctrl-u` / `Ctrl-d` | Page up / down |
| `Enter` | Drill into directory |
| `Backspace` / `-` | Navigate back |
| `o` | Toggle expand node |

### Selection & Clipboard
| Key | Action |
|-----|--------|
| `Space` | Mark item for multi-select |
| `y` | Yank (copy) to clipboard |
| `x` | Cut to clipboard |
| `p` | Paste from clipboard |
| `Esc` | Clear clipboard / marks |

### File Operations
| Key | Action |
|-----|--------|
| `d` / `Del` | Delete item(s) |
| `r` | Rename |
| `a` | Create file (touch) |
| `A` | Create directory (mkdir) |
| `T` | Take (mkdir + cd into new directory) |
| `Ctrl-z` | Undo |

### Views & Display
| Key | Action |
|-----|--------|
| `Tab` / `Shift-Tab` | Switch view tab |
| `v` | Toggle Tree / Miller layout |
| `i` | Toggle details panel |
| `t` | Toggle theme |
| `R` | Refresh / rescan |

### Commands
| Key | Action |
|-----|--------|
| `:` | Open command palette |
| `?` | Show help |
| `q` | Quit |

### Command Palette
| Command | Action |
|---------|--------|
| `:q` `:quit` | Quit |
| `:cd <path>` | Change directory |
| `:touch <name>` | Create file |
| `:mkdir <name>` | Create directory |
| `:take <name>` | Create dir and cd into it |
| `:yank` `:y` | Copy to clipboard |
| `:cut` `:x` | Cut to clipboard |
| `:paste` `:p` | Paste from clipboard |
| `:delete` `:rm` | Delete marked items |
| `:rename <name>` | Rename current item |
| `:clear` | Clear all marks |
| `:theme dark\|light` | Set theme |
| `:layout tree\|miller` | Set layout |
| `:help` | Show help |

## Library Usage

gravityfile is designed as a composable library:

```rust
use gravityfile_scan::{JwalkScanner, ScanConfig};
use gravityfile_analyze::{DuplicateFinder, DuplicateConfig};

// Scan a directory
let config = ScanConfig::new("/path/to/analyze");
let scanner = JwalkScanner::new();
let tree = scanner.scan(&config)?;

// Find duplicates
let dup_config = DuplicateConfig::builder()
    .min_size(1024u64)
    .build()?;
let finder = DuplicateFinder::with_config(dup_config);
let report = finder.find_duplicates(&tree);

println!("Found {} duplicate groups", report.group_count);
println!("Wasted space: {} bytes", report.total_wasted_space);
```

## Crate Structure

- **`gravityfile`** - Main binary and CLI
- **`gravityfile-core`** - Core types (FileNode, FileTree, etc.)
- **`gravityfile-scan`** - File system scanning engine
- **`gravityfile-analyze`** - Analysis algorithms (duplicates, age)
- **`gravityfile-ops`** - File operations engine (copy, move, rename, delete)
- **`gravityfile-tui`** - Terminal user interface

## Performance

- Parallel directory traversal via `jwalk`
- Memory-mapped I/O for large file hashing
- Partial hash optimization reduces disk reads for duplicate detection
- Event-driven TUI rendering minimizes CPU usage

## Acknowledgements
[![Built With Ratatui](https://img.shields.io/badge/Built_With_Ratatui-000?logo=ratatui&logoColor=fff)](https://ratatui.rs/)

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions welcome! Please feel free to submit a Pull Request.
