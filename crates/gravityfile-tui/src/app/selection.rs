//! Moving the selection, expanding rows and marking entries.

use std::path::{Path, PathBuf};

use super::state::{LayoutMode, View};
use super::{App, SelectedInfo};
use crate::ui::TreeView;

impl App {
    /// Toggle marking the currently selected item.
    pub(super) fn toggle_mark(&mut self) {
        let path = match self.view {
            View::Explorer => {
                if let Some((node, root_path)) = self.get_view_root_node() {
                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let selected = TreeView::new(
                                node,
                                &root_path,
                                &self.theme,
                                &self.marked,
                                &self.clipboard,
                            )
                            .row(&self.nav.tree_state, self.nav.tree_state.selected);
                            selected.as_ref().map(|i| i.path.clone())
                        }
                        LayoutMode::Miller => node
                            .children
                            .get(self.nav.miller_state.selected)
                            .map(|child| self.nav.view_root.join(child.file_name())),
                    }
                } else {
                    None
                }
            }
            View::Duplicates => {
                // Hold the immutable report independently of App so marking can
                // mutate the selection set without cloning the group's paths.
                let Some(report) = self.duplicates.clone() else {
                    return;
                };
                let Some(group_index) = self
                    .duplicate_filter
                    .get(&report, &self.nav.view_root, &self.path)
                    .groups
                    .get(self.duplicates_state.selected_group)
                    .map(|(index, _)| *index)
                else {
                    return;
                };
                let group = &report.groups[group_index];
                let selected = self
                    .duplicates_state
                    .selected_item(self.duplicates_state.selected_group);
                let is_on_header = !self
                    .duplicates_state
                    .is_expanded(self.duplicates_state.selected_group)
                    || selected == 0;
                if is_on_header {
                    if group.paths.iter().any(|path| self.marked.contains(path)) {
                        for path in &group.paths {
                            self.marked.remove(path);
                        }
                    } else {
                        // Always keep one copy, matching `d` on a header.
                        for path in group.paths.iter().skip(1) {
                            self.mark(path.clone());
                        }
                    }
                    return;
                }
                group.paths.get(selected - 1).cloned()
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let selected = self
                        .selected_stale_dir
                        .min(filtered.len().saturating_sub(1));
                    filtered.get(selected).map(|d| d.path.clone())
                } else {
                    None
                }
            }
            View::Errors => {
                // Only allow marking broken symlinks for deletion
                if let Some(warning) = self.displayed_warning(self.selected_warning) {
                    if warning.kind == gravityfile_core::WarningKind::BrokenSymlink {
                        Some(warning.path.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            View::Treemap => self.get_selected_treemap_child().map(|(_, path)| path),
        };

        if let Some(path) = path
            && !self.marked.remove(&path)
        {
            self.mark(path);
        }
    }

    /// Get the size of a path from the tree.
    /// The size of the entry at `target`, looked up by its path's components.
    pub(super) fn get_path_size(&self, target: &Path) -> Option<u64> {
        Some(self.tree.as_ref()?.find(target)?.size)
    }

    pub(super) fn move_up(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.nav.tree_state.move_up(1),
                LayoutMode::Miller => {
                    self.nav.miller_state.move_up(1);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Get counts first to avoid borrow issues
                let (group_count, file_counts) =
                    if let Some((filtered, _)) = self.get_filtered_duplicates() {
                        let counts: Vec<usize> = filtered.iter().map(|g| g.paths.len()).collect();
                        (filtered.len(), counts)
                    } else {
                        (0, Vec::new())
                    };
                if group_count > 0 {
                    self.duplicates_state.move_up(group_count, |idx| {
                        file_counts.get(idx).copied().unwrap_or(0)
                    });
                }
            }
            View::Age => {
                self.selected_stale_dir = self.selected_stale_dir.saturating_sub(1);
            }
            View::Errors => {
                self.selected_warning = self.selected_warning.saturating_sub(1);
            }
            View::Treemap => {
                self.nav.treemap_state.move_prev();
            }
        }
    }

    pub(super) fn move_down(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.nav.tree_state.move_down(1, self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.nav.miller_state.move_down(1, self.cached_miller_len);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Get counts first to avoid borrow issues
                let (group_count, file_counts) =
                    if let Some((filtered, _)) = self.get_filtered_duplicates() {
                        let counts: Vec<usize> = filtered.iter().map(|g| g.paths.len()).collect();
                        (filtered.len(), counts)
                    } else {
                        (0, Vec::new())
                    };
                if group_count > 0 {
                    self.duplicates_state.move_down(group_count, |idx| {
                        file_counts.get(idx).copied().unwrap_or(0)
                    });
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_stale_dir = (self.selected_stale_dir + 1).min(max);
                }
            }
            View::Errors => {
                let max = self.warning_count().saturating_sub(1);
                self.selected_warning = (self.selected_warning + 1).min(max);
            }
            View::Treemap => {
                self.nav.treemap_state.move_next(self.cached_treemap_len);
            }
        }
    }

    /// Rows a page moves: a screenful, keeping one row of overlap.
    pub(super) fn page_size(&self) -> usize {
        self.viewport_height.saturating_sub(1).max(1)
    }

    pub(super) fn page_up(&mut self) {
        let page = self.page_size();
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.nav.tree_state.move_up(page),
                LayoutMode::Miller => {
                    self.nav.miller_state.move_up(page);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Page up - move multiple groups up
                self.duplicates_state.selected_group =
                    self.duplicates_state.selected_group.saturating_sub(page);
            }
            View::Age => {
                self.selected_stale_dir = self.selected_stale_dir.saturating_sub(page);
            }
            View::Errors => {
                self.selected_warning = self.selected_warning.saturating_sub(page);
            }
            View::Treemap => {
                // Page up in treemap - move selection backward a page
                for _ in 0..page {
                    self.nav.treemap_state.move_prev();
                }
            }
        }
    }

    pub(super) fn page_down(&mut self) {
        let page = self.page_size();
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.nav.tree_state.move_down(page, self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.nav
                        .miller_state
                        .move_down(page, self.cached_miller_len);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Page down - move multiple groups down
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let max = filtered.len().saturating_sub(1);
                    self.duplicates_state.selected_group =
                        (self.duplicates_state.selected_group + page).min(max);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_stale_dir = (self.selected_stale_dir + page).min(max);
                }
            }
            View::Errors => {
                let max = self.warning_count().saturating_sub(1);
                self.selected_warning = (self.selected_warning + page).min(max);
            }
            View::Treemap => {
                // Page down in treemap - move selection forward a page
                for _ in 0..page {
                    self.nav.treemap_state.move_next(self.cached_treemap_len);
                }
            }
        }
    }

    pub(super) fn jump_to_top(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.nav.tree_state.jump_to_top(),
                LayoutMode::Miller => {
                    self.nav.miller_state.jump_to_top();
                    self.update_preview();
                }
            },
            View::Duplicates => self.duplicates_state.reset(),
            View::Age => self.selected_stale_dir = 0,
            View::Errors => self.selected_warning = 0,
            View::Treemap => self.nav.treemap_state.reset(),
        }
    }

    pub(super) fn jump_to_bottom(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.nav.tree_state.jump_to_bottom(self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.nav.miller_state.jump_to_bottom(self.cached_miller_len);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    self.duplicates_state.selected_group = filtered.len().saturating_sub(1);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    self.selected_stale_dir = filtered.len().saturating_sub(1);
                }
            }
            View::Errors => {
                self.selected_warning = self.warning_count().saturating_sub(1);
            }
            View::Treemap => {
                // Jump to last item in treemap
                self.nav.treemap_state.selected = self.cached_treemap_len.saturating_sub(1);
            }
        }
    }

    pub(super) fn expand_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let selected =
                TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                    .row(&self.nav.tree_state, self.nav.tree_state.selected);
            if let Some(item) = selected.as_ref() {
                self.load_children(&item.path);
                self.nav.tree_state.expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    /// Collapse the selected item, or navigate back if already collapsed/not a directory.
    /// This provides vim-style navigation where 'h' goes up when there's nothing to collapse.
    pub(super) fn collapse_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let selected =
                TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                    .row(&self.nav.tree_state, self.nav.tree_state.selected);
            if let Some(item) = selected.as_ref() {
                // Check if this is an expanded directory
                let is_expanded_dir = matches!(
                    item.node.kind,
                    crate::ui::VisibleNodeKind::Directory { expanded: true }
                );

                if is_expanded_dir {
                    // Collapse the directory
                    self.nav.tree_state.collapse(&item.path);
                    self.update_cached_tree_len();
                } else {
                    // Not expanded or not a directory - navigate back to parent
                    self.go_back();
                }
            }
        }
    }

    pub(super) fn toggle_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let selected =
                TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                    .row(&self.nav.tree_state, self.nav.tree_state.selected);
            if let Some(item) = selected.as_ref() {
                self.nav.tree_state.toggle_expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    pub(super) fn get_selected_info(&self) -> Option<SelectedInfo> {
        let path = self.get_selected_path()?;
        let tree = self.tree.as_ref()?;
        let node = Self::find_node_at_path(&tree.root, &path, &tree.root_path)?;
        Some(SelectedInfo {
            name: node.display_name().into_owned(),
            path,
            size: node.size,
            file_count: node.file_count(),
            dir_count: node.dir_count(),
            modified: node.timestamps.modified,
            is_dir: node.is_dir(),
        })
    }

    /// Check the active view's selection, never an inactive explorer cursor.
    pub(super) fn is_selected_file(&self) -> bool {
        self.get_selected_info().is_some_and(|info| !info.is_dir)
    }

    /// Get the path of the currently selected item.
    pub(super) fn get_selected_path(&self) -> Option<PathBuf> {
        match self.view {
            View::Explorer => {}
            View::Treemap => return self.get_selected_treemap_child().map(|(_, path)| path),
            View::Errors => {
                return self
                    .displayed_warning(self.selected_warning)
                    .map(|warning| warning.path.clone());
            }
            View::Duplicates => {
                let (groups, _) = self.get_filtered_duplicates()?;
                return self.duplicates_state.selected_file_path(&groups).cloned();
            }
            View::Age => {
                let directories = self.get_filtered_stale_dirs()?;
                return directories
                    .get(self.selected_stale_dir)
                    .map(|directory| directory.path.clone());
            }
        }

        match self.layout_mode {
            LayoutMode::Tree => {
                if let Some((view_node, view_path)) = self.get_view_root_node() {
                    return TreeView::new(
                        view_node,
                        &view_path,
                        &self.theme,
                        &self.marked,
                        &self.clipboard,
                    )
                    .row(&self.nav.tree_state, self.nav.tree_state.selected)
                    .map(|item| item.path);
                }
                None
            }
            LayoutMode::Miller => {
                if let Some((view_node, _)) = self.get_view_root_node()
                    && let Some(child) = view_node.children.get(self.nav.miller_state.selected)
                {
                    return Some(self.nav.view_root.join(child.file_name()));
                }
                None
            }
        }
    }

    /// Sync the visual cursor position to the actual view selection state.
    pub(super) fn sync_visual_cursor_to_view(&mut self) {
        if let Some(state) = &self.visual_state {
            match self.layout_mode {
                LayoutMode::Tree => self.nav.tree_state.selected = state.cursor,
                LayoutMode::Miller => self.nav.miller_state.selected = state.cursor,
            }
        }
    }

    /// Apply the visual selection range as marks.
    pub(super) fn apply_visual_selection_as_marks(&mut self) {
        let Some(state) = &self.visual_state else {
            return;
        };

        let (start, end) = state.selection_range();

        // Collect paths first to avoid borrow issues
        let paths_to_mark: Vec<PathBuf> = match self.layout_mode {
            LayoutMode::Tree => {
                // Get flattened tree items
                if let Some((node, root_path)) = self.get_view_root_node() {
                    let items = crate::ui::TreeView::new(
                        node,
                        &root_path,
                        &self.theme,
                        &self.marked,
                        &self.clipboard,
                    )
                    .flatten(&self.nav.tree_state);

                    (start..=end)
                        .filter_map(|i| items.get(i).map(|item| item.path.clone()))
                        .collect()
                } else {
                    Vec::new()
                }
            }
            LayoutMode::Miller => {
                // Index children in their natural (displayed) order. Miller
                // columns render `node.children` directly without re-sorting, so
                // the visual range must use the same order — otherwise marks land
                // on different files than the highlight (e.g. in quick-listed
                // dirs, which are name-sorted while sort_mode is size-based).
                if let Some((node, _)) = self.get_view_root_node() {
                    let view_root = self.nav.view_root.clone();
                    (start..=end)
                        .filter_map(|i| {
                            node.children
                                .get(i)
                                .map(|child| view_root.join(child.file_name()))
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
        };

        for path in paths_to_mark {
            self.mark(path);
        }
    }

    /// Keep the scroll offsets the last frame actually used.
    pub(super) fn adopt_rendered_offsets(&mut self) {
        (self.nav.tree_state.offset, self.nav.miller_state.offset) = self.rendered_offsets.get();
    }

    /// Paths the UI never operates on destructively: the scan root, the
    /// directory being viewed, and any ancestor of either.
    pub(super) fn is_protected(&self, path: &Path) -> bool {
        self.path.starts_with(path) || self.nav.view_root.starts_with(path)
    }

    /// Mark `path` unless it is protected. Returns whether it is now marked.
    pub(super) fn mark(&mut self, path: PathBuf) -> bool {
        if self.is_protected(&path) {
            return false;
        }
        self.marked.insert(path);
        true
    }
}
