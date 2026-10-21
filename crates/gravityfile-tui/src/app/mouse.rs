//! Mouse input: clicks, scrolling and double-clicks on rows.

use super::App;
use super::state::{LayoutMode, View};
use crate::event::MouseAction;

impl App {
    /// Handle a mouse action.
    pub(super) fn handle_mouse_action(&mut self, action: MouseAction) {
        // Store the last content area dimensions for hit testing
        // In a real implementation, we would track the rendered areas from the last frame
        // For now, we implement basic scroll wheel and click functionality

        // The wheel over the Miller preview scrolls the preview.
        if let MouseAction::ScrollUp { x, .. } | MouseAction::ScrollDown { x, .. } = action
            && self.view == View::Explorer
            && self.layout_mode == LayoutMode::Miller
            && x >= self.preview_area.get().x
        {
            let step = super::constants::PREVIEW_SCROLL_STEP;
            let delta = if matches!(action, MouseAction::ScrollUp { .. }) {
                -step
            } else {
                step
            };
            self.scroll_preview(delta);
            return;
        }

        match action {
            MouseAction::ScrollUp { .. } => {
                // Scroll up - move selection up
                match self.view {
                    View::Explorer => {
                        for _ in 0..3 {
                            self.move_up();
                        }
                    }
                    View::Duplicates => {
                        // Collect group info to avoid borrow issues
                        let group_info: Vec<usize> = self
                            .get_filtered_duplicates()
                            .map(|(filtered, _)| filtered.iter().map(|g| g.paths.len()).collect())
                            .unwrap_or_default();
                        let group_count = group_info.len();
                        for _ in 0..3 {
                            self.duplicates_state
                                .move_up(group_count, |i| group_info.get(i).copied().unwrap_or(0));
                        }
                    }
                    View::Age => {
                        for _ in 0..3 {
                            if self.selected_stale_dir > 0 {
                                self.selected_stale_dir -= 1;
                            }
                        }
                    }
                    View::Errors => {
                        for _ in 0..3 {
                            if self.selected_warning > 0 {
                                self.selected_warning -= 1;
                            }
                        }
                    }
                    View::Treemap => {
                        for _ in 0..3 {
                            self.nav.treemap_state.move_prev();
                        }
                    }
                }
            }
            MouseAction::ScrollDown { .. } => {
                // Scroll down - move selection down
                match self.view {
                    View::Explorer => {
                        for _ in 0..3 {
                            self.move_down();
                        }
                    }
                    View::Duplicates => {
                        // Collect group info to avoid borrow issues
                        let group_info: Vec<usize> = self
                            .get_filtered_duplicates()
                            .map(|(filtered, _)| filtered.iter().map(|g| g.paths.len()).collect())
                            .unwrap_or_default();
                        let group_count = group_info.len();
                        for _ in 0..3 {
                            self.duplicates_state.move_down(group_count, |i| {
                                group_info.get(i).copied().unwrap_or(0)
                            });
                        }
                    }
                    View::Age => {
                        let max = self
                            .get_filtered_stale_dirs()
                            .map(|dirs| dirs.len().saturating_sub(1))
                            .unwrap_or(0);
                        for _ in 0..3 {
                            if self.selected_stale_dir < max {
                                self.selected_stale_dir += 1;
                            }
                        }
                    }
                    View::Errors => {
                        let max = self.warning_count().saturating_sub(1);
                        for _ in 0..3 {
                            if self.selected_warning < max {
                                self.selected_warning += 1;
                            }
                        }
                    }
                    View::Treemap => {
                        for _ in 0..3 {
                            self.nav.treemap_state.move_next(self.cached_treemap_len);
                        }
                    }
                }
            }
            MouseAction::Click { y, .. } if self.view == View::Explorer => {
                // For now, simple click-to-select functionality
                // Calculate which item was clicked based on the Y position
                // This is a simplified implementation - a more complete version would
                // track rendered areas from the last frame

                let list_top = self.estimated_list_top_row();
                let _footer_height: u16 = 1; // Reserved for future use in click calculation

                if y >= list_top {
                    let relative_y = (y - list_top) as usize;

                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let new_selected = self.nav.tree_state.offset + relative_y;
                            if new_selected < self.cached_tree_len {
                                self.nav.tree_state.selected = new_selected;
                            }
                        }
                        LayoutMode::Miller => {
                            // In Miller mode, the middle column starts after the left column
                            // This is a simplified version - we just select by row
                            let new_selected = self.nav.miller_state.offset + relative_y;
                            if new_selected < self.cached_miller_len {
                                self.nav.miller_state.selected = new_selected;
                                self.update_preview();
                            }
                        }
                    }
                }
            }
            MouseAction::DoubleClick { y, .. } if self.view == View::Explorer => {
                // Double-click to enter directory
                let list_top = self.estimated_list_top_row();

                if y >= list_top {
                    let relative_y = (y - list_top) as usize;

                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let new_selected = self.nav.tree_state.offset + relative_y;
                            if new_selected < self.cached_tree_len {
                                self.nav.tree_state.selected = new_selected;
                                self.enter_selected();
                            }
                        }
                        LayoutMode::Miller => {
                            let new_selected = self.nav.miller_state.offset + relative_y;
                            if new_selected < self.cached_miller_len {
                                self.nav.miller_state.selected = new_selected;
                                self.enter_selected();
                            }
                        }
                    }
                }
            }
            MouseAction::RightClick { y, .. } if self.view == View::Explorer => {
                // Right-click to toggle mark
                let list_top = self.estimated_list_top_row();

                if y >= list_top {
                    let relative_y = (y - list_top) as usize;

                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let new_selected = self.nav.tree_state.offset + relative_y;
                            if new_selected < self.cached_tree_len {
                                self.nav.tree_state.selected = new_selected;
                                self.toggle_mark();
                            }
                        }
                        LayoutMode::Miller => {
                            let new_selected = self.nav.miller_state.offset + relative_y;
                            if new_selected < self.cached_miller_len {
                                self.nav.miller_state.selected = new_selected;
                                self.toggle_mark();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Absolute row of the first list item, for click hit testing.
    ///
    /// Mirrors the renderer's vertical stack: the header row, an optional
    /// directory-tab row, the view-tab row, an optional scan/analysis/quick-list
    /// banner row, then the list block's top border (both the Tree and Miller
    /// widgets draw inside a bordered block). Hit testing is lightweight, but this
    /// must match the layout exactly or clicks land on the wrong row.
    pub(super) fn estimated_list_top_row(&self) -> u16 {
        let chrome_rows = if self.tab_manager.len() > 1 { 3 } else { 2 };
        let banner_rows =
            u16::from(self.scan_progress.is_some() || self.analyzing || !self.has_full_scan);
        let border_row = 1;
        chrome_rows + banner_rows + border_row
    }
}
