# gravityfile-tui

The interactive explorer of [gravityfile](https://github.com/epistates/gravityfile),
built with ratatui: a size-sorted tree and Miller columns with previews,
file operations with undo, duplicates, age analysis, search, tabs, themes,
a configurable keymap, read-only snapshot browsing and Lua plugins.

This crate exists for the `grav` binary. Its API follows the binary's needs
and can change in any minor release. The keys, settings and commands are
documented in the [gravityfile README](https://github.com/epistates/gravityfile#readme).

```rust,no_run
use gravityfile_tui::TuiConfig;

// Explore the current directory with sizes as file lengths. `run_with_config`
// takes over the terminal and restores it on exit, including after a panic.
let config = TuiConfig::new().with_apparent_size(true);
gravityfile_tui::run_with_config(".".into(), config)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Licence

MIT or Apache-2.0, at your option.
