//! Miller columns widget for file navigation.
//!
//! Provides a three-column layout: Parent | Current | Preview

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidget, Widget};

use gravityfile_core::{FileNode, NodeKind};

use crate::app::state::{ClipboardMode, ClipboardState};
use crate::theme::Theme;
use crate::ui::{format_size, SizeBar};

/// State for Miller columns view.
#[derive(Debug, Clone, Default)]
pub struct MillerState {
    /// Selected index in current column.
    pub selected: usize,
    /// Scroll offset for current column.
    pub offset: usize,
}

impl MillerState {
    /// Create new Miller state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Move selection up.
    pub fn move_up(&mut self, count: usize) {
        self.selected = self.selected.saturating_sub(count);
    }

    /// Move selection down.
    pub fn move_down(&mut self, count: usize, max: usize) {
        self.selected = (self.selected + count).min(max.saturating_sub(1));
    }

    /// Jump to top.
    pub fn jump_to_top(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self, max: usize) {
        self.selected = max.saturating_sub(1);
    }

    /// Ensure selected item is visible.
    pub fn ensure_visible(&mut self, viewport_height: usize) {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + viewport_height {
            self.offset = self.selected - viewport_height + 1;
        }
    }

    /// Reset selection when directory changes.
    pub fn reset(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }
}

/// A lightweight entry for display in columns.
#[derive(Debug, Clone)]
pub struct ColumnEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub is_executable: bool,
    pub is_symlink: bool,
    pub is_broken_symlink: bool,
}

impl ColumnEntry {
    /// Create from a FileNode.
    pub fn from_node(node: &FileNode) -> Self {
        let (is_executable, is_symlink, is_broken_symlink) = match &node.kind {
            NodeKind::File { executable } => (*executable, false, false),
            NodeKind::Symlink { broken, .. } => (false, true, *broken),
            _ => (false, false, false),
        };

        Self {
            name: node.name.to_string(),
            size: node.size,
            is_dir: node.is_dir(),
            is_executable,
            is_symlink,
            is_broken_symlink,
        }
    }
}

/// Miller columns widget.
pub struct MillerColumns<'a> {
    /// Current directory node.
    current: &'a FileNode,
    /// Parent directory node (if available).
    parent: Option<&'a FileNode>,
    /// Current directory name for highlighting in parent.
    current_name: &'a str,
    /// View root path for building full paths.
    view_root: &'a PathBuf,
    /// Marked items.
    marked: &'a HashSet<PathBuf>,
    /// Clipboard state for visual indicators.
    clipboard: &'a ClipboardState,
    /// Theme.
    theme: &'a Theme,
    /// Optional block around the whole widget.
    block: Option<Block<'a>>,
}

impl<'a> MillerColumns<'a> {
    /// Create a new Miller columns widget.
    pub fn new(
        current: &'a FileNode,
        parent: Option<&'a FileNode>,
        current_name: &'a str,
        view_root: &'a PathBuf,
        marked: &'a HashSet<PathBuf>,
        clipboard: &'a ClipboardState,
        theme: &'a Theme,
    ) -> Self {
        Self {
            current,
            parent,
            current_name,
            view_root,
            marked,
            clipboard,
            theme,
            block: None,
        }
    }

    /// Set the block for the widget.
    #[allow(dead_code)]
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Get entries for a directory node.
    fn get_entries(node: &FileNode) -> Vec<ColumnEntry> {
        node.children
            .iter()
            .map(|child| ColumnEntry::from_node(child))
            .collect()
    }

    /// Render a single column.
    fn render_column(
        &self,
        area: Rect,
        buf: &mut Buffer,
        entries: &[ColumnEntry],
        selected: Option<usize>,
        highlight_name: Option<&str>,
        offset: usize,
        title: Option<&str>,
        base_path: Option<&PathBuf>,
        total_size: u64,
    ) {
        // Draw border
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border);

        let block = if let Some(title) = title {
            block
                .title(format!(" {} ", title))
                .title_style(self.theme.title)
        } else {
            block
        };

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Size bar configuration
        let size_bar_width: u16 = 8;

        let viewport_height = inner.height as usize;
        let end = (offset + viewport_height).min(entries.len());

        for (row_idx, entry_idx) in (offset..end).enumerate() {
            let entry = &entries[entry_idx];
            let y = inner.y + row_idx as u16;

            let is_selected = selected == Some(entry_idx);
            let is_highlighted =
                highlight_name.map_or(false, |name| name == entry.name);

            // Check if this entry is marked
            let entry_path = base_path.map(|bp| bp.join(&entry.name));
            let is_marked = entry_path
                .as_ref()
                .map(|p| self.marked.contains(p))
                .unwrap_or(false);

            // Check if item is in clipboard
            let in_clipboard = entry_path
                .as_ref()
                .map(|p| self.clipboard.paths.contains(p))
                .unwrap_or(false);
            let clipboard_mode = if in_clipboard {
                self.clipboard.mode
            } else {
                ClipboardMode::Empty
            };

            // Selection/clipboard indicator
            // Priority: marked > cut > copied > none
            let (checkbox, checkbox_style) = if is_marked {
                ("● ", Style::default().fg(self.theme.info))
            } else if clipboard_mode == ClipboardMode::Cut {
                ("✂ ", Style::default().fg(self.theme.warning))
            } else if clipboard_mode == ClipboardMode::Copy {
                ("⎘ ", Style::default().fg(self.theme.success))
            } else {
                ("  ", Style::default().fg(self.theme.muted))
            };
            let checkbox_width: u16 = 2;

            // Icon and style based on type
            let (icon, base_style) = if entry.is_dir {
                ("📁 ", self.theme.directory)
            } else if entry.is_symlink {
                if entry.is_broken_symlink {
                    ("🔗 ", self.theme.symlink.add_modifier(Modifier::DIM))
                } else {
                    ("🔗 ", self.theme.symlink)
                }
            } else if entry.is_executable {
                ("⚙ ", self.theme.executable)
            } else {
                ("📄 ", self.theme.file)
            };

            // Apply clipboard styling (cut items are dimmed, copied items are italic)
            let base_style = if clipboard_mode == ClipboardMode::Cut {
                base_style.add_modifier(Modifier::DIM)
            } else if clipboard_mode == ClipboardMode::Copy {
                base_style.add_modifier(Modifier::ITALIC)
            } else {
                base_style
            };

            // Calculate available width for name (account for size bar)
            let icon_width = 2;
            let size_width = 7; // Reduced to make room for size bar
            let available_for_name = inner
                .width
                .saturating_sub(checkbox_width + icon_width + size_width + size_bar_width + 2)
                as usize;

            // Truncate name if needed
            let name = if entry.name.len() > available_for_name {
                let truncated_len = available_for_name.saturating_sub(1);
                format!("{}…", &entry.name[..truncated_len.min(entry.name.len())])
            } else {
                entry.name.clone()
            };

            // Pad name
            let name_padding =
                " ".repeat(available_for_name.saturating_sub(name.len()));

            // Size text (shorter format)
            let size_text = format!("{:>7}", format_size(entry.size));

            // Build line with checkbox
            let checkbox_span = Span::styled(checkbox, checkbox_style);
            let spans = vec![
                checkbox_span,
                Span::styled(icon, base_style),
                Span::styled(&name, base_style),
                Span::raw(&name_padding),
                Span::styled(&size_text, Style::default().fg(self.theme.muted)),
            ];

            let line = Line::from(spans);

            // Apply selection/highlight style
            let line = if is_selected {
                line.style(self.theme.selected)
            } else if is_highlighted {
                line.style(self.theme.hover)
            } else {
                line
            };

            // Render line (leaving space for size bar)
            let line_area = Rect::new(
                inner.x,
                y,
                inner.width.saturating_sub(size_bar_width + 1),
                1,
            );
            Widget::render(line, line_area, buf);

            // Render size bar
            let ratio = if total_size > 0 {
                entry.size as f64 / total_size as f64
            } else {
                0.0
            };

            let bar_area = Rect::new(
                inner.x + inner.width - size_bar_width,
                y,
                size_bar_width,
                1,
            );

            let bar = SizeBar::new(ratio)
                .filled_style(self.theme.size_bar_style(ratio))
                .empty_style(Style::default().fg(self.theme.muted));

            Widget::render(bar, bar_area, buf);
        }

        // Show empty message if no entries
        if entries.is_empty() {
            let msg = Line::styled("(empty)", Style::default().fg(self.theme.muted));
            let msg_area = Rect::new(inner.x, inner.y, inner.width, 1);
            Widget::render(msg, msg_area, buf);
        }
    }

    /// Render file preview (for non-directory items).
    fn render_file_preview(&self, area: Rect, buf: &mut Buffer, entry: &ColumnEntry) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(" Preview ")
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        let lines = vec![
            Line::styled(&entry.name, self.theme.title.add_modifier(Modifier::BOLD)),
            Line::raw(""),
            Line::from(vec![
                Span::styled("Size: ", self.theme.help_desc),
                Span::raw(format_size(entry.size)),
            ]),
            Line::from(vec![
                Span::styled("Type: ", self.theme.help_desc),
                Span::raw(if entry.is_dir {
                    "Directory"
                } else if entry.is_symlink {
                    "Symbolic Link"
                } else if entry.is_executable {
                    "Executable"
                } else {
                    "File"
                }),
            ]),
        ];

        Paragraph::new(lines).render(inner, buf);
    }
}

impl StatefulWidget for MillerColumns<'_> {
    type State = MillerState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Handle outer block
        let inner_area = if let Some(block) = &self.block {
            let inner = block.inner(area);
            block.clone().render(area, buf);
            inner
        } else {
            area
        };

        if inner_area.height == 0 || inner_area.width == 0 {
            return;
        }

        // Split into three columns: Parent (25%) | Current (35%) | Preview (40%)
        let [parent_area, current_area, preview_area] = Layout::horizontal([
            Constraint::Percentage(25),
            Constraint::Percentage(35),
            Constraint::Percentage(40),
        ])
        .areas(inner_area);

        // Get entries
        let current_entries = Self::get_entries(self.current);
        let parent_entries = self.parent.map(Self::get_entries).unwrap_or_default();

        // Ensure selection is valid
        if state.selected >= current_entries.len() && !current_entries.is_empty() {
            state.selected = current_entries.len() - 1;
        }

        // Ensure visibility
        let viewport_height = current_area.height.saturating_sub(2) as usize;
        state.ensure_visible(viewport_height);

        // Calculate base paths for marking support
        let parent_base_path = self.view_root.parent().map(|p| p.to_path_buf());

        // Calculate total sizes for each column (for size bar ratios)
        let parent_total_size = self.parent.map(|p| p.size).unwrap_or(0);
        let current_total_size = self.current.size;

        // Render parent column
        self.render_column(
            parent_area,
            buf,
            &parent_entries,
            None,
            Some(self.current_name),
            0,
            Some("Parent"),
            parent_base_path.as_ref(),
            parent_total_size,
        );

        // Render current column with selection
        self.render_column(
            current_area,
            buf,
            &current_entries,
            Some(state.selected),
            None,
            state.offset,
            Some("Current"),
            Some(self.view_root),
            current_total_size,
        );

        // Render preview column
        if let Some(selected_entry) = current_entries.get(state.selected) {
            if selected_entry.is_dir {
                // Find the child node and show its contents
                if let Some(child_node) = self
                    .current
                    .children
                    .iter()
                    .find(|c| c.name.as_str() == selected_entry.name)
                {
                    let preview_entries = Self::get_entries(child_node);
                    let preview_base_path = self.view_root.join(&selected_entry.name);
                    let preview_total_size = child_node.size;
                    self.render_column(
                        preview_area,
                        buf,
                        &preview_entries,
                        None,
                        None,
                        0,
                        Some("Preview"),
                        Some(&preview_base_path),
                        preview_total_size,
                    );
                } else {
                    self.render_file_preview(preview_area, buf, selected_entry);
                }
            } else {
                // Show file info preview
                self.render_file_preview(preview_area, buf, selected_entry);
            }
        } else {
            // Empty preview
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(self.theme.border)
                .title(" Preview ")
                .title_style(self.theme.title);
            block.render(preview_area, buf);
        }
    }
}

/// Get the selected entry from Miller state.
#[allow(dead_code)]
pub fn get_selected_entry<'a>(
    state: &MillerState,
    node: &'a FileNode,
) -> Option<&'a FileNode> {
    node.children.get(state.selected)
}
