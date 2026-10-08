//! Directory tree widget.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, StatefulWidget, Widget};

use gravityfile_core::{FileNode, GitStatus, NodeKind};
use unicode_width::UnicodeWidthStr;

use crate::app::state::{ClipboardMode, ClipboardState};
use crate::theme::Theme;
use crate::ui::{CompactSizeBar, fit_to_width, format_size};

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
    pub fn toggle_expand(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_path_buf());
        }
    }

    /// Expand a path.
    pub fn expand(&mut self, path: &Path) {
        self.expanded.insert(path.to_path_buf());
    }

    /// Collapse a path.
    pub fn collapse(&mut self, path: &Path) {
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
    pub git_status: Option<GitStatus>,
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

    /// Count visible items without allocating a Vec.
    /// Use this when only the count is needed (e.g. updating `cached_tree_len`).
    pub fn flatten_count(&self, state: &TreeState) -> usize {
        self.count_node(self.root, &self.root_path.clone(), state)
    }

    fn count_node(&self, node: &FileNode, path: &PathBuf, state: &TreeState) -> usize {
        let is_expanded = state.is_expanded(path);
        let mut count = 1; // this node itself
        if is_expanded && node.is_dir() {
            for child in &node.children {
                let child_path = path.join(child.file_name());
                count += self.count_node(child, &child_path, state);
            }
        }
        count
    }

    #[allow(clippy::too_many_arguments)]
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
                name: node.display_name().into_owned(),
                size: node.size,
                kind,
                git_status: node.git_status,
            },
            depth,
            is_last_sibling: is_last,
            parent_last_siblings: parent_last_siblings.clone(),
        });

        // If directory is expanded, add children
        if is_expanded && node.is_dir() {
            let child_count = node.children.len();
            for (i, child) in node.children.iter().enumerate() {
                let child_path = path.join(child.file_name());
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

        // Size bar width. A column narrower than the bar cannot show it, and in
        // that case the row text gets the space back instead of being squeezed
        // into nothing.
        let size_bar_width: u16 = if inner_area.width >= 10 { 10 } else { 0 };
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

            // Available width for the name, in terminal columns: the guide
            // lines and indicators are multi-byte but one column per char.
            let prefix_width = prefix.width() + checkbox_width as usize + expand_indicator.width();
            let available_for_name = inner_area
                .width
                .saturating_sub(prefix_width as u16)
                .saturating_sub(size_bar_width + 1)
                .saturating_sub(size_text_width + 1);

            // Truncate name if needed (Unicode-safe)
            let name = fit_to_width(&item.node.name, available_for_name as usize);

            // Build line with checkbox
            let prefix_span = Span::styled(&prefix, self.theme.tree_lines);
            let checkbox_span = Span::styled(checkbox, checkbox_style);
            let expand_span = Span::styled(expand_indicator, Style::default().fg(self.theme.muted));
            let name_span = Span::styled(&name, base_style);

            // Git status indicator
            let (git_indicator, git_style) = match item.node.git_status {
                Some(status) if status.is_displayable() => {
                    let color = self.theme.git_status_color(status);
                    (
                        format!(" {}", status.indicator()),
                        Style::default().fg(color),
                    )
                }
                _ => (String::new(), Style::default()),
            };
            let git_span = Span::styled(&git_indicator, git_style);

            // Pad name to fill space (accounting for git indicator)
            // Use display width, not byte length, for correct Unicode padding
            let git_display_width = git_indicator.width() as u16;
            let name_display_width = name.width() as u16;
            let name_padding = " ".repeat(
                available_for_name
                    .saturating_sub(name_display_width)
                    .saturating_sub(git_display_width) as usize,
            );
            let padding_span = Span::raw(&name_padding);

            // Size text - show "..." for directories with unknown size (0 bytes)
            let size_text = if matches!(item.node.kind, VisibleNodeKind::Directory { .. })
                && item.node.size == 0
            {
                format!("{:>10}", "...")
            } else {
                format!("{:>10}", format_size(item.node.size))
            };
            let size_span = Span::styled(&size_text, Style::default().fg(self.theme.muted));

            let line = Line::from(vec![
                prefix_span,
                checkbox_span,
                expand_span,
                name_span,
                git_span,
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
                inner_area
                    .width
                    .saturating_sub(size_bar_width)
                    .saturating_sub(u16::from(size_bar_width > 0)),
                1,
            );
            Widget::render(line, line_area, buf);

            // Render the size bar hugging the right edge, when there is one.
            if size_bar_width > 0 {
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

                let bar = CompactSizeBar::new(ratio).style(self.theme.size_bar_style(ratio));
                Widget::render(bar, bar_area, buf);
            }
        }
    }
}

/// Get the selected item from a tree state and flattened items.
#[allow(dead_code)]
pub fn get_selected_item<'a>(
    state: &TreeState,
    items: &'a [VisibleItem],
) -> Option<&'a VisibleItem> {
    items.get(state.selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{sample_node_tree, sample_root_path};
    use ratatui::buffer::Buffer;

    fn render_at(area: Rect, state: &mut TreeState) -> Buffer {
        let root = sample_node_tree();
        let root_path = sample_root_path();
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();
        let mut buf = Buffer::empty(area);
        let view = TreeView::new(&root, &root_path, &theme, &marked, &clipboard);
        StatefulWidget::render(view, area, &mut buf, state);
        buf
    }

    fn expanded_state() -> TreeState {
        let mut state = TreeState::new(sample_root_path());
        state.expand(&sample_root_path().join("src"));
        state
    }

    #[test]
    fn flatten_lists_only_expanded_subtrees() {
        let root = sample_node_tree();
        let root_path = sample_root_path();
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();
        let view = TreeView::new(&root, &root_path, &theme, &marked, &clipboard);

        // Only the root is expanded, so its 7 direct children are visible.
        let collapsed = TreeState::new(root_path.clone());
        let names: Vec<_> = view
            .flatten(&collapsed)
            .into_iter()
            .map(|item| item.node.name)
            .collect();
        assert_eq!(names.len(), 8, "root plus its direct children: {names:?}");
        assert!(!names.contains(&"modified.rs".to_string()));

        // Expanding `src` reveals its children but not `nested`'s.
        let mut state = collapsed.clone();
        state.expand(&root_path.join("src"));
        let names: Vec<_> = view
            .flatten(&state)
            .into_iter()
            .map(|item| item.node.name)
            .collect();
        assert!(names.contains(&"modified.rs".to_string()));
        assert!(!names.contains(&"deep.txt".to_string()));

        state.expand(&root_path.join("src").join("nested"));
        let names: Vec<_> = view
            .flatten(&state)
            .into_iter()
            .map(|item| item.node.name)
            .collect();
        assert!(names.contains(&"deep.txt".to_string()));
    }

    #[test]
    fn flatten_count_matches_flatten_length() {
        let root = sample_node_tree();
        let root_path = sample_root_path();
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();

        // `flatten_count` exists purely to skip the Vec allocation, so the two
        // must never disagree — the app uses the count to bound navigation
        // against a list produced by `flatten`.
        for state in [TreeState::new(root_path.clone()), expanded_state()] {
            let view = TreeView::new(&root, &root_path, &theme, &marked, &clipboard);
            let counted = view.flatten_count(&state);
            let view = TreeView::new(&root, &root_path, &theme, &marked, &clipboard);
            assert_eq!(counted, view.flatten(&state).len());
        }
    }

    #[test]
    fn flatten_builds_paths_by_joining_names() {
        let root = sample_node_tree();
        let root_path = sample_root_path();
        let theme = Theme::dark();
        let marked = HashSet::new();
        let clipboard = ClipboardState::default();
        let mut state = expanded_state();
        state.expand(&root_path.join("src").join("nested"));

        let view = TreeView::new(&root, &root_path, &theme, &marked, &clipboard);
        let items = view.flatten(&state);
        let deep = items
            .iter()
            .find(|item| item.node.name == "deep.txt")
            .expect("deep.txt is visible");
        assert_eq!(deep.path, root_path.join("src/nested/deep.txt"));
        assert_eq!(deep.depth, 3);
    }

    #[test]
    fn toggle_expand_flips_state() {
        let mut state = TreeState::new(sample_root_path());
        let path = sample_root_path().join("src");
        assert!(!state.is_expanded(&path));
        state.toggle_expand(&path);
        assert!(state.is_expanded(&path));
        state.toggle_expand(&path);
        assert!(!state.is_expanded(&path));
    }

    #[test]
    fn ensure_visible_scrolls_the_minimum_distance() {
        let mut state = TreeState::new(sample_root_path());

        // Selection below the viewport scrolls just far enough to show it.
        state.selected = 12;
        state.ensure_visible(10);
        assert_eq!(state.offset, 3);

        // Selection above the viewport scrolls up to sit on the first row.
        state.selected = 1;
        state.ensure_visible(10);
        assert_eq!(state.offset, 1);

        // A selection already inside the viewport does not move the offset.
        state.selected = 5;
        state.ensure_visible(10);
        assert_eq!(state.offset, 1);
    }

    #[test]
    fn navigation_stays_within_bounds() {
        let mut state = TreeState::new(sample_root_path());
        state.move_up(100);
        assert_eq!(state.selected, 0);
        state.move_down(100, 8);
        assert_eq!(state.selected, 7);
        state.jump_to_top();
        assert_eq!(state.selected, 0);
        state.jump_to_bottom(8);
        assert_eq!(state.selected, 7);

        // An empty tree must not underflow.
        state.jump_to_bottom(0);
        assert_eq!(state.selected, 0);
        state.move_down(1, 0);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn renders_at_every_size_without_panicking() {
        // The size bar is anchored to the right edge; a column narrower than
        // the bar used to compute an out-of-bounds rect and abort the app.
        for width in 1u16..=60 {
            for height in [1u16, 2, 3, 10, 40] {
                let mut state = expanded_state();
                let _ = render_at(Rect::new(0, 0, width, height), &mut state);
            }
        }
    }

    #[test]
    fn renders_at_every_offset_within_a_block() {
        // A non-zero origin is what the app actually passes, and it is what
        // makes right-edge arithmetic easy to get wrong.
        for x in [0u16, 1, 7] {
            for y in [0u16, 1, 5] {
                let area = Rect::new(x, y, 40, 12);
                let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
                let root = sample_node_tree();
                let root_path = sample_root_path();
                let theme = Theme::dark();
                let marked = HashSet::new();
                let clipboard = ClipboardState::default();
                let mut state = expanded_state();
                let view = TreeView::new(&root, &root_path, &theme, &marked, &clipboard)
                    .block(Block::default().borders(ratatui::widgets::Borders::ALL));
                StatefulWidget::render(view, area, &mut buf, &mut state);
            }
        }
    }

    #[test]
    fn a_narrow_column_still_renders_rows() {
        // Below the size-bar width the bar is dropped and the row reclaims its
        // columns, so the tree structure stays visible. (The name budget is
        // still spent on the size text first, so names themselves only appear
        // once the column is wide enough for both.)
        let mut state = expanded_state();
        let buf = render_at(Rect::new(0, 0, 9, 10), &mut state);
        let rendered: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        assert!(
            rendered.contains('├') || rendered.contains('└'),
            "narrow render produced no rows at all: {rendered:?}"
        );
    }

    #[test]
    fn a_wide_column_renders_the_size_bar() {
        let mut state = expanded_state();
        let buf = render_at(Rect::new(0, 0, 60, 10), &mut state);
        let rendered: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        assert!(
            rendered.contains("65.5 kB"),
            "expected a formatted size in the row: {rendered:?}"
        );
    }

    #[test]
    fn exactly_one_row_carries_the_selection_highlight() {
        let mut state = expanded_state();
        state.selected = 2;
        let buf = render_at(Rect::new(0, 0, 60, 10), &mut state);
        let selected_bg = Theme::dark()
            .selected
            .bg
            .expect("selection has a background");

        let highlighted: Vec<u16> = (0..10u16)
            .filter(|&y| buf[(0, y)].bg == selected_bg)
            .collect();
        assert_eq!(highlighted, vec![2], "only the selected row is highlighted");
    }
}
