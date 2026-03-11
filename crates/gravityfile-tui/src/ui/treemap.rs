//! Treemap visualization for disk usage.
//!
//! Implements a squarified treemap algorithm for displaying file system
//! sizes in a space-filling rectangular layout.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Widget};

use gravityfile_core::FileNode;

use crate::app::state::ClipboardState;
use crate::theme::Theme;
use crate::ui::truncate_to_width;

/// State for the treemap view.
#[derive(Debug, Clone, Default)]
pub struct TreemapState {
    /// Currently selected rectangle index.
    pub selected: usize,
    /// Hover highlight index (from mouse).
    pub hover: Option<usize>,
}

impl TreemapState {
    /// Create new treemap state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Move selection to next rectangle.
    pub fn move_next(&mut self, count: usize) {
        self.selected = self.selected.saturating_add(1).min(count.saturating_sub(1));
    }

    /// Move selection to previous rectangle.
    pub fn move_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Reset selection.
    pub fn reset(&mut self) {
        self.selected = 0;
        self.hover = None;
    }
}

/// A rectangle in the treemap with associated data.
#[derive(Debug, Clone)]
pub struct TreemapRect {
    /// Path to the file/directory.
    pub path: PathBuf,
    /// Name for display.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Calculated rectangle.
    pub rect: Rect,
    /// Depth in hierarchy (for future depth-based coloring).
    #[allow(dead_code)]
    pub depth: usize,
}

/// Treemap widget.
pub struct TreemapView<'a> {
    root: &'a FileNode,
    root_path: &'a PathBuf,
    theme: &'a Theme,
    marked: &'a HashSet<PathBuf>,
    #[allow(dead_code)]
    clipboard: &'a ClipboardState,
    block: Option<Block<'a>>,
    /// Selected rectangle index for highlighting.
    selected: Option<usize>,
}

impl<'a> TreemapView<'a> {
    /// Create a new treemap view.
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
            theme,
            marked,
            clipboard,
            block: None,
            selected: None,
        }
    }

    /// Set the block (border) for the widget.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Set the selected rectangle index.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = Some(index);
        self
    }

    /// Calculate treemap rectangles using squarified algorithm.
    pub fn calculate_rects(&self, area: Rect) -> Vec<TreemapRect> {
        let mut rects = Vec::new();

        if area.width == 0 || area.height == 0 || self.root.size == 0 {
            return rects;
        }

        // Get direct children for the treemap (we only show one level at a time)
        let children: Vec<_> = self.root.children.iter().collect();

        if children.is_empty() {
            return rects;
        }

        // Prepare items sorted by size (descending)
        let mut items: Vec<(&FileNode, u64)> = children.iter().map(|n| (*n, n.size)).collect();
        items.sort_by(|a, b| b.1.cmp(&a.1));

        // Apply squarified algorithm
        self.squarify(&items, area, self.root.size, 0, &mut rects);

        rects
    }

    /// Squarified treemap layout algorithm.
    fn squarify(
        &self,
        items: &[(&FileNode, u64)],
        area: Rect,
        total_size: u64,
        depth: usize,
        rects: &mut Vec<TreemapRect>,
    ) {
        if items.is_empty() || area.width == 0 || area.height == 0 || total_size == 0 {
            return;
        }

        // For single item, just use the whole area
        if items.len() == 1 {
            let (node, size) = items[0];
            rects.push(TreemapRect {
                path: self.root_path.join(&*node.name),
                name: node.name.to_string(),
                size,
                is_dir: node.is_dir(),
                rect: area,
                depth,
            });
            return;
        }

        // Determine orientation (horizontal or vertical split)
        let horizontal = area.width >= area.height;

        // Try to find best row of items
        let mut row: Vec<usize> = Vec::new();
        let mut row_size: u64 = 0;
        let mut best_ratio = f64::MAX;

        let area_size = (area.width as u64) * (area.height as u64);

        for (i, (_, size)) in items.iter().enumerate() {
            let candidate_row_size = row_size + size;
            let candidate_area =
                (area_size as f64) * (candidate_row_size as f64 / total_size as f64);

            // Calculate aspect ratio of this row
            let row_items = i + 1;
            let avg_area = candidate_area / row_items as f64;

            let (w, h) = if horizontal {
                let row_width = (candidate_area / area.height as f64).max(1.0);
                let item_height = (avg_area / row_width).max(1.0);
                (row_width, item_height)
            } else {
                let row_height = (candidate_area / area.width as f64).max(1.0);
                let item_width = (avg_area / row_height).max(1.0);
                (item_width, row_height)
            };

            let ratio = (w / h).max(h / w);

            if ratio <= best_ratio || i == 0 {
                best_ratio = ratio;
                row.push(i);
                row_size = candidate_row_size;
            } else {
                break;
            }
        }

        // Layout the row
        if row.is_empty() {
            return;
        }

        let row_fraction = row_size as f64 / total_size as f64;

        let (row_area, remaining_area) = if horizontal {
            let row_width = ((area.width as f64) * row_fraction).max(1.0) as u16;
            let row_width = row_width.min(area.width);
            (
                Rect::new(area.x, area.y, row_width, area.height),
                Rect::new(
                    area.x + row_width,
                    area.y,
                    area.width.saturating_sub(row_width),
                    area.height,
                ),
            )
        } else {
            let row_height = ((area.height as f64) * row_fraction).max(1.0) as u16;
            let row_height = row_height.min(area.height);
            (
                Rect::new(area.x, area.y, area.width, row_height),
                Rect::new(
                    area.x,
                    area.y + row_height,
                    area.width,
                    area.height.saturating_sub(row_height),
                ),
            )
        };

        // Place items in the row
        let mut offset: u16 = 0;
        for &idx in &row {
            let (node, size) = items[idx];
            // Guard against division by zero (shouldn't happen but be defensive)
            let item_fraction = if row_size > 0 {
                size as f64 / row_size as f64
            } else {
                1.0 / row.len() as f64 // Equal distribution fallback
            };

            let item_rect = if horizontal {
                let item_height = ((row_area.height as f64) * item_fraction).max(1.0) as u16;
                let item_height = item_height.min(row_area.height.saturating_sub(offset));
                let rect = Rect::new(row_area.x, row_area.y + offset, row_area.width, item_height);
                offset += item_height;
                rect
            } else {
                let item_width = ((row_area.width as f64) * item_fraction).max(1.0) as u16;
                let item_width = item_width.min(row_area.width.saturating_sub(offset));
                let rect = Rect::new(row_area.x + offset, row_area.y, item_width, row_area.height);
                offset += item_width;
                rect
            };

            if item_rect.width > 0 && item_rect.height > 0 {
                rects.push(TreemapRect {
                    path: self.root_path.join(&*node.name),
                    name: node.name.to_string(),
                    size,
                    is_dir: node.is_dir(),
                    rect: item_rect,
                    depth,
                });
            }
        }

        // Process remaining items
        let remaining_items: Vec<_> = items.iter().skip(row.len()).cloned().collect();
        let remaining_size = total_size - row_size;

        self.squarify(
            &remaining_items,
            remaining_area,
            remaining_size,
            depth,
            rects,
        );
    }
}

impl Widget for TreemapView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
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

        // Calculate rectangles
        let rects = self.calculate_rects(inner_area);

        if rects.is_empty() {
            let msg = Line::styled("(empty)", Style::default().fg(self.theme.muted));
            Paragraph::new(msg).render(inner_area, buf);
            return;
        }

        // Render each rectangle
        for (idx, treemap_rect) in rects.iter().enumerate() {
            let rect = treemap_rect.rect;

            if rect.width < 2 || rect.height < 1 {
                continue;
            }

            // Determine color based on size ratio
            let ratio = treemap_rect.size as f64 / self.root.size as f64;
            let color = self.theme.size_color(ratio);

            // Check if marked
            let is_marked = self.marked.contains(&treemap_rect.path);
            // Check if selected
            let is_selected = self.selected == Some(idx);

            // Base style - highlight selected with reversed colors or a distinct style
            let style = if is_selected {
                Style::default()
                    .fg(self.theme.background)
                    .bg(color)
                    .add_modifier(Modifier::BOLD)
            } else if is_marked {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };

            // Background style for fill
            let bg_style = if is_selected {
                Style::default().bg(color)
            } else {
                Style::default().bg(self.theme.background)
            };

            // Draw border - use different style for directories vs files
            let border_char = if treemap_rect.is_dir { '─' } else { '╌' };

            // Fill background with a gradient based on size
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
            }

            // Draw border
            if rect.width >= 2 && rect.height >= 1 {
                // Top border
                for x in rect.x..rect.x + rect.width {
                    if let Some(cell) = buf.cell_mut((x, rect.y)) {
                        cell.set_char(border_char);
                        cell.set_style(style);
                    }
                }

                // Left border
                for y in rect.y..rect.y + rect.height {
                    if let Some(cell) = buf.cell_mut((rect.x, y)) {
                        cell.set_char('│');
                        cell.set_style(style);
                    }
                }

                // Corners
                if let Some(cell) = buf.cell_mut((rect.x, rect.y)) {
                    cell.set_char('┌');
                }
                if rect.width > 1
                    && let Some(cell) = buf.cell_mut((rect.x + rect.width - 1, rect.y))
                {
                    cell.set_char('┐');
                    cell.set_style(style);
                }
            }

            // Draw label if there's room
            if rect.width >= 4 && rect.height >= 2 {
                let available_width = (rect.width as usize).saturating_sub(2);
                let label = {
                    use unicode_width::UnicodeWidthStr;
                    if treemap_rect.name.width() > available_width {
                        format!(
                            "{}…",
                            truncate_to_width(
                                &treemap_rect.name,
                                available_width.saturating_sub(1)
                            )
                        )
                    } else {
                        treemap_rect.name.clone()
                    }
                };

                let label_y = rect.y + 1;
                let label_x = rect.x + 1;

                for (i, ch) in label.chars().enumerate() {
                    let x = label_x + i as u16;
                    if x < rect.x + rect.width - 1
                        && let Some(cell) = buf.cell_mut((x, label_y))
                    {
                        cell.set_char(ch);
                        cell.set_style(style.add_modifier(Modifier::BOLD));
                    }
                }
            }

            // Draw size if there's room
            if rect.width >= 6 && rect.height >= 3 {
                let size_str = crate::ui::format_size(treemap_rect.size);
                let size_y = rect.y + 2;
                let size_x = rect.x + 1;

                for (i, ch) in size_str.chars().enumerate() {
                    let x = size_x + i as u16;
                    if x < rect.x + rect.width - 1
                        && let Some(cell) = buf.cell_mut((x, size_y))
                    {
                        cell.set_char(ch);
                        cell.set_style(Style::default().fg(self.theme.muted));
                    }
                }
            }
        }
    }
}
