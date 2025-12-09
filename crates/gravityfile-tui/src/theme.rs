//! Color theme for the TUI.
//!
//! Provides a comprehensive theming system with dark and light themes,
//! using a semantic color palette based on Tailwind CSS colors.

use ratatui::style::{Color, Modifier, Style};

/// Theme variant (dark or light).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeVariant {
    #[default]
    Dark,
    Light,
}

/// Color theme for the TUI.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Current theme variant.
    pub variant: ThemeVariant,

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

    // Progress
    pub progress_bar: Style,
    pub progress_bg: Style,

    // Header/Footer
    pub header: Style,
    pub footer: Style,

    // Command palette
    pub command_prompt: Style,
    pub command_input: Style,
    pub command_cursor: Style,

    // Marked items
    pub marked: Style,
}

impl Theme {
    /// Dark theme using a slate-based palette.
    pub fn dark() -> Self {
        // Slate palette (Tailwind CSS)
        let slate_50 = Color::Rgb(248, 250, 252);
        let slate_100 = Color::Rgb(241, 245, 249);
        let slate_300 = Color::Rgb(203, 213, 225);
        let slate_400 = Color::Rgb(148, 163, 184);
        let slate_500 = Color::Rgb(100, 116, 139);
        let slate_600 = Color::Rgb(71, 85, 105);
        let slate_700 = Color::Rgb(51, 65, 85);
        let slate_800 = Color::Rgb(30, 41, 59);
        let slate_900 = Color::Rgb(15, 23, 42);

        // Accent colors (Tailwind CSS)
        let blue_400 = Color::Rgb(96, 165, 250);
        let blue_500 = Color::Rgb(59, 130, 246);
        let green_500 = Color::Rgb(34, 197, 94);
        let yellow_500 = Color::Rgb(234, 179, 8);
        let orange_500 = Color::Rgb(249, 115, 22);
        let red_500 = Color::Rgb(239, 68, 68);
        let cyan_400 = Color::Rgb(34, 211, 238);
        let amber_500 = Color::Rgb(245, 158, 11);

        Self {
            variant: ThemeVariant::Dark,
            background: slate_900,
            foreground: slate_100,
            muted: slate_500,

            selected: Style::new().bg(slate_700).fg(slate_50).add_modifier(Modifier::BOLD),
            hover: Style::new().bg(slate_800),

            size_huge: red_500,
            size_large: orange_500,
            size_medium: yellow_500,
            size_small: green_500,
            size_tiny: slate_600,

            success: green_500,
            warning: yellow_500,
            error: red_500,
            info: blue_400,

            border: Style::new().fg(slate_600),
            title: Style::new().fg(blue_400).add_modifier(Modifier::BOLD),
            help_key: Style::new().fg(blue_400).add_modifier(Modifier::BOLD),
            help_desc: Style::new().fg(slate_400),

            tree_lines: Style::new().fg(slate_600),
            directory: Style::new().fg(blue_500).add_modifier(Modifier::BOLD),
            file: Style::new().fg(slate_300),
            symlink: Style::new().fg(cyan_400),
            executable: Style::new().fg(green_500),

            progress_bar: Style::new().fg(blue_500),
            progress_bg: Style::new().fg(slate_700),

            header: Style::new().bg(slate_800).fg(slate_100),
            footer: Style::new().bg(slate_800).fg(slate_400),

            command_prompt: Style::new().fg(blue_400).add_modifier(Modifier::BOLD),
            command_input: Style::new().fg(slate_100),
            command_cursor: Style::new().add_modifier(Modifier::REVERSED),

            marked: Style::new().fg(amber_500).add_modifier(Modifier::BOLD),
        }
    }

    /// Light theme using a slate-based palette.
    pub fn light() -> Self {
        // Slate palette (Tailwind CSS)
        let slate_50 = Color::Rgb(248, 250, 252);
        let slate_100 = Color::Rgb(241, 245, 249);
        let slate_200 = Color::Rgb(226, 232, 240);
        let slate_400 = Color::Rgb(148, 163, 184);
        let slate_500 = Color::Rgb(100, 116, 139);
        let slate_600 = Color::Rgb(71, 85, 105);
        let slate_700 = Color::Rgb(51, 65, 85);
        let slate_800 = Color::Rgb(30, 41, 59);
        let slate_900 = Color::Rgb(15, 23, 42);

        // Accent colors (Tailwind CSS - darker variants for light theme)
        let blue_600 = Color::Rgb(37, 99, 235);
        let blue_700 = Color::Rgb(29, 78, 216);
        let green_600 = Color::Rgb(22, 163, 74);
        let yellow_600 = Color::Rgb(202, 138, 4);
        let orange_600 = Color::Rgb(234, 88, 12);
        let red_600 = Color::Rgb(220, 38, 38);
        let cyan_600 = Color::Rgb(8, 145, 178);
        let amber_600 = Color::Rgb(217, 119, 6);

        Self {
            variant: ThemeVariant::Light,
            background: slate_50,
            foreground: slate_900,
            muted: slate_500,

            selected: Style::new().bg(slate_200).fg(slate_900).add_modifier(Modifier::BOLD),
            hover: Style::new().bg(slate_100),

            size_huge: red_600,
            size_large: orange_600,
            size_medium: yellow_600,
            size_small: green_600,
            size_tiny: slate_400,

            success: green_600,
            warning: yellow_600,
            error: red_600,
            info: blue_600,

            border: Style::new().fg(slate_400),
            title: Style::new().fg(blue_700).add_modifier(Modifier::BOLD),
            help_key: Style::new().fg(blue_700).add_modifier(Modifier::BOLD),
            help_desc: Style::new().fg(slate_600),

            tree_lines: Style::new().fg(slate_400),
            directory: Style::new().fg(blue_700).add_modifier(Modifier::BOLD),
            file: Style::new().fg(slate_700),
            symlink: Style::new().fg(cyan_600),
            executable: Style::new().fg(green_600),

            progress_bar: Style::new().fg(blue_600),
            progress_bg: Style::new().fg(slate_200),

            header: Style::new().bg(slate_100).fg(slate_800),
            footer: Style::new().bg(slate_100).fg(slate_600),

            command_prompt: Style::new().fg(blue_700).add_modifier(Modifier::BOLD),
            command_input: Style::new().fg(slate_900),
            command_cursor: Style::new().add_modifier(Modifier::REVERSED),

            marked: Style::new().fg(amber_600).add_modifier(Modifier::BOLD),
        }
    }

    /// Create theme from variant.
    pub fn from_variant(variant: ThemeVariant) -> Self {
        match variant {
            ThemeVariant::Dark => Self::dark(),
            ThemeVariant::Light => Self::light(),
        }
    }

    /// Toggle between dark and light themes.
    pub fn toggle(&self) -> Self {
        match self.variant {
            ThemeVariant::Dark => Self::light(),
            ThemeVariant::Light => Self::dark(),
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
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
