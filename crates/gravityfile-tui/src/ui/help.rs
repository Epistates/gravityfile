//! Help overlay widget.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::event::get_key_bindings;
use crate::theme::Theme;

/// Help overlay showing key bindings.
pub struct HelpOverlay<'a> {
    theme: &'a Theme,
}

impl<'a> HelpOverlay<'a> {
    /// Create a new help overlay.
    pub fn new(theme: &'a Theme) -> Self {
        Self { theme }
    }
}

impl Widget for HelpOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Calculate centered popup area
        let popup_width = 50.min(area.width.saturating_sub(4));
        let popup_height = 16.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the popup area
        Clear.render(popup_area, buf);

        // Draw border
        let block = Block::default()
            .title(" Help ")
            .title_style(self.theme.title)
            .borders(Borders::ALL)
            .border_style(self.theme.border);

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // Build help text
        let bindings = get_key_bindings();
        let mut lines: Vec<Line> = Vec::new();

        lines.push(Line::from(vec![
            Span::styled(
                "Keyboard Shortcuts",
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::raw(""));

        for binding in bindings {
            let key_span = Span::styled(
                format!("{:>12}", binding.keys),
                self.theme.help_key,
            );
            let sep_span = Span::raw("  ");
            let desc_span = Span::styled(binding.description, self.theme.help_desc);

            lines.push(Line::from(vec![key_span, sep_span, desc_span]));
        }

        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Press ? or Esc to close",
            Style::default().fg(self.theme.muted),
        ));

        let paragraph = Paragraph::new(lines);
        paragraph.render(inner, buf);
    }
}

/// Center a rect within an area.
#[allow(dead_code)]
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, center, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);

    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(center);

    center
}
