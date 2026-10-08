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
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
mod terminal_text_tests {
    use super::*;

    #[test]
    fn fitting_escapes_untrusted_text_before_measuring_both_edges() {
        let name = "résumé\n\u{1b}]52;\u{202e}txt";
        let escaped = gravityfile_core::escape_terminal_text(name);
        assert_eq!(fit_to_width(name, 100), escaped);
        assert_eq!(fit_left_to_width(name, 100), escaped);
        for width in 0..100 {
            for fitted in [fit_to_width(name, width), fit_left_to_width(name, width)] {
                assert!(fitted.width() <= width);
                assert_eq!(gravityfile_core::escape_terminal_text(&fitted), fitted);
            }
        }
        assert_eq!(name, "résumé\n\u{1b}]52;\u{202e}txt");
    }
}

/// Truncate a string to at most `max_width` display columns, respecting
/// grapheme boundaries and Unicode widths. Returns a byte-valid substring.
pub fn truncate_to_width(s: &str, max_width: usize) -> &str {
    let mut byte_pos = 0;
    let mut col_count = 0;
    for grapheme in s.graphemes(true) {
        let w = grapheme.width();
        if col_count + w > max_width {
            break;
        }
        col_count += w;
        byte_pos += grapheme.len();
    }
    &s[..byte_pos]
}

/// Keep the rightmost `max_width` display columns of `s`, respecting grapheme
/// boundaries and Unicode widths. Returns a byte-valid suffix.
pub fn truncate_left_to_width(s: &str, max_width: usize) -> &str {
    let mut col_count = 0;
    let mut start = s.len();
    for (idx, grapheme) in s.grapheme_indices(true).rev() {
        let w = grapheme.width();
        if col_count + w > max_width {
            break;
        }
        col_count += w;
        start = idx;
    }
    &s[start..]
}

/// Fit `text` into `max_width` columns, appending a trailing `…` if truncated.
/// Always Unicode-width and grapheme-boundary safe.
/// Terminal controls and directional formatting are escaped before measuring.
pub fn fit_to_width(text: &str, max_width: usize) -> String {
    let escaped = gravityfile_core::escape_terminal_text(text);
    let text = escaped.as_ref();
    if max_width == 0 {
        String::new()
    } else if text.width() > max_width {
        format!("{}…", truncate_to_width(text, max_width.saturating_sub(1)))
    } else {
        text.to_string()
    }
}

/// Fit `text` into `max_width` columns by dropping from the left, prepending a
/// leading `…` if truncated. Always Unicode-width and grapheme-boundary safe.
/// Terminal controls and directional formatting are escaped before measuring.
pub fn fit_left_to_width(text: &str, max_width: usize) -> String {
    let escaped = gravityfile_core::escape_terminal_text(text);
    let text = escaped.as_ref();
    if max_width == 0 {
        String::new()
    } else if text.width() > max_width {
        format!(
            "…{}",
            truncate_left_to_width(text, max_width.saturating_sub(1))
        )
    } else {
        text.to_string()
    }
}

/// Visible prefix, cursor grapheme, and suffix within a terminal-column budget.
/// Clamp external byte cursors to a preceding grapheme boundary.
pub(crate) fn visible_input_parts(text: &str, cursor: usize, width: usize) -> (&str, &str, &str) {
    let cursor = text
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .take_while(|&index| index <= cursor)
        .last()
        .unwrap_or(0);
    let current = text[cursor..].graphemes(true).next();
    let cursor_text = current.unwrap_or(" ");
    let cursor_text = if cursor_text.width() <= width {
        cursor_text
    } else if width > 0 {
        " "
    } else {
        ""
    };
    let before = truncate_left_to_width(&text[..cursor], width.saturating_sub(cursor_text.width()));
    let after = truncate_to_width(
        &text[cursor + current.map_or(0, str::len)..],
        width.saturating_sub(before.width() + cursor_text.width()),
    );
    (before, cursor_text, after)
}

/// Layout areas for the application.
#[derive(Debug, Clone, Copy)]
pub struct AppLayout {
    pub main: Rect,
    pub details: Option<Rect>,
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

        Self { main, details }
    }
}

/// Whether sizes display in binary units. Chosen once, before the first frame.
static BINARY_UNITS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Choose the units every size is displayed in.
pub fn set_units(units: gravityfile_core::units::Units) {
    BINARY_UNITS.store(
        units == gravityfile_core::units::Units::Binary,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Format a byte size in the chosen units.
pub fn format_size(bytes: u64) -> String {
    use gravityfile_core::units::Units;
    let units = if BINARY_UNITS.load(std::sync::atomic::Ordering::Relaxed) {
        Units::Binary
    } else {
        Units::Si
    };
    gravityfile_core::units::format_size(bytes, units)
}

/// Format a duration in human-readable form.
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
        // Time is in the future. Small offsets are almost always clock skew
        // (NFS, VM clock drift), so render those as "just now" rather than the
        // alarming "in future".
        Err(e) => {
            if e.duration().as_secs() < 60 {
                "just now".to_string()
            } else {
                "in future".to_string()
            }
        }
    }
}
