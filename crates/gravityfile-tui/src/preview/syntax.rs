//! Syntax highlighting using syntect.

use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Global syntax highlighting resources (loaded once).
static SYNTECT: OnceLock<(SyntaxSet, Theme)> = OnceLock::new();
const MAX_HIGHLIGHT_LINE_BYTES: usize = 16 * 1024;
const MAX_HIGHLIGHT_PREVIEW_BYTES: usize = 256 * 1024;

/// Syntax highlighter for file previews.
pub struct SyntaxHighlighter;

impl SyntaxHighlighter {
    /// Initialize and get the global syntax/theme resources.
    pub fn init() -> (&'static SyntaxSet, &'static Theme) {
        let (syntaxes, theme) = SYNTECT.get_or_init(|| {
            let syntaxes = SyntaxSet::load_defaults_newlines();
            let theme_set = ThemeSet::load_defaults();
            // Use a theme that works well in terminals
            let theme = theme_set
                .themes
                .get("base16-ocean.dark")
                .cloned()
                .or_else(|| theme_set.themes.values().next().cloned())
                .unwrap_or_default();
            (syntaxes, theme)
        });
        (syntaxes, theme)
    }

    /// Find the syntax definition for a file based on extension or first line.
    pub fn find_syntax(path: &Path, first_line: Option<&str>) -> Option<&'static SyntaxReference> {
        let (syntaxes, _) = Self::init();

        // Try by filename first
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && let Some(syntax) = syntaxes.find_syntax_by_extension(name)
        {
            return Some(syntax);
        }

        // Try by extension
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && let Some(syntax) = syntaxes.find_syntax_by_extension(ext)
        {
            return Some(syntax);
        }

        // Try by first line (shebang detection)
        if let Some(line) = first_line
            && let Some(syntax) = syntaxes.find_syntax_by_first_line(line)
        {
            return Some(syntax);
        }

        None
    }

    pub(crate) fn within_budget(lines: &[String]) -> bool {
        let mut remaining = MAX_HIGHLIGHT_PREVIEW_BYTES;
        for line in lines {
            if line.len() > MAX_HIGHLIGHT_LINE_BYTES || line.len() > remaining {
                return false;
            }
            remaining -= line.len();
        }
        true
    }

    /// Fall back as a whole so skipped lines cannot corrupt later parser state.
    pub(crate) fn highlight_with_status(
        lines: &[String],
        syntax: &SyntaxReference,
        tab_size: u8,
    ) -> (Vec<Line<'static>>, bool) {
        if !Self::within_budget(lines) {
            return (Self::plain_lines(lines, tab_size), false);
        }
        let (syntaxes, theme) = Self::init();
        let mut highlighter = HighlightLines::new(syntax, theme);
        let tab_replacement = " ".repeat(tab_size as usize);

        let mut output = Vec::with_capacity(lines.len());
        for line in lines {
            match highlighter.highlight_line(line, syntaxes) {
                Ok(regions) => output.push(Self::regions_to_line(regions, &tab_replacement)),
                Err(_) => return (Self::plain_lines(lines, tab_size), false),
            }
        }
        (output, true)
    }

    /// Convert syntect highlight regions to a ratatui Line.
    fn regions_to_line(
        regions: Vec<(highlighting::Style, &str)>,
        tab_replacement: &str,
    ) -> Line<'static> {
        let spans: Vec<Span<'static>> = regions
            .into_iter()
            .map(|(style, text)| {
                let mut modifier = Modifier::empty();
                if style.font_style.contains(highlighting::FontStyle::BOLD) {
                    modifier |= Modifier::BOLD;
                }
                if style.font_style.contains(highlighting::FontStyle::ITALIC) {
                    modifier |= Modifier::ITALIC;
                }
                if style
                    .font_style
                    .contains(highlighting::FontStyle::UNDERLINE)
                {
                    modifier |= Modifier::UNDERLINED;
                }

                Span::styled(
                    Self::display_text(text, tab_replacement),
                    Style::default()
                        .fg(Self::to_ratatui_color(style.foreground))
                        .add_modifier(modifier),
                )
            })
            .collect();

        Line::from(spans)
    }

    /// Convert syntect Color to ratatui Color.
    /// Based on bat's implementation for proper ANSI color support.
    fn to_ratatui_color(color: highlighting::Color) -> Color {
        if color.a == 0 {
            // Terminal palette colors encoded with alpha = 0
            match color.r {
                0x00 => Color::Black,
                0x01 => Color::Red,
                0x02 => Color::Green,
                0x03 => Color::Yellow,
                0x04 => Color::Blue,
                0x05 => Color::Magenta,
                0x06 => Color::Cyan,
                0x07 => Color::White,
                n => Color::Indexed(n),
            }
        } else if color.a == 1 {
            // Default terminal color
            Color::Reset
        } else {
            // True color RGB
            Color::Rgb(color.r, color.g, color.b)
        }
    }

    /// Create plain (non-highlighted) lines from text.
    pub fn plain_lines(lines: &[String], tab_size: u8) -> Vec<Line<'static>> {
        let tab_replacement = " ".repeat(tab_size as usize);
        lines
            .iter()
            .map(|line| Line::from(Self::display_text(line, &tab_replacement)))
            .collect()
    }

    fn display_text(text: &str, tab_replacement: &str) -> String {
        gravityfile_core::escape_terminal_text(&text.replace('\t', tab_replacement)).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlighting_budgets_preserve_text_and_report_plain_fallback() {
        let syntax = SyntaxHighlighter::find_syntax(Path::new("probe.rs"), None).unwrap();
        assert!(SyntaxHighlighter::within_budget(&vec![
                "x".repeat(MAX_HIGHLIGHT_LINE_BYTES);
                MAX_HIGHLIGHT_PREVIEW_BYTES / MAX_HIGHLIGHT_LINE_BYTES
            ]));
        for input in [
            vec![
                "x".repeat(MAX_HIGHLIGHT_LINE_BYTES + 1),
                "日本語\t\u{202e}".into(),
            ],
            vec![
                "x".repeat(MAX_HIGHLIGHT_LINE_BYTES);
                MAX_HIGHLIGHT_PREVIEW_BYTES / MAX_HIGHLIGHT_LINE_BYTES + 1
            ],
        ] {
            let (lines, highlighted) = SyntaxHighlighter::highlight_with_status(&input, syntax, 4);
            assert!(!highlighted);
            assert_eq!(lines, SyntaxHighlighter::plain_lines(&input, 4));
            assert_eq!(lines.len(), input.len());
        }
        assert!(SyntaxHighlighter::highlight_with_status(&["fn main() {}".into()], syntax, 4).1);
    }

    #[test]
    fn plain_and_highlighted_previews_escape_controls_and_preserve_tabs_and_unicode() {
        let input = vec!["// 日本語\t\u{1b}]52;c;secret\u{7}\u{202e}txt".to_string()];
        let syntax = SyntaxHighlighter::find_syntax(Path::new("probe.rs"), None).unwrap();
        let expected = "// 日本語    \\u{1b}]52;c;secret\\u{7}\\u{202e}txt";
        for lines in [
            SyntaxHighlighter::plain_lines(&input, 4),
            SyntaxHighlighter::highlight_with_status(&input, syntax, 4).0,
        ] {
            assert_eq!(lines.len(), 1);
            let rendered: String = lines[0]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(rendered, expected);
        }
        assert!(input[0].contains('\u{1b}'));
    }
}
