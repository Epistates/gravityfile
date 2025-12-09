//! Size bar widget for visualizing relative sizes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

/// A horizontal bar showing relative size.
pub struct SizeBar {
    /// Value to display (0.0 - 1.0).
    ratio: f64,
    /// Style for filled portion.
    filled_style: Style,
    /// Style for empty portion.
    empty_style: Style,
    /// Character for filled portion.
    filled_char: char,
    /// Character for empty portion.
    empty_char: char,
}

impl SizeBar {
    /// Create a new size bar.
    pub fn new(ratio: f64) -> Self {
        Self {
            ratio: ratio.clamp(0.0, 1.0),
            filled_style: Style::default(),
            empty_style: Style::default(),
            filled_char: '█',
            empty_char: '░',
        }
    }

    /// Set the style for the filled portion.
    pub fn filled_style(mut self, style: Style) -> Self {
        self.filled_style = style;
        self
    }

    /// Set the style for the empty portion.
    pub fn empty_style(mut self, style: Style) -> Self {
        self.empty_style = style;
        self
    }

    /// Set custom characters.
    #[allow(dead_code)]
    pub fn chars(mut self, filled: char, empty: char) -> Self {
        self.filled_char = filled;
        self.empty_char = empty;
        self
    }
}

impl Widget for SizeBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let filled_width = (area.width as f64 * self.ratio).round() as u16;

        for x in 0..area.width {
            let (char, style) = if x < filled_width {
                (self.filled_char, self.filled_style)
            } else {
                (self.empty_char, self.empty_style)
            };

            buf[(area.x + x, area.y)]
                .set_char(char)
                .set_style(style);
        }
    }
}

/// A compact size bar using Unicode block characters for finer granularity.
#[allow(dead_code)]
pub struct CompactSizeBar {
    ratio: f64,
    style: Style,
}

#[allow(dead_code)]
impl CompactSizeBar {
    /// Block characters for 8 levels of fill.
    const BLOCKS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

    /// Create a new compact size bar.
    pub fn new(ratio: f64) -> Self {
        Self {
            ratio: ratio.clamp(0.0, 1.0),
            style: Style::default(),
        }
    }

    /// Set the style.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

impl Widget for CompactSizeBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Calculate how many eighths of cells to fill
        let total_eighths = (area.width as f64 * 8.0 * self.ratio).round() as u16;
        let full_cells = total_eighths / 8;
        let partial_eighths = (total_eighths % 8) as usize;

        for x in 0..area.width {
            let char = if x < full_cells {
                Self::BLOCKS[8] // Full block
            } else if x == full_cells && partial_eighths > 0 {
                Self::BLOCKS[partial_eighths]
            } else {
                ' '
            };

            buf[(area.x + x, area.y)].set_char(char).set_style(self.style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn test_size_bar_empty() {
        let bar = SizeBar::new(0.0);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);

        bar.render(area, &mut buf);

        // All cells should be empty char
        for x in 0..10 {
            assert_eq!(buf[(x, 0)].symbol(), "░");
        }
    }

    #[test]
    fn test_size_bar_full() {
        let bar = SizeBar::new(1.0);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);

        bar.render(area, &mut buf);

        // All cells should be filled char
        for x in 0..10 {
            assert_eq!(buf[(x, 0)].symbol(), "█");
        }
    }

    #[test]
    fn test_size_bar_half() {
        let bar = SizeBar::new(0.5);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);

        bar.render(area, &mut buf);

        // First 5 cells filled, rest empty
        for x in 0..5 {
            assert_eq!(buf[(x, 0)].symbol(), "█");
        }
        for x in 5..10 {
            assert_eq!(buf[(x, 0)].symbol(), "░");
        }
    }
}
