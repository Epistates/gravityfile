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
#[non_exhaustive]
pub struct TuiConfig {
    /// Whether to automatically start scanning on startup.
    /// `None` means defer to the user's saved settings.
    /// `Some(true)` forces scanning on, `Some(false)` forces it off.
    pub scan_on_startup: Option<bool>,
    /// Path to a file where the last working directory will be written on exit.
    /// This enables shell integration for `cd` on exit functionality.
    pub cwd_file: Option<std::path::PathBuf>,
    /// Whether to print the last working directory to stdout on exit.
    pub print_cwd: bool,
}

impl TuiConfig {
    /// Create a new config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable auto-scan on startup.
    pub fn with_scan_on_startup(mut self, scan: bool) -> Self {
        self.scan_on_startup = Some(scan);
        self
    }

    /// Set the cwd file path for shell integration.
    pub fn with_cwd_file(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.cwd_file = path;
        self
    }

    /// Enable printing cwd on exit.
    pub fn with_print_cwd(mut self, print: bool) -> Self {
        self.print_cwd = print;
        self
    }
}

/// Data collected at exit for shell integration.
#[derive(Debug, Clone)]
pub struct ExitData {
    /// The last working directory when the app exited.
    pub last_cwd: std::path::PathBuf,
}

/// Run the TUI application with default configuration.
pub fn run(path: std::path::PathBuf) -> AppResult<()> {
    run_with_config(path, TuiConfig::default())
}

/// Run the TUI application with custom configuration.
pub fn run_with_config(path: std::path::PathBuf, config: TuiConfig) -> AppResult<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;

    // Install a panic hook that restores the terminal before printing the panic message,
    // so the terminal is not left in raw mode on unexpected panics.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        original_hook(panic_info);
    }));

    // Create tokio runtime for async operations
    let rt = tokio::runtime::Runtime::new()?;

    // Store config options we need after the app runs
    let cwd_file = config.cwd_file.clone();
    let print_cwd = config.print_cwd;

    let terminal = ratatui::init();

    // Enable mouse capture
    let _ = execute!(std::io::stdout(), EnableMouseCapture);

    let app = App::with_config(path, config);
    let result = rt.block_on(async { app.run_and_return_exit_data(terminal).await });

    // Disable mouse capture before restoring terminal
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    // Shutdown runtime immediately to cancel background tasks
    rt.shutdown_timeout(std::time::Duration::from_millis(100));

    // Handle exit data for shell integration
    if let Ok(exit_data) = &result {
        // Write cwd to file if requested
        if let Some(cwd_path) = cwd_file {
            let _ = std::fs::write(&cwd_path, exit_data.last_cwd.to_string_lossy().as_bytes());
        }

        // Print cwd if requested
        if print_cwd {
            println!("{}", exit_data.last_cwd.display());
        }
    }

    result.map(|_| ())
}
