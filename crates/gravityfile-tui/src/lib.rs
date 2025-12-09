//! Terminal user interface for gravityfile.
//!
//! This crate provides an interactive TUI for exploring disk usage,
//! built with ratatui.

mod app;
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
