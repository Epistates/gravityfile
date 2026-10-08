//! Text styles for human-readable output.
//!
//! Colour is decided once per stream from `--color`, `NO_COLOR`,
//! `CLICOLOR[_FORCE]` and whether the stream is a terminal. Disabled styles
//! render as nothing, so writers never branch on colour themselves. Styling
//! wraps text that has already been escaped for the terminal.

use anstream::{AutoStream, ColorChoice};
use anstyle::{AnsiColor, Style};

use crate::cli::ColorWhen;

#[derive(Debug, Clone, Copy, Default)]
pub struct Styles {
    pub heading: Style,
    pub directory: Style,
    pub link: Style,
    pub size: Style,
    pub dim: Style,
    pub warning: Style,
    pub error: Style,
}

impl Styles {
    fn colored() -> Self {
        Self {
            heading: Style::new().bold(),
            directory: AnsiColor::Blue.on_default().bold(),
            link: AnsiColor::Cyan.on_default(),
            size: Style::new().bold(),
            dim: Style::new().dimmed(),
            warning: AnsiColor::Yellow.on_default().bold(),
            error: AnsiColor::Red.on_default().bold(),
        }
    }

    /// Styles that render nothing.
    pub fn plain() -> Self {
        Self::default()
    }

    /// Styles for standard output and standard error.
    pub fn for_streams(when: ColorWhen) -> (Self, Self) {
        let choice = match when {
            ColorWhen::Auto => ColorChoice::Auto,
            ColorWhen::Always => ColorChoice::Always,
            ColorWhen::Never => ColorChoice::Never,
        };
        choice.write_global();
        let pick = |choice: ColorChoice| {
            if choice == ColorChoice::Never {
                Self::plain()
            } else {
                Self::colored()
            }
        };
        (
            pick(AutoStream::choice(&std::io::stdout())),
            pick(AutoStream::choice(&std::io::stderr())),
        )
    }
}

/// `text` wrapped in `style`.
pub fn paint(style: Style, text: impl std::fmt::Display) -> String {
    format!("{style}{text}{style:#}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_styles_add_nothing() {
        assert_eq!(paint(Styles::plain().directory, "src/"), "src/");
        assert_eq!(
            paint(Styles::colored().error, "x"),
            "\u{1b}[1m\u{1b}[31mx\u{1b}[0m"
        );
    }
}
