# gravityfile-tui

Interactive terminal user interface for gravityfile, built with ratatui.

This crate provides a feature-rich TUI for exploring disk usage, finding duplicates, and analyzing file ages.

## Features

- **Interactive Explorer** - Navigate directory trees with vim-style keybindings
- **Multiple Views** - Explorer, Duplicates, Age, and Errors views
- **Drill-Down Navigation** - Explore subdirectories without rescanning
- **Deletion Support** - Mark files for deletion with confirmation
- **Command Palette** - Vim-style `:` commands for power users
- **Theming** - Dark and light theme support
- **Details Panel** - Toggle detailed file information
- **Async Operations** - Non-blocking scanning and deletion

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
| `h` / `←` | Collapse directory |
| `l` / `→` | Expand directory |
| `g` | Jump to top |
| `G` | Jump to bottom |
| `Ctrl+d` | Page down |
| `Ctrl+u` | Page up |

### Navigation
| Key | Action |
|-----|--------|
| `Enter` | Drill into directory |
| `Backspace` / `-` | Navigate back |
| `Tab` | Switch view |

### Actions
| Key | Action |
|-----|--------|
| `d` | Mark for deletion |
| `x` | Clear all marks |
| `y` | Confirm deletion |
| `i` | Toggle details panel |
| `t` | Toggle theme |
| `r` | Refresh / rescan |
| `?` | Show help |
| `:` | Open command palette |
| `q` / `Esc` | Quit |

## Command Palette

Press `:` to open the command palette:

| Command | Action |
|---------|--------|
| `:q` / `:quit` | Quit application |
| `:r` / `:refresh` | Rescan directory |
| `:cd <path>` | Navigate to path |
| `:cd ..` | Go to parent |
| `:root` | Go to scan root |
| `:back` | Navigate back |
| `:explorer` / `:e` | Switch to explorer view |
| `:duplicates` / `:d` | Switch to duplicates view |
| `:age` / `:a` | Switch to age view |
| `:errors` | Switch to errors view |
| `:theme` / `:t` | Toggle theme |
| `:dark` | Set dark theme |
| `:light` | Set light theme |
| `:details` / `:i` | Toggle details panel |
| `:clear` | Clear deletion marks |
| `:help` / `:?` | Show help |

## Views

### Explorer View
The default view showing a tree of directories and files sorted by size. The size bar shows relative size compared to the largest item.

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
  - `state.rs` - State types (AppMode, View, etc.)
  - `commands.rs` - Command palette parsing
  - `navigation.rs` - List navigation abstractions
  - `render.rs` - All rendering code
  - `scanning.rs` - Background scan operations
  - `deletion.rs` - File deletion logic
- **ui/** - Widget implementations
  - `tree.rs` - Tree widget for directory display
  - `help.rs` - Help overlay
  - `modals.rs` - Modal dialogs
- **theme.rs** - Color schemes
- **event.rs** - Terminal event handling

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
