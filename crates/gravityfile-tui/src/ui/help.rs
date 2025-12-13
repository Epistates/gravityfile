//! Help overlay widget.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::event::{get_command_help, get_help_sections};
use crate::theme::Theme;

/// Help overlay showing key bindings organized by section.
pub struct HelpOverlay<'a> {
    theme: &'a Theme,
}

impl<'a> HelpOverlay<'a> {
    /// Create a new help overlay.
    pub fn new(theme: &'a Theme) -> Self {
        Self { theme }
    }

    fn render_section_column(
        &self,
        sections: &[&crate::event::HelpSection],
        area: Rect,
        buf: &mut Buffer,
    ) {
        let mut y = area.y;

        for section in sections {
            if y >= area.y + area.height {
                break;
            }

            // Section title
            let title_line = Line::from(Span::styled(
                section.title,
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            ));
            if y < area.y + area.height {
                buf.set_line(area.x, y, &title_line, area.width);
                y += 1;
            }

            // Bindings
            for binding in &section.bindings {
                if y >= area.y + area.height {
                    break;
                }

                let key_span = Span::styled(
                    format!("{:>12}", binding.keys),
                    self.theme.help_key,
                );
                let desc_span = Span::styled(
                    format!(" {}", binding.description),
                    self.theme.help_desc,
                );
                let line = Line::from(vec![key_span, desc_span]);
                buf.set_line(area.x, y, &line, area.width);
                y += 1;
            }

            // Spacing between sections
            y += 1;
        }
    }
}

impl Widget for HelpOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Calculate centered popup area - larger to fit all content
        let popup_width = 80.min(area.width.saturating_sub(4));
        let popup_height = 32.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the popup area
        Clear.render(popup_area, buf);

        // Draw border
        let block = Block::default()
            .title(" Help - Press ? or Esc to close ")
            .title_style(self.theme.title)
            .borders(Borders::ALL)
            .border_style(self.theme.border);

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // Split into two columns
        let [left_col, right_col] = Layout::horizontal([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .areas(inner);

        // Get sections
        let sections = get_help_sections();
        let section_refs: Vec<&_> = sections.iter().collect();

        // Left column: Navigation, Selection & Clipboard, File Operations
        let left_sections: Vec<&_> = section_refs
            .iter()
            .filter(|s| {
                s.title == "Navigation"
                    || s.title == "Selection & Clipboard"
                    || s.title == "File Operations"
            })
            .copied()
            .collect();

        // Right column: Views & Display, Commands, then Command Palette reference
        let right_sections: Vec<&_> = section_refs
            .iter()
            .filter(|s| s.title == "Views & Display" || s.title == "Commands")
            .copied()
            .collect();

        // Render left column
        self.render_section_column(&left_sections, left_col, buf);

        // Render right column with sections + command palette
        let mut y = right_col.y;

        // First render the sections
        for section in &right_sections {
            if y >= right_col.y + right_col.height {
                break;
            }

            // Section title
            let title_line = Line::from(Span::styled(
                section.title,
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            ));
            buf.set_line(right_col.x, y, &title_line, right_col.width);
            y += 1;

            // Bindings
            for binding in &section.bindings {
                if y >= right_col.y + right_col.height {
                    break;
                }

                let key_span = Span::styled(
                    format!("{:>12}", binding.keys),
                    self.theme.help_key,
                );
                let desc_span = Span::styled(
                    format!(" {}", binding.description),
                    self.theme.help_desc,
                );
                let line = Line::from(vec![key_span, desc_span]);
                buf.set_line(right_col.x, y, &line, right_col.width);
                y += 1;
            }

            y += 1; // Spacing
        }

        // Add Command Palette section
        if y < right_col.y + right_col.height {
            let title_line = Line::from(Span::styled(
                "Command Palette (:)",
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            ));
            buf.set_line(right_col.x, y, &title_line, right_col.width);
            y += 1;

            let commands = get_command_help();
            for (cmd, desc) in commands {
                if y >= right_col.y + right_col.height {
                    break;
                }

                let cmd_span = Span::styled(
                    format!("{:>14}", cmd),
                    self.theme.help_key,
                );
                let desc_span = Span::styled(
                    format!(" {}", desc),
                    self.theme.help_desc,
                );
                let line = Line::from(vec![cmd_span, desc_span]);
                buf.set_line(right_col.x, y, &line, right_col.width);
                y += 1;
            }
        }
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
