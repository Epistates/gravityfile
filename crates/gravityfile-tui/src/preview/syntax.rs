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
                .unwrap_or_else(|| theme_set.themes.values().next().unwrap().clone());
            (syntaxes, theme)
        });
        (syntaxes, theme)
    }

    /// Find the syntax definition for a file based on extension or first line.
    pub fn find_syntax(path: &Path, first_line: Option<&str>) -> Option<&'static SyntaxReference> {
        let (syntaxes, _) = Self::init();

        // Try by filename first
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if let Some(syntax) = syntaxes.find_syntax_by_extension(name) {
                return Some(syntax);
            }
        }

        // Try by extension
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if let Some(syntax) = syntaxes.find_syntax_by_extension(ext) {
                return Some(syntax);
            }
        }

        // Try by first line (shebang detection)
        if let Some(line) = first_line {
            if let Some(syntax) = syntaxes.find_syntax_by_first_line(line) {
                return Some(syntax);
            }
        }

        None
    }

    /// Highlight lines of text and convert to ratatui Lines.
    pub fn highlight_lines(
        lines: &[String],
        syntax: &SyntaxReference,
        tab_size: u8,
    ) -> Vec<Line<'static>> {
        let (syntaxes, theme) = Self::init();
        let mut highlighter = HighlightLines::new(syntax, theme);
        let tab_replacement = " ".repeat(tab_size as usize);

        lines
            .iter()
            .filter_map(|line| {
                highlighter
                    .highlight_line(line, syntaxes)
                    .ok()
                    .map(|regions| Self::regions_to_line(regions, &tab_replacement))
            })
            .collect()
    }

    /// Convert syntect highlight regions to a ratatui Line.
    fn regions_to_line(regions: Vec<(highlighting::Style, &str)>, tab_replacement: &str) -> Line<'static> {
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
                if style.font_style.contains(highlighting::FontStyle::UNDERLINE) {
                    modifier |= Modifier::UNDERLINED;
                }

                Span::styled(
                    text.replace('\t', tab_replacement),
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
            .map(|line| Line::from(line.replace('\t', &tab_replacement)))
            .collect()
    }
}
