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

mod app;
mod config;
mod event;
mod event_source;
mod graphics_query;
mod keymap;
mod opener;
mod preview;
mod search;
mod shell_integration;
mod terminal_session;
#[cfg(test)]
mod test_support;
mod theme;
mod ui;

use app::App;
pub use app::AppResult;

/// Configuration for the TUI application.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TuiConfig {
    /// Host executable supporting the private plugin worker protocol. If absent,
    /// embedded TUI hosts do not load plugins into their own process implicitly.
    pub plugin_worker_executable: Option<std::path::PathBuf>,
    /// Whether to automatically start scanning on startup.
    /// `None` means defer to the user's saved settings.
    /// `Some(true)` forces scanning on, `Some(false)` forces it off.
    pub scan_on_startup: Option<bool>,
    /// Path to a file where the last working directory will be written on exit.
    /// This enables shell integration for `cd` on exit functionality.
    pub cwd_file: Option<std::path::PathBuf>,
    /// Whether to print the last working directory to stdout on exit.
    pub print_cwd: bool,
    /// What happens to an item the user chooses to overwrite on paste.
    pub replace_policy: gravityfile_ops::ReplacePolicy,
    /// What happens to items the user deletes.
    pub delete_mode: gravityfile_ops::DeleteMode,
    /// How sizes are displayed.
    pub units: gravityfile_core::units::Units,
}

impl TuiConfig {
    /// Enable supervised plugins using a worker-capable host executable.
    pub fn with_plugin_worker_executable(mut self, path: std::path::PathBuf) -> Self {
        self.plugin_worker_executable = Some(path);
        self
    }
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

    /// Display sizes in `units`.
    pub fn with_units(mut self, units: gravityfile_core::units::Units) -> Self {
        self.units = units;
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
    use crossterm::event::EnableMouseCapture;
    use crossterm::execute;

    ui::set_units(config.units);

    // Create tokio runtime for async operations - setup services BEFORE terminal
    let rt = tokio::runtime::Runtime::new()?;

    // Store config options we need after the app runs
    let cwd_file = config.cwd_file.clone();
    let print_cwd = config.print_cwd;

    // Initialize terminal with Ratatui's high-level API
    // This automatically installs a panic hook that restores the terminal.
    let mut session = terminal_session::TerminalSession::new();
    let terminal = ratatui::try_init()?;

    // The guard also restores partially initialized state on this error path.
    execute!(std::io::stdout(), EnableMouseCapture)?;

    // Detect the terminal's graphics protocol and font size.
    //
    // This writes control sequences and reads the reply, so it has to happen
    // after raw mode is enabled and before the event loop takes ownership of
    // stdin. Unix probing reads synchronously with a 250 ms deadline and an
    // 8 KiB reply budget; no reader survives a timeout. Other platforms use
    // half-block previews without a stdio query. `image_previews` opts out.
    //
    // Failed detection is not an error: image files simply fall back to the
    // ordinary binary preview.
    let app = App::with_config(path, config);
    let image_picker = if app.image_previews_enabled() {
        match graphics_query::query_picker() {
            Ok(picker) => Some(picker),
            Err(error) => {
                tracing::debug!("Terminal graphics detection failed: {error}");
                None
            }
        }
    } else {
        None
    };

    let app = app.with_image_picker(image_picker);
    let result = rt.block_on(async { app.run_and_return_exit_data(terminal).await });

    // Discard all pending terminal output sitting in the kernel's PTY buffer.
    // During heavy TUI rendering (e.g. scanning 45K files at 20fps), the Rust
    // process writes escape sequences faster than the terminal emulator can
    // consume them. This creates a multi-second backlog in the PTY buffer.
    // Without flushing, LeaveAlternateScreen queues behind all that output
    // and the user sees the TUI "frozen" for seconds after pressing quit.
    #[cfg(unix)]
    unsafe {
        libc::tcflush(libc::STDOUT_FILENO, libc::TCOFLUSH);
    }

    // Now these commands go to the front of an empty buffer — instant.
    let terminal_restore = session.restore();
    rt.shutdown_background();

    // Prefer the application error, but never skip the cleanup attempts above.
    let exit_data = result?;
    terminal_restore?;

    // Handle exit data for shell integration
    // Write cwd to file if requested.
    if let Some(cwd_path) = cwd_file {
        shell_integration::write_cwd_file(&cwd_path, &exit_data.last_cwd)?;
    }

    // Print cwd if requested.
    if print_cwd {
        shell_integration::print_cwd(&exit_data.last_cwd, &mut std::io::stdout().lock())?;
    }

    Ok(())
}
