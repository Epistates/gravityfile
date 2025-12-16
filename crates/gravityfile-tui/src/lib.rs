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
//! use gravityfile_tui::{self, TuiConfig};
//! use std::path::PathBuf;
//!
//! // Run the TUI on a directory
//! let config = TuiConfig::default();
//! gravityfile_tui::run_with_config(PathBuf::from("/path/to/explore"), config).unwrap();
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
mod opener;
mod preview;
mod search;
mod theme;
mod ui;

pub use app::{App, AppResult};
pub use theme::Theme;

/// Configuration for the TUI application.
#[derive(Debug, Clone, Default)]
pub struct TuiConfig {
    /// Whether to automatically start scanning on startup.
    /// If false (default), only a quick directory listing is shown
    /// and the user must press 'R' to start a full scan.
    pub scan_on_startup: bool,
}

impl TuiConfig {
    /// Create a new config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable auto-scan on startup.
    pub fn with_scan_on_startup(mut self, scan: bool) -> Self {
        self.scan_on_startup = scan;
        self
    }
}

/// Run the TUI application with default configuration.
pub fn run(path: std::path::PathBuf) -> AppResult<()> {
    run_with_config(path, TuiConfig::default())
}

/// Run the TUI application with custom configuration.
pub fn run_with_config(path: std::path::PathBuf, config: TuiConfig) -> AppResult<()> {
    // Create tokio runtime for async operations
    let rt = tokio::runtime::Runtime::new()?;

    let terminal = ratatui::init();
    let result = rt.block_on(App::with_config(path, config).run(terminal));
    ratatui::restore();

    // Shutdown runtime immediately to cancel background tasks
    rt.shutdown_timeout(std::time::Duration::from_millis(100));

    result
}
