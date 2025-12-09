//! Terminal user interface for gravityfile.
//!
//! This crate provides an interactive TUI for exploring disk usage,
//! built with ratatui.
//!
//! # Overview
//!
//! `gravityfile-tui` provides a feature-rich terminal interface:
//!
//! - **Explorer view** - Navigate directory trees sorted by size
//! - **Duplicates view** - Browse duplicate file groups
//! - **Age view** - Analyze file age distribution
//! - **Errors view** - Review scan warnings
//!
//! # Usage
//!
//! ```rust,no_run
//! use gravityfile_tui;
//! use std::path::PathBuf;
//!
//! // Run the TUI on a directory
//! gravityfile_tui::run(PathBuf::from("/path/to/explore")).unwrap();
//! ```
//!
//! # Keyboard Navigation
//!
//! - `j`/`k` - Move down/up
//! - `h`/`l` - Collapse/expand directories
//! - `Enter` - Drill into directory
//! - `Backspace` - Navigate back
//! - `Tab` - Switch view
//! - `d` - Mark for deletion
//! - `:` - Command palette
//! - `?` - Help
//! - `q` - Quit

pub mod app;
mod event;
mod theme;
mod ui;

pub use app::{App, AppResult};
pub use theme::Theme;

/// Run the TUI application.
pub fn run(path: std::path::PathBuf) -> AppResult<()> {
    // Create tokio runtime for async operations
    let rt = tokio::runtime::Runtime::new()?;

    let terminal = ratatui::init();
    let result = rt.block_on(App::new(path).run(terminal));
    ratatui::restore();

    // Shutdown runtime immediately to cancel background tasks
    rt.shutdown_timeout(std::time::Duration::from_millis(100));

    result
}
