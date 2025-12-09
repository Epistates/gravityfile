//! UI components and widgets.

mod help;
pub mod modals;
mod size_bar;
mod tree;

pub use help::HelpOverlay;
pub use size_bar::SizeBar;
pub use tree::{TreeState, TreeView, VisibleNodeKind};

use ratatui::layout::{Constraint, Layout, Rect};

/// Layout areas for the application.
#[derive(Debug, Clone, Copy)]
pub struct AppLayout {
    #[allow(dead_code)]
    pub header: Rect,
    pub main: Rect,
    pub details: Option<Rect>,
    #[allow(dead_code)]
    pub footer: Rect,
}

impl AppLayout {
    /// Compute layout from terminal area.
    pub fn new(area: Rect, show_details: bool) -> Self {
        let min_main_width = 50;
        let details_width = 30;

        // Vertical split: header, main content, footer
        let [header, content, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .areas(area);

        // Horizontal split for details panel (if enabled and space available)
        let (main, details) = if show_details && area.width >= min_main_width + details_width {
            let [main, details] = Layout::horizontal([
                Constraint::Min(min_main_width),
                Constraint::Length(details_width),
            ])
            .areas(content);
            (main, Some(details))
        } else {
            (content, None)
        };

        Self {
            header,
            main,
            details,
            footer,
        }
    }
}

/// Format a byte size in human-readable form.
pub fn format_size(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::BINARY)
}

/// Format a duration in human-readable form.
#[allow(dead_code)]
pub fn format_duration(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Format a timestamp relative to now.
pub fn format_relative_time(time: std::time::SystemTime) -> String {
    let now = std::time::SystemTime::now();
    match now.duration_since(time) {
        Ok(duration) => {
            let secs = duration.as_secs();
            if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86400 {
                format!("{}h ago", secs / 3600)
            } else if secs < 2592000 {
                format!("{}d ago", secs / 86400)
            } else if secs < 31536000 {
                format!("{}mo ago", secs / 2592000)
            } else {
                format!("{}y ago", secs / 31536000)
            }
        }
        Err(_) => "in future".to_string(),
    }
}
