//! Size bar widget for visualizing relative sizes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

/// A compact size bar using Unicode block characters for finer granularity.
pub struct CompactSizeBar {
    ratio: f64,
    style: Style,
}

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

            buf[(area.x + x, area.y)]
                .set_char(char)
                .set_style(self.style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn compact_size_bar_full_fills_all_cells() {
        let bar = CompactSizeBar::new(1.0);
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        for x in 0..8 {
            assert_eq!(buf[(x, 0)].symbol(), "█");
        }
    }

    #[test]
    fn compact_size_bar_empty_and_clamped() {
        // Negative ratio is clamped to 0 — no full blocks rendered.
        let bar = CompactSizeBar::new(-1.0);
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        for x in 0..8 {
            assert_eq!(buf[(x, 0)].symbol(), " ");
        }
    }
}
