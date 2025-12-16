# gravityfile-tui

Interactive terminal user interface for gravityfile, built with ratatui.

This crate provides a feature-rich TUI for exploring disk usage, finding duplicates, and analyzing file ages.

## Features

- **Interactive Explorer** - Navigate directory trees with vim-style keybindings
- **Miller Columns Layout** - Ranger-style three-pane view (Parent | Current | Preview)
- **Multiple Views** - Explorer, Duplicates, Age, and Errors views
- **Drill-Down Navigation** - Explore subdirectories without rescanning
- **File Operations** - Copy, move, rename, create, delete with vim-style keybindings
- **Clipboard Support** - Yank/cut/paste with conflict resolution
- **Undo Support** - Undo file operations with `Ctrl-z`
- **Command Palette** - Vim-style `:` commands for power users
- **Theming** - Dark and light theme support
- **Details Panel** - Toggle detailed file information
- **Async Operations** - Non-blocking scanning and file operations

## Usage

### As a Library

```rust
use gravityfile_tui;
use std::path::PathBuf;

// Run the TUI on a directory
gravityfile_tui::run(PathBuf::from("/path/to/explore"))?;
```

### Programmatic Access

```rust
use gravityfile_tui::{App, Theme};

// Create app instance
let app = App::new(PathBuf::from("/path/to/explore"));

// Run with terminal
let terminal = ratatui::init();
let result = app.run(terminal);
ratatui::restore();
```

## Keyboard Navigation

### Movement
| Key | Action |
|-----|--------|
| `j` / `↓` | Move down |
| `k` / `↑` | Move up |
| `h` / `←` | Collapse directory / Move left (Miller) |
| `l` / `→` | Expand directory / Move right (Miller) |
| `g` | Jump to top |
| `G` | Jump to bottom |
| `Ctrl+d` | Page down |
| `Ctrl+u` | Page up |

### Navigation
| Key | Action |
|-----|--------|
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
| `Ctrl+z` | Undo |

### Views & Display
| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Switch view tab |
| `v` | Toggle Tree / Miller layout |
| `i` | Toggle details panel |
| `t` | Toggle theme |
| `R` | Refresh / rescan |
| `?` | Show help |
| `:` | Open command palette |
| `q` | Quit |

## Command Palette

Press `:` to open the command palette:

| Command | Action |
|---------|--------|
| `:q` `:quit` | Quit application |
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

## Views

### Explorer View
The default view showing a tree of directories and files sorted by size. The size bar shows relative size compared to the largest item.

Toggle between two layout modes with `v`:
- **Tree Layout** (default) - Hierarchical tree with expand/collapse
- **Miller Layout** - Three-column ranger-style view (Parent | Current | Preview)

### Duplicates View
Shows groups of duplicate files found via content hashing. Files are grouped by their BLAKE3 hash and sorted by wasted space.

### Age View
Shows file age distribution in buckets (Today, This Week, This Month, etc.) and identifies stale directories that haven't been modified recently.

### Errors View
Displays warnings and errors encountered during scanning, such as permission denied or broken symlinks.

## Architecture

The TUI is organized into focused modules:

- **app/** - Application state and logic
  - `mod.rs` - Core App struct and event loop
  - `state.rs` - State types (AppMode, View, LayoutMode, ClipboardState, etc.)
  - `commands.rs` - Command palette parsing
  - `navigation.rs` - List navigation abstractions
  - `input.rs` - Text input handling for rename/create modes
  - `render.rs` - All rendering code
  - `scanning.rs` - Background scan operations
  - `deletion.rs` - File deletion logic
- **ui/** - Widget implementations
  - `tree.rs` - Tree widget for directory display
  - `miller.rs` - Miller columns (three-pane ranger-style) view
  - `help.rs` - Help overlay
  - `modals.rs` - Modal dialogs (confirmation, conflict resolution, etc.)
  - `size_bar.rs` - Size visualization bars
- **theme.rs** - Color schemes
- **event.rs** - Terminal event handling and key bindings

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
