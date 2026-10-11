//! Miller columns widget for file navigation.
//!
//! Provides a three-column layout: Parent | Current | Preview

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidget, Widget};

use gravityfile_core::{FileNode, GitStatus, NodeKind};

use crate::app::state::{ClipboardMode, ClipboardState};
use ratatui_image::Image as RatatuiImage;

use crate::preview::PreviewContent;
use crate::theme::Theme;
use crate::ui::{CompactSizeBar, fit_to_width, format_size};

/// State for Miller columns view.
#[derive(Debug, Clone, Default)]
pub struct MillerState {
    /// Selected index in current column.
    pub selected: usize,
    /// Scroll offset for current column.
    pub offset: usize,
}

impl MillerState {
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
}

/// A lightweight entry for display in columns.
#[derive(Debug, Clone)]
pub struct ColumnEntry {
    pub name: String,
    pub native_name: std::ffi::OsString,
    pub size: u64,
    pub is_dir: bool,
    pub is_executable: bool,
    pub is_symlink: bool,
    pub is_broken_symlink: bool,
    pub git_status: Option<GitStatus>,
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
            name: node.display_name().into_owned(),
            native_name: node.file_name().to_os_string(),
            size: node.size,
            is_dir: node.is_dir(),
            is_executable,
            is_symlink,
            is_broken_symlink,
            git_status: node.git_status,
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
    /// File preview content (for non-directory items).
    file_preview: Option<&'a PreviewContent>,
    /// Current preview mode.
    preview_mode: crate::preview::PreviewMode,
    /// Rows of the file preview scrolled past.
    preview_scroll: usize,
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
            file_preview: None,
            preview_mode: crate::preview::PreviewMode::Auto,
            preview_scroll: 0,
        }
    }

    /// Set the file preview content, scrolled down `scroll` rows.
    pub fn file_preview(mut self, preview: &'a PreviewContent, scroll: usize) -> Self {
        self.file_preview = Some(preview);
        self.preview_scroll = scroll;
        self
    }

    /// Set the preview mode.
    pub fn preview_mode(mut self, mode: crate::preview::PreviewMode) -> Self {
        self.preview_mode = mode;
        self
    }

    /// Render a single column.
    #[allow(clippy::too_many_arguments)]
    fn render_column(
        &self,
        area: Rect,
        buf: &mut Buffer,
        children: &[FileNode],
        selected: Option<usize>,
        highlight_name: Option<&std::ffi::OsStr>,
        offset: usize,
        title: Option<&str>,
        base_path: Option<&Path>,
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

        // Size bar configuration. A column narrower than the bar cannot show
        // it, and in that case the row text gets the space back instead of
        // being squeezed into nothing.
        let size_bar_width: u16 = if inner.width >= 8 { 8 } else { 0 };

        let viewport_height = inner.height as usize;
        let end = (offset + viewport_height).min(children.len());

        // Only the rows on screen are converted for display.
        for (row_idx, entry_idx) in (offset..end).enumerate() {
            let entry = &ColumnEntry::from_node(&children[entry_idx]);
            let y = inner.y + row_idx as u16;

            let is_selected = selected == Some(entry_idx);
            let is_highlighted = highlight_name.is_some_and(|name| name == entry.native_name);

            // Check if this entry is marked
            let entry_path = base_path.map(|bp| bp.join(&entry.native_name));
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

            // Icon and style based on type (ASCII/box-drawing to match tree view)
            let (icon, base_style) = if entry.is_dir {
                (" ", self.theme.directory)
            } else if entry.is_symlink {
                if entry.is_broken_symlink {
                    (" ", self.theme.symlink.add_modifier(Modifier::DIM))
                } else {
                    (" ", self.theme.symlink)
                }
            } else if entry.is_executable {
                ("* ", self.theme.executable)
            } else {
                ("  ", self.theme.file)
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

            // Git status indicator
            let (git_indicator, git_style) = match entry.git_status {
                Some(status) if status.is_displayable() => {
                    let color = self.theme.git_status_color(status);
                    (
                        format!(" {}", status.indicator()),
                        Style::default().fg(color),
                    )
                }
                _ => (String::new(), Style::default()),
            };
            let git_len = git_indicator.len();

            // Truncate name if needed (account for git indicator, Unicode-safe)
            let name_max_len = available_for_name.saturating_sub(git_len);
            let name = fit_to_width(&entry.name, name_max_len);

            // Pad name (account for git indicator, using display widths)
            let name_display_width = {
                use unicode_width::UnicodeWidthStr;
                name.width()
            };
            let name_padding = " ".repeat(
                available_for_name
                    .saturating_sub(name_display_width)
                    .saturating_sub(git_len),
            );

            // Size text (shorter format) - show "..." for directories with unknown size
            let size_text = if entry.is_dir && entry.size == 0 {
                format!("{:>7}", "...")
            } else {
                format!("{:>7}", format_size(entry.size))
            };

            // Build line with checkbox
            let checkbox_span = Span::styled(checkbox, checkbox_style);
            let git_span = Span::styled(&git_indicator, git_style);
            let spans = vec![
                checkbox_span,
                Span::styled(icon, base_style),
                Span::styled(&name, base_style),
                git_span,
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
                inner
                    .width
                    .saturating_sub(size_bar_width)
                    .saturating_sub(u16::from(size_bar_width > 0)),
                1,
            );
            Widget::render(line, line_area, buf);

            // Render the size bar hugging the right edge, when there is one.
            if size_bar_width > 0 {
                let ratio = if total_size > 0 {
                    entry.size as f64 / total_size as f64
                } else {
                    0.0
                };

                let bar_area =
                    Rect::new(inner.x + inner.width - size_bar_width, y, size_bar_width, 1);

                let bar = CompactSizeBar::new(ratio).style(self.theme.size_bar_style(ratio));
                Widget::render(bar, bar_area, buf);
            }
        }

        // Show empty message if no entries
        if children.is_empty() {
            let msg = Line::styled("(empty)", Style::default().fg(self.theme.muted));
            let msg_area = Rect::new(inner.x, inner.y, inner.width, 1);
            Widget::render(msg, msg_area, buf);
        }
    }

    /// Render file preview (for non-directory items).
    fn render_file_preview(&self, area: Rect, buf: &mut Buffer, entry: &ColumnEntry) {
        let title = format!(" Preview [{}] (P) ", self.preview_mode.name());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        // If we have preview content, use it
        if let Some(preview) = self.file_preview {
            match preview {
                PreviewContent::Text {
                    lines,
                    highlighted,
                    total_lines,
                } => {
                    // Add header with file name and highlighting indicator
                    let header = Line::from(vec![
                        Span::styled(
                            gravityfile_core::escape_terminal_text(&entry.name),
                            self.theme.title.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            format!(
                                "[{total_lines} lines, {}]",
                                if *highlighted { "highlighted" } else { "plain" }
                            ),
                            Style::default().fg(self.theme.muted),
                        ),
                    ]);

                    let mut display_lines = vec![header, Line::raw("")];
                    let max_lines = inner.height.saturating_sub(2) as usize;
                    for line in lines.iter().skip(self.preview_scroll).take(max_lines) {
                        display_lines.push(line.clone());
                    }

                    Paragraph::new(display_lines).render(inner, buf);
                }
                PreviewContent::Hex { lines, total_bytes } => {
                    let header = Line::from(vec![
                        Span::styled(
                            gravityfile_core::escape_terminal_text(&entry.name),
                            self.theme.title.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            format!("[binary: {} bytes]", total_bytes),
                            Style::default().fg(self.theme.warning),
                        ),
                    ]);

                    let mut display_lines = vec![header, Line::raw("")];
                    let max_lines = inner.height.saturating_sub(2) as usize;
                    for line in lines.iter().skip(self.preview_scroll).take(max_lines) {
                        display_lines.push(line.clone());
                    }

                    Paragraph::new(display_lines).render(inner, buf);
                }
                PreviewContent::Directory {
                    entries,
                    total_entries,
                } => {
                    let header = Line::from(vec![
                        Span::styled(
                            gravityfile_core::escape_terminal_text(&entry.name),
                            self.theme.title.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            format!("[{} items; {} listed]", total_entries, entries.len()),
                            Style::default().fg(self.theme.muted),
                        ),
                    ]);

                    let mut display_lines = vec![header, Line::raw("")];
                    let max_lines = inner.height.saturating_sub(2) as usize;
                    for (name, is_dir) in entries.iter().skip(self.preview_scroll).take(max_lines) {
                        let icon = if *is_dir { " " } else { "  " };
                        let style = if *is_dir {
                            self.theme.directory
                        } else {
                            self.theme.file
                        };
                        display_lines.push(Line::styled(
                            format!("{}{}", icon, gravityfile_core::escape_terminal_text(name)),
                            style,
                        ));
                    }

                    Paragraph::new(display_lines).render(inner, buf);
                }
                PreviewContent::Metadata {
                    size,
                    modified,
                    created,
                    accessed,
                    file_type,
                    permissions,
                } => {
                    let mut lines = vec![
                        Line::styled(
                            gravityfile_core::escape_terminal_text(&entry.name),
                            self.theme.title.add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(""),
                        Line::from(vec![
                            Span::styled("Type: ", self.theme.help_desc),
                            Span::raw(file_type.clone()),
                        ]),
                        Line::from(vec![
                            Span::styled("Size: ", self.theme.help_desc),
                            Span::raw(crate::ui::format_size(*size)),
                            Span::styled(
                                format!(" ({} bytes)", size),
                                Style::default().fg(self.theme.muted),
                            ),
                        ]),
                    ];

                    if let Some(perms) = permissions {
                        lines.push(Line::from(vec![
                            Span::styled("Permissions: ", self.theme.help_desc),
                            Span::raw(perms.clone()),
                        ]));
                    }

                    if let Some(mtime) = modified
                        && let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH)
                    {
                        let secs = duration.as_secs();
                        lines.push(Line::from(vec![
                            Span::styled("Modified: ", self.theme.help_desc),
                            Span::raw(format_timestamp(secs)),
                        ]));
                    }

                    if let Some(ctime) = created
                        && let Ok(duration) = ctime.duration_since(std::time::UNIX_EPOCH)
                    {
                        let secs = duration.as_secs();
                        lines.push(Line::from(vec![
                            Span::styled("Created: ", self.theme.help_desc),
                            Span::raw(format_timestamp(secs)),
                        ]));
                    }

                    if let Some(atime) = accessed
                        && let Ok(duration) = atime.duration_since(std::time::UNIX_EPOCH)
                    {
                        let secs = duration.as_secs();
                        lines.push(Line::from(vec![
                            Span::styled("Accessed: ", self.theme.help_desc),
                            Span::raw(format_timestamp(secs)),
                        ]));
                    }

                    Paragraph::new(lines).render(inner, buf);
                }
                PreviewContent::Archive {
                    format,
                    entry_count,
                    total_size,
                    entries,
                } => {
                    let header = Line::from(vec![
                        Span::styled(
                            gravityfile_core::escape_terminal_text(&entry.name),
                            self.theme.title.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            format!("[{} archive]", format),
                            Style::default().fg(self.theme.warning),
                        ),
                    ]);

                    let summary = Line::from(vec![Span::styled(
                        format!(
                            "{} entries, {} uncompressed",
                            entry_count,
                            crate::ui::format_size(*total_size)
                        ),
                        Style::default().fg(self.theme.muted),
                    )]);

                    let mut display_lines = vec![header, summary, Line::raw("")];
                    let max_lines = inner.height.saturating_sub(3) as usize;

                    for archive_entry in entries.iter().skip(self.preview_scroll).take(max_lines) {
                        // Distinguish symlinks from directories with a
                        // distinct icon so the user can tell them apart.
                        let icon = if archive_entry.is_symlink {
                            "@ "
                        } else if archive_entry.is_dir {
                            " "
                        } else {
                            "  "
                        };

                        let size_str = if archive_entry.is_dir || archive_entry.is_symlink {
                            String::new()
                        } else {
                            format!(" ({})", crate::ui::format_size(archive_entry.size))
                        };

                        let ratio_str = archive_entry
                            .compression_ratio
                            .map(|r| format!(" [{:.0}%]", r * 100.0))
                            .unwrap_or_default();

                        // Show symlink target if available
                        let target_str = archive_entry
                            .link_target
                            .as_ref()
                            .map(|t| format!(" -> {}", gravityfile_core::escape_terminal_text(t)))
                            .unwrap_or_default();

                        let style = if archive_entry.is_symlink {
                            self.theme.symlink
                        } else if archive_entry.is_dir {
                            self.theme.directory
                        } else {
                            self.theme.file
                        };

                        display_lines.push(Line::from(vec![
                            Span::styled(
                                format!(
                                    "{}{}",
                                    icon,
                                    gravityfile_core::escape_terminal_text(&archive_entry.path)
                                ),
                                style,
                            ),
                            Span::styled(target_str, self.theme.symlink),
                            Span::styled(size_str, Style::default().fg(self.theme.muted)),
                            Span::styled(ratio_str, Style::default().fg(self.theme.success)),
                        ]));
                    }

                    let below = entry_count.saturating_sub(self.preview_scroll + max_lines);
                    if below > 0 {
                        display_lines.push(Line::styled(
                            format!("... and {below} more entries"),
                            Style::default().fg(self.theme.muted),
                        ));
                    }

                    Paragraph::new(display_lines).render(inner, buf);
                }
                PreviewContent::Image {
                    protocol,
                    width,
                    height,
                    format,
                } => {
                    // One header line, then the image fills the remainder. The
                    // protocol was already encoded off-thread, so rendering is
                    // just a blit.
                    let header = Line::from(vec![
                        Span::styled(
                            gravityfile_core::escape_terminal_text(&entry.name),
                            self.theme.title.add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            format!("{format} {width}×{height}"),
                            Style::default().fg(self.theme.muted),
                        ),
                    ]);
                    let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
                    Widget::render(Paragraph::new(header), header_area, buf);

                    if inner.height > 1 {
                        let image_area = Rect::new(
                            inner.x,
                            inner.y + 1,
                            inner.width,
                            inner.height.saturating_sub(1),
                        );
                        RatatuiImage::new(&protocol.0).render(image_area, buf);
                    }
                }
                PreviewContent::Error(msg) => {
                    let lines = vec![
                        Line::styled(
                            gravityfile_core::escape_terminal_text(&entry.name),
                            self.theme.title.add_modifier(Modifier::BOLD),
                        ),
                        Line::raw(""),
                        Line::styled(
                            gravityfile_core::escape_terminal_text(msg),
                            Style::default().fg(self.theme.error),
                        ),
                    ];
                    Paragraph::new(lines).render(inner, buf);
                }
                PreviewContent::Loading => {
                    Paragraph::new(Line::styled(
                        "Loading preview…",
                        Style::default().fg(self.theme.muted),
                    ))
                    .render(inner, buf);
                }
                PreviewContent::Empty => {
                    self.render_basic_info(inner, buf, entry);
                }
            }
        } else {
            // Fall back to basic file info
            self.render_basic_info(inner, buf, entry);
        }
    }

    /// Render basic file info (no content preview).
    fn render_basic_info(&self, area: Rect, buf: &mut Buffer, entry: &ColumnEntry) {
        // Show "calculating..." for directories with unknown size
        let size_str = if entry.is_dir && entry.size == 0 {
            "calculating...".to_string()
        } else {
            format_size(entry.size)
        };

        let lines = vec![
            Line::styled(
                gravityfile_core::escape_terminal_text(&entry.name),
                self.theme.title.add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::from(vec![
                Span::styled("Size: ", self.theme.help_desc),
                Span::raw(size_str),
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

        Paragraph::new(lines).render(area, buf);
    }
}

impl StatefulWidget for MillerColumns<'_> {
    type State = MillerState;

    fn render(self, inner_area: Rect, buf: &mut Buffer, state: &mut Self::State) {
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

        let current_children = &self.current.children;
        let parent_children = self.parent.map_or(&[][..], |p| &p.children);

        // Ensure selection is valid
        if state.selected >= current_children.len() && !current_children.is_empty() {
            state.selected = current_children.len() - 1;
        }

        // Ensure visibility
        let viewport_height = current_area.height.saturating_sub(2) as usize;
        state.ensure_visible(viewport_height);

        // Calculate base paths for marking support
        let parent_base_path = self.view_root.parent().map(|p| p.to_path_buf());

        // Calculate total sizes for each column (for size bar ratios)
        let parent_total_size = self.parent.map(|p| p.size).unwrap_or(0);
        let current_total_size = self.current.size;

        // Render parent column, scrolled so the current directory is on screen
        let current_name = self
            .view_root
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new(self.current_name));
        let parent_viewport = parent_area.height.saturating_sub(2) as usize;
        let parent_offset = parent_children
            .iter()
            .position(|child| child.file_name() == current_name)
            .map_or(0, |index| {
                index
                    .saturating_sub(parent_viewport / 2)
                    .min(parent_children.len().saturating_sub(parent_viewport))
            });
        self.render_column(
            parent_area,
            buf,
            parent_children,
            None,
            Some(current_name),
            parent_offset,
            Some("Parent"),
            parent_base_path.as_deref(),
            parent_total_size,
        );

        // Render current column with selection
        self.render_column(
            current_area,
            buf,
            current_children,
            Some(state.selected),
            None,
            state.offset,
            Some("Current"),
            Some(self.view_root),
            current_total_size,
        );

        // Render preview column
        if let Some(child_node) = current_children.get(state.selected) {
            if child_node.is_dir() {
                let preview_base_path = self.view_root.join(child_node.file_name());
                self.render_column(
                    preview_area,
                    buf,
                    &child_node.children,
                    None,
                    None,
                    0,
                    Some("Preview"),
                    Some(&preview_base_path),
                    child_node.size,
                );
            } else {
                self.render_file_preview(preview_area, buf, &ColumnEntry::from_node(child_node));
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

/// Format a Unix timestamp as a human-readable local date-time string.
fn format_timestamp(secs: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(secs as i64, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::PreviewContent;
    use crate::test_support::{sample_node_tree, sample_root_path};
    use ratatui::buffer::Buffer;

    #[test]
    fn preview_names_targets_and_errors_escape_directional_controls() {
        use crate::preview::ArchiveEntry;
        let name = "a\u{202e}b";
        let previews = [
            PreviewContent::Directory {
                total_entries: 1,
                entries: vec![(name.into(), false)],
            },
            PreviewContent::Error(name.into()),
            PreviewContent::Archive {
                format: "TAR".into(),
                entry_count: 1,
                total_size: 0,
                entries: vec![ArchiveEntry {
                    path: name.into(),
                    size: 0,
                    is_dir: false,
                    is_symlink: true,
                    link_target: Some("c\u{2067}d".into()),
                    compression_ratio: None,
                }],
            },
        ];
        for preview in &previews {
            let root = sample_node_tree();
            let mut state = MillerState {
                selected: root
                    .children
                    .iter()
                    .position(|child| !child.is_dir())
                    .unwrap(),
                ..MillerState::default()
            };
            let buffer = render_at(Rect::new(0, 0, 240, 20), &mut state, Some(preview));
            let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(rendered.contains("a\\u{202e}b"));
            assert!(!rendered.contains('\u{202e}'));
            assert!(!rendered.contains('\u{2067}'));
            if matches!(preview, PreviewContent::Archive { .. }) {
                assert!(rendered.contains("c\\u{2067}d"));
            }
        }
    }

    #[test]
    fn renders_filename_controls_visibly_without_changing_entry_identity() {
        use crate::test_support::{directory, file};
        let name = "a\u{202e}txt";
        let root = directory("root", vec![file(name, 1)]);
        let root_path = sample_root_path();
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();
        let area = Rect::new(0, 0, 160, 20);
        let mut buffer = Buffer::empty(area);
        let mut state = MillerState::default();
        StatefulWidget::render(
            MillerColumns::new(&root, None, name, &root_path, &marked, &clipboard, &theme),
            area,
            &mut buffer,
            &mut state,
        );
        let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("a\\u{202e}txt"));
        assert!(!rendered.contains('\u{202e}'));
        assert_eq!(ColumnEntry::from_node(&root.children[0]).native_name, name);
        assert_eq!(
            ColumnEntry::from_node(&root.children[0]).name,
            r"a\u{202e}txt"
        );
    }

    /// Render the widget over `area` into an 120x40 buffer, so a rect that
    /// escapes its area is caught by the buffer bounds rather than silently
    /// clipped.
    fn render_at(area: Rect, state: &mut MillerState, preview: Option<&PreviewContent>) -> Buffer {
        let root = sample_node_tree();
        let parent = sample_node_tree();
        let root_path = sample_root_path();
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();

        let canvas = Rect::new(0, 0, area.right().max(120), area.bottom().max(40));
        let mut buf = Buffer::empty(canvas);

        let mut miller = MillerColumns::new(
            &root,
            Some(&parent),
            "src",
            &root_path,
            &marked,
            &clipboard,
            &theme,
        );
        if let Some(preview) = preview {
            miller = miller.file_preview(preview, 0);
        }
        StatefulWidget::render(miller, area, &mut buf, state);
        buf
    }

    #[test]
    fn navigation_stays_within_bounds() {
        let mut state = MillerState::default();
        state.move_up(10);
        assert_eq!(state.selected, 0);
        state.move_down(100, 7);
        assert_eq!(state.selected, 6);
        state.jump_to_top();
        assert_eq!((state.selected, state.offset), (0, 0));
        state.jump_to_bottom(7);
        assert_eq!(state.selected, 6);

        // An empty directory must not underflow.
        state.jump_to_bottom(0);
        assert_eq!(state.selected, 0);
        state.move_down(5, 0);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn ensure_visible_scrolls_the_minimum_distance() {
        let mut state = MillerState {
            selected: 20,
            offset: 0,
        };
        state.ensure_visible(8);
        assert_eq!(state.offset, 13);

        state.selected = 13;
        state.ensure_visible(8);
        assert_eq!(state.offset, 13);

        state.selected = 2;
        state.ensure_visible(8);
        assert_eq!(state.offset, 2);
    }

    #[test]
    fn column_entries_carry_display_metadata_from_the_node() {
        let root = sample_node_tree();
        let entries: Vec<ColumnEntry> = root.children.iter().map(ColumnEntry::from_node).collect();

        let run = entries.iter().find(|e| e.name == "run.sh").unwrap();
        assert!(run.is_executable && !run.is_dir && !run.is_symlink);

        let link = entries.iter().find(|e| e.name == "link").unwrap();
        assert!(link.is_symlink && !link.is_broken_symlink);

        let dangling = entries.iter().find(|e| e.name == "dangling").unwrap();
        assert!(dangling.is_symlink && dangling.is_broken_symlink);

        let src = entries.iter().find(|e| e.name == "src").unwrap();
        assert!(src.is_dir && !src.is_executable);
    }

    #[test]
    fn renders_at_every_size_without_panicking() {
        // The three columns are percentage splits, so a narrow terminal can
        // produce a column narrower than the right-anchored size bar. That used
        // to compute an out-of-bounds rect and abort the app.
        for width in 1u16..=120 {
            for height in [1u16, 2, 3, 10, 40] {
                let mut state = MillerState::default();
                let _ = render_at(Rect::new(0, 0, width, height), &mut state, None);
            }
        }
    }

    #[test]
    fn renders_every_preview_kind_without_panicking() {
        use crate::preview::{ArchiveEntry, PreviewContent};
        use ratatui::text::Line;

        let previews = vec![
            PreviewContent::Empty,
            PreviewContent::Loading,
            PreviewContent::Error("boom".into()),
            PreviewContent::Text {
                lines: vec![Line::raw("hello"); 200],
                highlighted: true,
                total_lines: 200,
            },
            PreviewContent::Hex {
                lines: vec![Line::raw("00000000  ff"); 200],
                total_bytes: 4096,
            },
            PreviewContent::Directory {
                total_entries: 200,
                entries: vec![("child".to_string(), true); 200],
            },
            PreviewContent::Metadata {
                size: 1234,
                modified: Some(std::time::SystemTime::UNIX_EPOCH),
                created: Some(std::time::SystemTime::UNIX_EPOCH),
                accessed: Some(std::time::SystemTime::UNIX_EPOCH),
                file_type: "File".into(),
                permissions: Some("rw-r--r--".into()),
            },
            PreviewContent::Archive {
                format: "ZIP".into(),
                entry_count: 300,
                total_size: 9999,
                entries: vec![
                    ArchiveEntry {
                        path: "inner/file".into(),
                        size: 10,
                        is_dir: false,
                        is_symlink: false,
                        link_target: None,
                        compression_ratio: Some(0.5),
                    };
                    300
                ],
            },
        ];

        for preview in &previews {
            for (width, height) in [(1u16, 1u16), (12, 4), (40, 6), (80, 24), (200, 60)] {
                // Select a file, so the third column renders the preview
                // rather than a directory listing.
                let root = sample_node_tree();
                let file_index = root
                    .children
                    .iter()
                    .position(|child| !child.is_dir())
                    .expect("fixture has a file");
                let mut state = MillerState {
                    selected: file_index,
                    offset: 0,
                };
                let _ = render_at(Rect::new(0, 0, width, height), &mut state, Some(preview));
            }
        }
    }

    #[test]
    fn an_out_of_range_selection_is_clamped_during_render() {
        let mut state = MillerState {
            selected: 999,
            offset: 0,
        };
        let _ = render_at(Rect::new(0, 0, 100, 20), &mut state, None);
        assert_eq!(state.selected, sample_node_tree().children.len() - 1);
    }

    #[test]
    fn a_wide_layout_renders_all_three_column_titles() {
        let mut state = MillerState::default();
        let buf = render_at(Rect::new(0, 0, 120, 20), &mut state, None);
        let rendered: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        for title in ["Parent", "Current", "Preview"] {
            assert!(
                rendered.contains(title),
                "missing {title} column: {rendered}"
            );
        }
    }

    #[test]
    fn the_parent_column_scrolls_to_the_current_directory() {
        let time = crate::test_support::timestamps();
        let mut parent = FileNode::new_directory("parent", time);
        parent.children = (0..100)
            .map(|index| FileNode::new_directory(format!("d-{index:03}"), time))
            .collect();
        let current = parent.children[80].clone();
        let view_root = sample_root_path().join("d-080");
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);

        let miller = MillerColumns::new(
            &current,
            Some(&parent),
            "d-080",
            &view_root,
            &marked,
            &clipboard,
            &theme,
        );
        StatefulWidget::render(miller, area, &mut buf, &mut MillerState::default());

        let rendered: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("d-080"), "{rendered}");
        assert!(!rendered.contains("d-000"), "{rendered}");
    }

    #[test]
    fn an_empty_directory_says_so() {
        let empty = FileNode::new_directory("empty", crate::test_support::timestamps());
        let root_path = sample_root_path();
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        let mut state = MillerState::default();

        let miller = MillerColumns::new(
            &empty, None, "empty", &root_path, &marked, &clipboard, &theme,
        );
        StatefulWidget::render(miller, area, &mut buf, &mut state);

        let rendered: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("(empty)"), "{rendered}");
    }

    #[test]
    fn timestamps_outside_the_representable_range_render_a_placeholder() {
        // A corrupt or absurd mtime must not panic the preview pane.
        assert_eq!(format_timestamp(i64::MAX as u64), "—");
        assert_ne!(format_timestamp(0), "—");
    }
}
