//! UI components and widgets.

mod help;
mod miller;
pub mod modals;
mod size_bar;
mod tree;
mod treemap;

pub use help::HelpOverlay;
pub use miller::{MillerColumns, MillerState};
pub use size_bar::CompactSizeBar;
pub use tree::{TreeState, TreeView, VisibleNodeKind};
pub use treemap::{TreemapState, TreemapView};

use ratatui::layout::{Constraint, Layout, Rect};
use unicode_width::UnicodeWidthChar;

/// Truncate a string to at most `max_width` display columns, respecting
/// char boundaries and Unicode widths. Returns a byte-valid substring.
pub fn truncate_to_width(s: &str, max_width: usize) -> &str {
    let mut byte_pos = 0;
    let mut col_count = 0;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(1);
        if col_count + w > max_width {
            break;
        }
        col_count += w;
        byte_pos += ch.len_utf8();
    }
    &s[..byte_pos]
}

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
    /// Compute layout from the content area (already minus header/footer rows).
    ///
    /// `render_app` owns the vertical layout and passes the content `Rect` here.
    /// `AppLayout` only handles the optional horizontal details-panel split so we
    /// do not double-subtract header/footer rows.
    pub fn new(area: Rect, show_details: bool) -> Self {
        let min_main_width = 50;
        // M-8: make details panel width proportional instead of fixed 30
        let details_width = (area.width / 4).clamp(30, 60);

        // Horizontal split for details panel (if enabled and space available)
        let (main, details) = if show_details && area.width >= min_main_width + details_width {
            let [main, details] = Layout::horizontal([
                Constraint::Min(min_main_width),
                Constraint::Length(details_width),
            ])
            .areas(area);
            (main, Some(details))
        } else {
            (area, None)
        };

        Self {
            // These are unused — kept only so downstream code that reads
            // them will not break. They intentionally point at the content
            // area since the real header/footer rects are owned by
            // `render_app` and are not passed down.
            header: area,
            main,
            details,
            footer: area,
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
