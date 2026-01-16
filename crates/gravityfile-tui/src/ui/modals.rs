//! Modal dialog widgets.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use gravityfile_ops::{Conflict, ConflictKind, OperationProgress, OperationType};

use crate::app::input::InputState;
use crate::app::state::{DeletionProgress, SettingsState};
use crate::theme::Theme;
use crate::ui::format_size;

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
                    "Move {} items ({}) to trash?",
                    self.marked_paths.len(),
                    format_size(total_size)
                ),
                Style::default()
                    .fg(self.theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
        ];

        // List items (limited) - show full paths for clarity
        let max_items = (inner.height as usize).saturating_sub(5);
        let max_path_len = (inner.width as usize).saturating_sub(6);
        for path in self.marked_paths.iter().take(max_items) {
            // Show full path, truncated from the left if too long
            let full_path = path.display().to_string();
            let display_path = if full_path.len() > max_path_len {
                format!("...{}", &full_path[full_path.len().saturating_sub(max_path_len - 3)..])
            } else {
                full_path
            };

            // Add type indicator and style based on file type
            let (icon, style) = if path.is_symlink() {
                ("🔗", self.theme.symlink)
            } else if path.is_dir() {
                ("📁", self.theme.directory)
            } else {
                ("  ", Style::default())
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::raw(icon),
                Span::styled(format!(" {}", display_path), style),
            ]));
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

/// Progress dialog for copy/move operations.
pub struct OperationProgressModal<'a> {
    theme: &'a Theme,
    progress: &'a OperationProgress,
}

impl<'a> OperationProgressModal<'a> {
    /// Create a new operation progress modal.
    pub fn new(theme: &'a Theme, progress: &'a OperationProgress) -> Self {
        Self { theme, progress }
    }

    fn operation_title(&self) -> &'static str {
        match self.progress.operation_type {
            OperationType::Copy => " Copying... ",
            OperationType::Move => " Moving... ",
            OperationType::Delete => " Deleting... ",
            OperationType::Rename => " Renaming... ",
            OperationType::CreateFile => " Creating File... ",
            OperationType::CreateDirectory => " Creating Directory... ",
        }
    }

    fn operation_verb(&self) -> &'static str {
        match self.progress.operation_type {
            OperationType::Copy => "Copied",
            OperationType::Move => "Moved",
            OperationType::Delete => "Deleted",
            OperationType::Rename => "Renamed",
            OperationType::CreateFile => "Created",
            OperationType::CreateDirectory => "Created",
        }
    }
}

impl Widget for OperationProgressModal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup_width = 55.min(area.width.saturating_sub(4));
        let popup_height = 12.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        Clear.render(popup_area, buf);

        let block = Block::default()
            .title(self.operation_title())
            .title_style(
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.info));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let mut lines = vec![];

        // Calculate percentage
        let pct = if self.progress.bytes_total > 0 {
            ((self.progress.bytes_processed as f64 / self.progress.bytes_total as f64) * 100.0)
                as u8
        } else if self.progress.files_total > 0 {
            ((self.progress.files_completed as f64 / self.progress.files_total as f64) * 100.0)
                as u8
        } else {
            0
        };

        // Progress bar
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

        // Files progress
        lines.push(Line::from(vec![
            Span::styled("  Files:    ", self.theme.help_desc),
            Span::raw(format!(
                "{}/{} {}",
                self.progress.files_completed,
                self.progress.files_total,
                self.operation_verb().to_lowercase()
            )),
        ]));

        // Bytes progress
        lines.push(Line::from(vec![
            Span::styled("  Size:     ", self.theme.help_desc),
            Span::raw(format!(
                "{} / {}",
                format_size(self.progress.bytes_processed),
                format_size(self.progress.bytes_total)
            )),
        ]));

        // Error count
        if !self.progress.errors.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  Errors:   ", Style::default().fg(self.theme.error)),
                Span::styled(
                    self.progress.errors.len().to_string(),
                    Style::default().fg(self.theme.error),
                ),
            ]));
        }

        // Current file
        if let Some(current) = &self.progress.current_file {
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

        Paragraph::new(lines).render(inner, buf);
    }
}

/// Text input modal for rename/create operations.
pub struct InputModal<'a> {
    theme: &'a Theme,
    input: &'a InputState,
    title: &'a str,
    prompt: &'a str,
}

impl<'a> InputModal<'a> {
    /// Create a new input modal.
    pub fn new(theme: &'a Theme, input: &'a InputState, title: &'a str, prompt: &'a str) -> Self {
        Self {
            theme,
            input,
            title,
            prompt,
        }
    }
}

impl Widget for InputModal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup_width = 50.min(area.width.saturating_sub(4));
        let popup_height = if self.input.error().is_some() { 8 } else { 7 };
        let popup_height = popup_height.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        Clear.render(popup_area, buf);

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .title_style(
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(self.theme.border);

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let mut lines = vec![];

        // Prompt
        lines.push(Line::styled(self.prompt, self.theme.help_desc));
        lines.push(Line::raw(""));

        // Input field with cursor
        let buffer = self.input.buffer();
        let cursor = self.input.cursor();
        let max_visible = (inner.width as usize).saturating_sub(4);

        // Calculate visible portion
        let (visible_start, cursor_in_view) = if cursor > max_visible - 1 {
            (cursor - (max_visible - 1), max_visible - 1)
        } else {
            (0, cursor)
        };

        let visible_end = (visible_start + max_visible).min(buffer.len());
        let visible_text = if buffer.is_empty() {
            String::new()
        } else {
            buffer
                .chars()
                .skip(visible_start)
                .take(visible_end - visible_start)
                .collect()
        };

        let mut input_spans = vec![Span::raw("  ")];

        if visible_text.is_empty() {
            // Show cursor at empty position
            input_spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        } else {
            // Text before cursor
            let before: String = visible_text.chars().take(cursor_in_view).collect();
            if !before.is_empty() {
                input_spans.push(Span::raw(before));
            }

            // Cursor character
            let cursor_char: String = visible_text.chars().skip(cursor_in_view).take(1).collect();
            if cursor_char.is_empty() {
                input_spans.push(Span::styled(
                    " ",
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            } else {
                input_spans.push(Span::styled(
                    cursor_char,
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            }

            // Text after cursor
            let after: String = visible_text.chars().skip(cursor_in_view + 1).collect();
            if !after.is_empty() {
                input_spans.push(Span::raw(after));
            }
        }

        lines.push(Line::from(input_spans));

        // Error message if any
        if let Some(error) = self.input.error() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("  {}", error),
                Style::default().fg(self.theme.error),
            ));
        }

        // Help line
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(" Enter ", self.theme.help_key),
            Span::raw("Confirm  "),
            Span::styled(" Esc ", self.theme.help_key),
            Span::raw("Cancel"),
        ]));

        Paragraph::new(lines).render(inner, buf);
    }
}

/// Conflict resolution modal.
pub struct ConflictModal<'a> {
    theme: &'a Theme,
    conflict: &'a Conflict,
}

impl<'a> ConflictModal<'a> {
    /// Create a new conflict modal.
    pub fn new(theme: &'a Theme, conflict: &'a Conflict) -> Self {
        Self { theme, conflict }
    }
}

impl Widget for ConflictModal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup_width = 60.min(area.width.saturating_sub(4));
        let popup_height = 12.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        Clear.render(popup_area, buf);

        let (title, title_color) = match self.conflict.kind {
            ConflictKind::FileExists => (" File Exists ", self.theme.warning),
            ConflictKind::DirectoryExists => (" Directory Exists ", self.theme.warning),
            ConflictKind::SourceIsAncestor => (" Invalid Operation ", self.theme.error),
            ConflictKind::PermissionDenied => (" Permission Denied ", self.theme.error),
            ConflictKind::SameFile => (" Same File ", self.theme.info),
        };

        let block = Block::default()
            .title(title)
            .title_style(Style::default().fg(title_color).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(title_color));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let mut lines = vec![];

        // Conflict description
        let description = match self.conflict.kind {
            ConflictKind::FileExists => "A file with this name already exists:",
            ConflictKind::DirectoryExists => "A directory with this name already exists:",
            ConflictKind::SourceIsAncestor => "Cannot copy/move a directory into itself:",
            ConflictKind::PermissionDenied => "Permission denied for:",
            ConflictKind::SameFile => "Source and destination are the same file:",
        };

        lines.push(Line::styled(description, self.theme.help_desc));
        lines.push(Line::raw(""));

        // Source path
        let source_name = self
            .conflict
            .source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.conflict.source.display().to_string());
        let max_len = (inner.width as usize).saturating_sub(4);
        let display_name = if source_name.len() > max_len {
            format!(
                "...{}",
                &source_name[source_name.len().saturating_sub(max_len - 3)..]
            )
        } else {
            source_name
        };
        lines.push(Line::styled(
            format!("  {}", display_name),
            Style::default().fg(self.theme.info),
        ));

        // Destination path
        let dest = &self.conflict.destination;
        let dest_name = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dest.display().to_string());
        let dest_display = if dest_name.len() > max_len {
            format!(
                "...{}",
                &dest_name[dest_name.len().saturating_sub(max_len - 3)..]
            )
        } else {
            dest_name
        };
        lines.push(Line::styled(
            format!("  → {}", dest_display),
            Style::default().fg(self.theme.muted),
        ));

        lines.push(Line::raw(""));
        lines.push(Line::raw(""));

        // Action options based on conflict type
        match self.conflict.kind {
            ConflictKind::FileExists | ConflictKind::DirectoryExists => {
                lines.push(Line::from(vec![
                    Span::styled(" s ", self.theme.help_key),
                    Span::raw("Skip  "),
                    Span::styled(" o ", self.theme.help_key),
                    Span::raw("Overwrite  "),
                    Span::styled(" r ", self.theme.help_key),
                    Span::raw("Rename"),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(" S ", self.theme.help_key),
                    Span::raw("Skip All  "),
                    Span::styled(" O ", self.theme.help_key),
                    Span::raw("Overwrite All  "),
                    Span::styled(" Esc ", self.theme.help_key),
                    Span::raw("Abort"),
                ]));
            }
            ConflictKind::PermissionDenied => {
                lines.push(Line::from(vec![
                    Span::styled(" s ", self.theme.help_key),
                    Span::raw("Skip  "),
                    Span::styled(" S ", self.theme.help_key),
                    Span::raw("Skip All  "),
                    Span::styled(" Esc ", self.theme.help_key),
                    Span::raw("Abort"),
                ]));
            }
            ConflictKind::SourceIsAncestor => {
                lines.push(Line::from(vec![
                    Span::styled(" Enter/Esc ", self.theme.help_key),
                    Span::raw("Acknowledge"),
                ]));
            }
            ConflictKind::SameFile => {
                // Can only skip (do nothing) or create renamed copy
                lines.push(Line::styled(
                    "Cannot copy a file onto itself.",
                    Style::default().fg(self.theme.muted),
                ));
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![
                    Span::styled(" Enter/s ", self.theme.help_key),
                    Span::raw("Skip  "),
                    Span::styled(" r ", self.theme.help_key),
                    Span::raw("Create Renamed Copy  "),
                    Span::styled(" Esc ", self.theme.help_key),
                    Span::raw("Cancel"),
                ]));
            }
        }

        Paragraph::new(lines).render(inner, buf);
    }
}

/// Settings modal for user preferences.
pub struct SettingsModal<'a> {
    theme: &'a Theme,
    state: &'a SettingsState,
}

impl<'a> SettingsModal<'a> {
    /// Create a new settings modal.
    pub fn new(theme: &'a Theme, state: &'a SettingsState) -> Self {
        Self { theme, state }
    }

    fn render_toggle(&self, label: &str, value: bool, selected: bool) -> Line<'static> {
        let checkbox = if value { "[x]" } else { "[ ]" };
        let style = if selected {
            Style::default()
                .fg(self.theme.info)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                style,
            ),
            Span::styled(checkbox.to_string(), style),
            Span::styled(format!(" {}", label), style),
        ])
    }

    fn render_choice(&self, label: &str, value: &str, selected: bool) -> Line<'static> {
        let style = if selected {
            Style::default()
                .fg(self.theme.info)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                style,
            ),
            Span::styled(format!("{}: ", label), style),
            Span::styled(
                value.to_string(),
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
            ),
        ])
    }
}

impl Widget for SettingsModal<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup_width = 50.min(area.width.saturating_sub(4));
        let popup_height = 14.min(area.height.saturating_sub(4));

        let popup_x = (area.width.saturating_sub(popup_width)) / 2 + area.x;
        let popup_y = (area.height.saturating_sub(popup_height)) / 2 + area.y;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        Clear.render(popup_area, buf);

        let title = if self.state.dirty {
            " Settings * "
        } else {
            " Settings "
        };

        let block = Block::default()
            .title(title)
            .title_style(
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(self.theme.border);

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let mut lines = vec![];

        // Section header
        lines.push(Line::styled(
            " Startup",
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
        lines.push(Line::raw(""));

        // Scan on startup
        lines.push(self.render_toggle(
            "Scan on startup",
            self.state.settings.scan_on_startup,
            self.state.selected == 0,
        ));

        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " Display",
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
        lines.push(Line::raw(""));

        // Show hidden files
        lines.push(self.render_toggle(
            "Show hidden files",
            self.state.settings.show_hidden,
            self.state.selected == 1,
        ));

        // Default layout
        lines.push(self.render_choice(
            "Default layout",
            &self.state.settings.default_layout,
            self.state.selected == 2,
        ));

        lines.push(Line::raw(""));
        lines.push(Line::raw(""));

        // Help line
        lines.push(Line::from(vec![
            Span::styled(" j/k ", self.theme.help_key_style()),
            Span::styled("Nav ", self.theme.help_desc_style()),
            Span::styled(" Space/Enter ", self.theme.help_key_style()),
            Span::styled("Toggle ", self.theme.help_desc_style()),
            Span::styled(" s ", self.theme.help_key_style()),
            Span::styled("Save ", self.theme.help_desc_style()),
            Span::styled(" Esc ", self.theme.help_key_style()),
            Span::styled("Close", self.theme.help_desc_style()),
        ]));

        Paragraph::new(lines).render(inner, buf);
    }
}
