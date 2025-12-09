//! Modal dialog widgets.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::theme::Theme;
use crate::ui::format_size;

use crate::app::state::DeletionProgress;

/// Confirmation dialog for deletion.
pub struct DeleteConfirmModal<'a> {
    theme: &'a Theme,
    marked_paths: &'a HashSet<PathBuf>,
    get_size: Box<dyn Fn(&PathBuf) -> Option<u64> + 'a>,
}

impl<'a> DeleteConfirmModal<'a> {
    /// Create a new delete confirmation modal.
    pub fn new<F>(theme: &'a Theme, marked_paths: &'a HashSet<PathBuf>, get_size: F) -> Self
    where
        F: Fn(&PathBuf) -> Option<u64> + 'a,
    {
        Self {
            theme,
            marked_paths,
            get_size: Box::new(get_size),
        }
    }
}

impl Widget for DeleteConfirmModal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Calculate popup area
        let popup_width = 60.min(area.width.saturating_sub(4));
        let popup_height =
            (self.marked_paths.len() as u16 + 8).min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the popup area
        Clear.render(popup_area, buf);

        // Draw border
        let block = Block::default()
            .title(" Confirm Deletion ")
            .title_style(
                Style::default()
                    .fg(self.theme.error)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.error));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // Calculate total size
        let total_size: u64 = self
            .marked_paths
            .iter()
            .filter_map(|p| (self.get_size)(p))
            .sum();

        // Build content
        let mut lines = vec![
            Line::styled(
                format!(
                    "Delete {} items ({})? This cannot be undone!",
                    self.marked_paths.len(),
                    format_size(total_size)
                ),
                Style::default()
                    .fg(self.theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
        ];

        // List items (limited)
        let max_items = (inner.height as usize).saturating_sub(5);
        for path in self.marked_paths.iter().take(max_items) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            let prefix = if path.is_dir() { "  " } else { "  " };
            lines.push(Line::raw(format!("  {}{}", prefix, name)));
        }

        if self.marked_paths.len() > max_items {
            lines.push(Line::styled(
                format!(
                    "  ... and {} more",
                    self.marked_paths.len() - max_items
                ),
                Style::default().fg(self.theme.muted),
            ));
        }

        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(" y/Enter ", self.theme.help_key),
            Span::raw("Confirm  "),
            Span::styled(" n/Esc ", self.theme.help_key),
            Span::raw("Cancel"),
        ]));

        Paragraph::new(lines).render(inner, buf);
    }
}

/// Progress dialog for deletion operation.
pub struct DeletionProgressModal<'a> {
    theme: &'a Theme,
    progress: Option<&'a DeletionProgress>,
}

impl<'a> DeletionProgressModal<'a> {
    /// Create a new deletion progress modal.
    pub fn new(theme: &'a Theme, progress: Option<&'a DeletionProgress>) -> Self {
        Self { theme, progress }
    }
}

impl Widget for DeletionProgressModal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Calculate popup area
        let popup_width = 50.min(area.width.saturating_sub(4));
        let popup_height = 10.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear the popup area
        Clear.render(popup_area, buf);

        // Draw border
        let block = Block::default()
            .title(" Deleting... ")
            .title_style(
                Style::default()
                    .fg(self.theme.warning)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.warning));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let mut lines = vec![];

        if let Some(progress) = self.progress {
            // Progress bar
            let pct = progress.percentage();
            let bar_width = (inner.width as usize).saturating_sub(10);
            let filled = (pct as usize * bar_width) / 100;
            let empty = bar_width.saturating_sub(filled);

            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::raw("  ["),
                Span::styled(
                    "\u{2588}".repeat(filled),
                    Style::default().fg(self.theme.info),
                ),
                Span::styled(
                    "\u{2591}".repeat(empty),
                    Style::default().fg(self.theme.muted),
                ),
                Span::raw(format!("] {}%", pct)),
            ]));

            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("  Progress: ", self.theme.help_desc),
                Span::raw(format!(
                    "{}/{} items",
                    progress.deleted + progress.failed,
                    progress.total
                )),
            ]));

            lines.push(Line::from(vec![
                Span::styled("  Freed:    ", self.theme.help_desc),
                Span::raw(format_size(progress.bytes_freed)),
            ]));

            if progress.failed > 0 {
                lines.push(Line::from(vec![
                    Span::styled("  Failed:   ", Style::default().fg(self.theme.error)),
                    Span::styled(
                        progress.failed.to_string(),
                        Style::default().fg(self.theme.error),
                    ),
                ]));
            }

            // Current item being deleted
            if let Some(current) = &progress.current {
                let name = current
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| current.display().to_string());
                let max_len = (inner.width as usize).saturating_sub(4);
                let display_name = if name.len() > max_len {
                    format!("...{}", &name[name.len().saturating_sub(max_len - 3)..])
                } else {
                    name
                };
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("  {}", display_name),
                    Style::default().fg(self.theme.muted),
                ));
            }
        } else {
            lines.push(Line::raw("  Preparing deletion..."));
        }

        Paragraph::new(lines).render(inner, buf);
    }
}

/// Command palette input widget.
pub struct CommandPalette<'a> {
    theme: &'a Theme,
    input: &'a str,
    cursor: usize,
}

impl<'a> CommandPalette<'a> {
    /// Create a new command palette widget.
    pub fn new(theme: &'a Theme, input: &'a str, cursor: usize) -> Self {
        Self {
            theme,
            input,
            cursor,
        }
    }
}

impl Widget for CommandPalette<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Clear the area first
        Clear.render(area, buf);

        // Build the command line display
        let prompt = ":";

        // Build the line with cursor indication
        let mut spans = vec![Span::styled(
            prompt,
            Style::default()
                .fg(self.theme.info)
                .add_modifier(Modifier::BOLD),
        )];

        if self.input.is_empty() {
            // Show placeholder
            spans.push(Span::styled(
                "type command (q, cd, help...)",
                Style::default().fg(self.theme.muted),
            ));
        } else {
            // Show input with cursor
            let cursor_pos = self.cursor.min(self.input.len());
            let (before, after) = self.input.split_at(cursor_pos);
            spans.push(Span::raw(before));

            if !after.is_empty() {
                // Show cursor under next char
                let (cursor_char, rest) = after.split_at(1);
                spans.push(Span::styled(
                    cursor_char,
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
                spans.push(Span::raw(rest));
            } else {
                // Cursor at end - show block cursor
                spans.push(Span::styled(
                    " ",
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            }
        }

        let line = Line::from(spans);
        Paragraph::new(line)
            .style(self.theme.footer)
            .render(area, buf);
    }
}
