//! Directory tree widget.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, StatefulWidget, Widget};

use gravityfile_core::{FileNode, NodeKind};

use crate::app::state::{ClipboardMode, ClipboardState};
use crate::theme::Theme;
use crate::ui::{format_size, SizeBar};

/// State for the tree view.
#[derive(Debug, Default, Clone)]
pub struct TreeState {
    /// Currently selected index in the flattened view.
    pub selected: usize,
    /// Scroll offset.
    pub offset: usize,
    /// Set of expanded directory paths.
    pub expanded: HashSet<PathBuf>,
    /// Current root path.
    #[allow(dead_code)]
    root_path: PathBuf,
}

impl TreeState {
    /// Create new tree state.
    pub fn new(root_path: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        // Expand root by default
        expanded.insert(root_path.clone());
        Self {
            selected: 0,
            offset: 0,
            expanded,
            root_path,
        }
    }

    /// Toggle expansion of a path.
    pub fn toggle_expand(&mut self, path: &PathBuf) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.clone());
        }
    }

    /// Expand a path.
    pub fn expand(&mut self, path: &PathBuf) {
        self.expanded.insert(path.clone());
    }

    /// Collapse a path.
    pub fn collapse(&mut self, path: &PathBuf) {
        self.expanded.remove(path);
    }

    /// Check if a path is expanded.
    pub fn is_expanded(&self, path: &PathBuf) -> bool {
        self.expanded.contains(path)
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
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self, max: usize) {
        self.selected = max.saturating_sub(1);
    }

    /// Ensure selected item is visible, adjusting offset if needed.
    pub fn ensure_visible(&mut self, viewport_height: usize) {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + viewport_height {
            self.offset = self.selected - viewport_height + 1;
        }
    }
}

/// A flattened visible item in the tree.
#[derive(Debug, Clone)]
pub struct VisibleItem {
    pub path: PathBuf,
    pub node: VisibleNode,
    pub depth: usize,
    pub is_last_sibling: bool,
    pub parent_last_siblings: Vec<bool>,
}

/// Lightweight node info for display.
#[derive(Debug, Clone)]
pub struct VisibleNode {
    pub name: String,
    pub size: u64,
    pub kind: VisibleNodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleNodeKind {
    Directory { expanded: bool },
    File { executable: bool },
    Symlink { broken: bool },
    Other,
}

/// Tree view widget.
pub struct TreeView<'a> {
    root: &'a FileNode,
    root_path: &'a PathBuf,
    total_size: u64,
    theme: &'a Theme,
    marked: &'a HashSet<PathBuf>,
    clipboard: &'a ClipboardState,
    block: Option<Block<'a>>,
}

impl<'a> TreeView<'a> {
    /// Create a new tree view.
    pub fn new(
        root: &'a FileNode,
        root_path: &'a PathBuf,
        theme: &'a Theme,
        marked: &'a HashSet<PathBuf>,
        clipboard: &'a ClipboardState,
    ) -> Self {
        Self {
            root,
            root_path,
            total_size: root.size,
            theme,
            marked,
            clipboard,
            block: None,
        }
    }

    /// Set the block (border) for the widget.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Flatten tree to visible items based on expansion state.
    pub fn flatten(&self, state: &TreeState) -> Vec<VisibleItem> {
        let mut items = Vec::new();
        self.flatten_node(
            self.root,
            self.root_path.clone(),
            0,
            true,
            Vec::new(),
            state,
            &mut items,
        );
        items
    }

    fn flatten_node(
        &self,
        node: &FileNode,
        path: PathBuf,
        depth: usize,
        is_last: bool,
        parent_last_siblings: Vec<bool>,
        state: &TreeState,
        items: &mut Vec<VisibleItem>,
    ) {
        let is_expanded = state.is_expanded(&path);

        let kind = match &node.kind {
            NodeKind::Directory { .. } => VisibleNodeKind::Directory {
                expanded: is_expanded,
            },
            NodeKind::File { executable } => VisibleNodeKind::File {
                executable: *executable,
            },
            NodeKind::Symlink { broken, .. } => VisibleNodeKind::Symlink { broken: *broken },
            NodeKind::Other => VisibleNodeKind::Other,
        };

        items.push(VisibleItem {
            path: path.clone(),
            node: VisibleNode {
                name: node.name.to_string(),
                size: node.size,
                kind,
            },
            depth,
            is_last_sibling: is_last,
            parent_last_siblings: parent_last_siblings.clone(),
        });

        // If directory is expanded, add children
        if is_expanded && node.is_dir() {
            let child_count = node.children.len();
            for (i, child) in node.children.iter().enumerate() {
                let child_path = path.join(&*child.name);
                let child_is_last = i == child_count - 1;
                let mut child_parent_lasts = parent_last_siblings.clone();
                child_parent_lasts.push(is_last);

                self.flatten_node(
                    child,
                    child_path,
                    depth + 1,
                    child_is_last,
                    child_parent_lasts,
                    state,
                    items,
                );
            }
        }
    }
}

impl StatefulWidget for TreeView<'_> {
    type State = TreeState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Handle block/border
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

        let items = self.flatten(state);
        let viewport_height = inner_area.height as usize;

        // Ensure selected item is visible
        state.ensure_visible(viewport_height);

        // Calculate visible range
        let start = state.offset;
        let end = (start + viewport_height).min(items.len());

        // Size bar width
        let size_bar_width: u16 = 10;
        let size_text_width: u16 = 10;

        for (row_idx, item_idx) in (start..end).enumerate() {
            let item = &items[item_idx];
            let y = inner_area.y + row_idx as u16;
            let is_selected = item_idx == state.selected;
            let is_marked = self.marked.contains(&item.path);

            // Check if item is in clipboard
            let in_clipboard = self.clipboard.paths.contains(&item.path);
            let clipboard_mode = if in_clipboard {
                self.clipboard.mode
            } else {
                ClipboardMode::Empty
            };

            // Build tree prefix
            let mut prefix = String::new();
            for &parent_is_last in &item.parent_last_siblings {
                prefix.push_str(if parent_is_last { "  " } else { "│ " });
            }
            if item.depth > 0 {
                prefix.push_str(if item.is_last_sibling {
                    "└─"
                } else {
                    "├─"
                });
            }

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

            // Expand indicator
            let expand_indicator = match item.node.kind {
                VisibleNodeKind::Directory { expanded } => {
                    if expanded {
                        "▼ "
                    } else {
                        "▶ "
                    }
                }
                _ => "  ",
            };

            // Node style - clipboard items get special styling
            let base_style = match item.node.kind {
                VisibleNodeKind::Directory { .. } => self.theme.directory,
                VisibleNodeKind::File { executable: true } => self.theme.executable,
                VisibleNodeKind::File { executable: false } => self.theme.file,
                VisibleNodeKind::Symlink { broken: true } => {
                    self.theme.symlink.add_modifier(Modifier::DIM)
                }
                VisibleNodeKind::Symlink { broken: false } => self.theme.symlink,
                VisibleNodeKind::Other => self.theme.muted.into(),
            };

            // Apply clipboard styling (cut items are dimmed, copied items are italic)
            let base_style = if clipboard_mode == ClipboardMode::Cut {
                base_style.add_modifier(Modifier::DIM)
            } else if clipboard_mode == ClipboardMode::Copy {
                base_style.add_modifier(Modifier::ITALIC)
            } else {
                base_style
            };

            // Calculate available width for name
            let prefix_width = prefix.len() + checkbox_width as usize + expand_indicator.len();
            let available_for_name = inner_area
                .width
                .saturating_sub(prefix_width as u16)
                .saturating_sub(size_bar_width + 1)
                .saturating_sub(size_text_width + 1);

            // Truncate name if needed
            let name = if item.node.name.len() > available_for_name as usize {
                let truncated_len = available_for_name.saturating_sub(1) as usize;
                format!("{}…", &item.node.name[..truncated_len])
            } else {
                item.node.name.clone()
            };

            // Build line with checkbox
            let prefix_span = Span::styled(&prefix, self.theme.tree_lines);
            let checkbox_span = Span::styled(checkbox, checkbox_style);
            let expand_span = Span::styled(expand_indicator, Style::default().fg(self.theme.muted));
            let name_span = Span::styled(&name, base_style);

            // Pad name to fill space
            let name_padding =
                " ".repeat(available_for_name.saturating_sub(name.len() as u16) as usize);
            let padding_span = Span::raw(&name_padding);

            // Size text
            let size_text = format!("{:>10}", format_size(item.node.size));
            let size_span = Span::styled(&size_text, Style::default().fg(self.theme.muted));

            let line = Line::from(vec![
                prefix_span,
                checkbox_span,
                expand_span,
                name_span,
                padding_span,
                Span::raw(" "),
                size_span,
            ]);

            // Apply selection style (cursor highlight)
            let line = if is_selected {
                line.style(self.theme.selected)
            } else if is_marked {
                // Marked but not selected - subtle highlight
                line.style(Style::default().add_modifier(Modifier::BOLD))
            } else {
                line
            };

            // Render line
            let line_area = Rect::new(
                inner_area.x,
                y,
                inner_area.width.saturating_sub(size_bar_width + 1),
                1,
            );
            Widget::render(line, line_area, buf);

            // Render size bar
            let ratio = if self.total_size > 0 {
                item.node.size as f64 / self.total_size as f64
            } else {
                0.0
            };

            let bar_area = Rect::new(
                inner_area.x + inner_area.width - size_bar_width,
                y,
                size_bar_width,
                1,
            );

            let bar = SizeBar::new(ratio)
                .filled_style(self.theme.size_bar_style(ratio))
                .empty_style(Style::default().fg(self.theme.muted));

            if is_selected {
                // Dim the bar slightly for selected row
                Widget::render(bar, bar_area, buf);
            } else {
                Widget::render(bar, bar_area, buf);
            }
        }
    }
}

/// Get the selected item from a tree state and flattened items.
#[allow(dead_code)]
pub fn get_selected_item<'a>(state: &TreeState, items: &'a [VisibleItem]) -> Option<&'a VisibleItem> {
    items.get(state.selected)
}
