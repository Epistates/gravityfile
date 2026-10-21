//! Colour themes.
//!
//! A theme is built from a small semantic [`Palette`], as in carwash, so both
//! tools offer the same named themes. Backgrounds stay the terminal's own,
//! and truecolour palettes fall back to the terminal's 16 colours when it
//! cannot show 24-bit colour.

use ratatui::style::{Color, Modifier, Style};

/// The semantic colours a theme is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub name: &'static str,
    pub text: Color,
    pub subtle: Color,
    pub muted: Color,
    /// Highlighted rows, header and footer.
    pub surface: Color,
    pub border: Color,
    pub accent: Color,
    pub accent2: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    /// Text drawn on a cell filled with a status or size colour.
    pub ink: Color,
    pub truecolor: bool,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    /// Catppuccin Mocha.
    pub const GESTALT: Self = Self {
        name: "gestalt",
        text: rgb(0xcdd6f4),
        subtle: rgb(0xa6adc8),
        muted: rgb(0x6c7086),
        surface: rgb(0x313244),
        border: rgb(0x45475a),
        accent: rgb(0xb4befe),
        accent2: rgb(0xcba6f7),
        success: rgb(0xa6e3a1),
        warning: rgb(0xf9e2af),
        error: rgb(0xf38ba8),
        info: rgb(0x89b4fa),
        ink: rgb(0x11111b),
        truecolor: true,
    };

    /// Catppuccin Latte, for light terminals.
    pub const LATTE: Self = Self {
        name: "latte",
        text: rgb(0x4c4f69),
        subtle: rgb(0x6c6f85),
        muted: rgb(0x9ca0b0),
        surface: rgb(0xccd0da),
        border: rgb(0xbcc0cc),
        accent: rgb(0x7287fd),
        accent2: rgb(0x8839ef),
        success: rgb(0x40a02b),
        warning: rgb(0xdf8e1d),
        error: rgb(0xd20f39),
        info: rgb(0x1e66f5),
        ink: rgb(0xeff1f5),
        truecolor: true,
    };

    pub const NORD: Self = Self {
        name: "nord",
        text: rgb(0xeceff4),
        subtle: rgb(0xd8dee9),
        muted: rgb(0x616e88),
        surface: rgb(0x3b4252),
        border: rgb(0x434c5e),
        accent: rgb(0x88c0d0),
        accent2: rgb(0x81a1c1),
        success: rgb(0xa3be8c),
        warning: rgb(0xebcb8b),
        error: rgb(0xbf616a),
        info: rgb(0x5e81ac),
        ink: rgb(0x2e3440),
        truecolor: true,
    };

    pub const DRACULA: Self = Self {
        name: "dracula",
        text: rgb(0xf8f8f2),
        subtle: rgb(0xd6d6d0),
        muted: rgb(0x6272a4),
        surface: rgb(0x44475a),
        border: rgb(0x44475a),
        accent: rgb(0xbd93f9),
        accent2: rgb(0xff79c6),
        success: rgb(0x50fa7b),
        warning: rgb(0xf1fa8c),
        error: rgb(0xff5555),
        info: rgb(0x8be9fd),
        ink: rgb(0x282a36),
        truecolor: true,
    };

    /// The terminal's own 16 colours.
    pub const ANSI: Self = Self {
        name: "ansi",
        text: Color::Reset,
        subtle: Color::Gray,
        muted: Color::DarkGray,
        surface: Color::DarkGray,
        border: Color::DarkGray,
        accent: Color::Cyan,
        accent2: Color::Magenta,
        success: Color::Green,
        warning: Color::Yellow,
        error: Color::Red,
        info: Color::Blue,
        ink: Color::Black,
        truecolor: false,
    };

    /// Every palette, in the order `t` cycles through them.
    pub const ALL: [Self; 5] = [
        Self::GESTALT,
        Self::LATTE,
        Self::NORD,
        Self::DRACULA,
        Self::ANSI,
    ];

    /// The palette called `name`; `dark` and `light` name the defaults.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::GESTALT),
            "light" => Some(Self::LATTE),
            _ => Self::ALL.into_iter().find(|palette| palette.name == name),
        }
    }
}

/// Whether the terminal advertises 24-bit colour.
fn supports_truecolor() -> bool {
    std::env::var("COLORTERM").is_ok_and(|value| value == "truecolor" || value == "24bit")
        || std::env::var("TERM_PROGRAM").is_ok_and(|value| {
            matches!(
                value.as_str(),
                "iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "WarpTerminal"
            )
        })
}

/// Styles for every part of the interface, derived from a [`Palette`].
#[derive(Debug, Clone)]
pub struct Theme {
    /// The theme asked for, which `t` cycles from even when the terminal
    /// shows it as `ansi`.
    pub name: &'static str,

    // Base colors
    pub background: Color,
    pub foreground: Color,
    pub muted: Color,

    // Interactive elements
    pub selected: Style,
    pub hover: Style,

    // Size indicators (gradient by proportion)
    pub size_huge: Color,   // > 50%
    pub size_large: Color,  // > 25%
    pub size_medium: Color, // > 10%
    pub size_small: Color,  // > 1%
    pub size_tiny: Color,   // <= 1%

    // Status colors
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    /// Text on a cell filled with a status or size colour.
    pub ink: Color,

    // UI elements
    pub border: Style,
    pub title: Style,
    pub help_key: Style,
    pub help_desc: Style,

    // Tree elements
    pub tree_lines: Style,
    pub directory: Style,
    pub file: Style,
    pub symlink: Style,
    pub executable: Style,

    // Header/Footer
    pub header: Style,
    pub footer: Style,

    // Footer keybinding hints (pill/badge style)
    pub help_key_bg: Color,
    pub help_key_fg: Color,
    pub help_desc_fg: Color,
    pub footer_bg: Color,

    // Git status colors
    pub git_modified: Color,
    pub git_staged: Color,
    pub git_untracked: Color,
    pub git_ignored: Color,
    pub git_conflict: Color,
}

impl Theme {
    /// The theme called `name`, if there is one, adjusted to the terminal:
    /// a truecolour palette becomes `ansi` where 24-bit colour is missing.
    pub fn named(name: &str) -> Option<Self> {
        let palette = Palette::named(name)?;
        let shown = if palette.truecolor && !supports_truecolor() {
            Palette::ANSI
        } else {
            palette
        };
        Some(Self {
            name: palette.name,
            ..Self::from_palette(shown)
        })
    }

    /// The default dark theme, Gestalt.
    pub fn dark() -> Self {
        Self::from_palette(Palette::GESTALT)
    }

    /// The next theme in [`Palette::ALL`].
    pub fn next(&self) -> Self {
        let index = Palette::ALL
            .iter()
            .position(|palette| palette.name == self.name)
            .map_or(0, |index| (index + 1) % Palette::ALL.len());
        Self::named(Palette::ALL[index].name).unwrap_or_else(Self::dark)
    }

    /// Every style, derived from `palette`.
    pub fn from_palette(palette: Palette) -> Self {
        let p = palette;
        Self {
            name: palette.name,
            background: Color::Reset,
            foreground: p.text,
            muted: p.muted,

            selected: Style::new()
                .bg(p.surface)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
            hover: Style::new().bg(p.surface),

            size_huge: p.error,
            size_large: p.warning,
            size_medium: p.success,
            size_small: p.info,
            size_tiny: p.muted,

            success: p.success,
            warning: p.warning,
            error: p.error,
            info: p.info,
            ink: p.ink,

            border: Style::new().fg(p.border),
            title: Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
            help_key: Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
            help_desc: Style::new().fg(p.subtle),

            tree_lines: Style::new().fg(p.muted),
            directory: Style::new().fg(p.info).add_modifier(Modifier::BOLD),
            file: Style::new().fg(p.text),
            symlink: Style::new().fg(p.accent2),
            executable: Style::new().fg(p.success),

            header: Style::new().bg(p.surface).fg(p.text),
            footer: Style::new().bg(p.surface).fg(p.subtle),

            help_key_bg: p.border,
            help_key_fg: p.accent,
            help_desc_fg: p.subtle,
            footer_bg: p.surface,

            git_modified: p.warning,
            git_staged: p.success,
            git_untracked: p.accent2,
            git_ignored: p.muted,
            git_conflict: p.error,
        }
    }

    /// Get color for a size ratio (0.0 to 1.0).
    pub fn size_color(&self, ratio: f64) -> Color {
        match ratio {
            r if r > 0.50 => self.size_huge,
            r if r > 0.25 => self.size_large,
            r if r > 0.10 => self.size_medium,
            r if r > 0.01 => self.size_small,
            _ => self.size_tiny,
        }
    }

    /// Get style for a size bar at given ratio.
    pub fn size_bar_style(&self, ratio: f64) -> Style {
        Style::new().fg(self.size_color(ratio))
    }

    /// Style for keybinding keys in footer (pill/badge style).
    pub fn help_key_style(&self) -> Style {
        Style::new()
            .bg(self.help_key_bg)
            .fg(self.help_key_fg)
            .add_modifier(Modifier::BOLD)
    }

    /// Style for keybinding descriptions in footer.
    pub fn help_desc_style(&self) -> Style {
        Style::new().fg(self.help_desc_fg)
    }

    /// Style for footer background.
    pub fn footer_style(&self) -> Style {
        Style::new().bg(self.footer_bg)
    }

    /// Get color for a git status.
    pub fn git_status_color(&self, status: gravityfile_core::GitStatus) -> Color {
        use gravityfile_core::GitStatus;
        match status {
            GitStatus::Modified => self.git_modified,
            GitStatus::Staged => self.git_staged,
            GitStatus::Untracked => self.git_untracked,
            GitStatus::Ignored => self.git_ignored,
            GitStatus::Conflict => self.git_conflict,
            GitStatus::Clean => self.muted,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_is_reachable_by_name_and_by_cycling() {
        for palette in Palette::ALL {
            assert_eq!(Palette::named(palette.name), Some(palette));
        }
        assert_eq!(Palette::named("dark"), Some(Palette::GESTALT));
        assert_eq!(Palette::named("light"), Some(Palette::LATTE));
        assert_eq!(Palette::named("solarized"), None);

        let mut theme = Theme::from_palette(Palette::ANSI);
        let mut seen = Vec::new();
        for _ in 0..Palette::ALL.len() {
            theme = theme.next();
            seen.push(theme.name);
        }
        // Even where every palette shows as ansi, cycling visits them all.
        let names: Vec<_> = Palette::ALL.iter().map(|palette| palette.name).collect();
        assert_eq!(seen, names);
    }

    #[test]
    fn backgrounds_are_the_terminals_own() {
        for palette in Palette::ALL {
            assert_eq!(Theme::from_palette(palette).background, Color::Reset);
        }
    }
}
