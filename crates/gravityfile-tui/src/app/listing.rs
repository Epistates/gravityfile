//! The tree as the explorer lists it: finding nodes, sorting, and the
//! row counts each layout navigates over.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gravityfile_core::FileNode;

use super::App;
use super::state::{LayoutMode, SortMode, View};
use crate::ui::TreeView;

impl App {
    /// Update cached tree item count.
    pub(super) fn update_cached_tree_len(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            // Use flatten_count to avoid allocating a full Vec just for the length.
            self.cached_tree_len =
                TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                    .flatten_count(&self.nav.tree_state);
        } else {
            self.cached_tree_len = 0;
        }
    }

    /// Update cached miller entry count.
    pub(super) fn update_cached_miller_len(&mut self) {
        if let Some((node, _)) = self.get_view_root_node() {
            self.cached_miller_len = node.children.len();
        } else {
            self.cached_miller_len = 0;
        }
    }

    /// Update cached treemap rectangle count.
    pub(super) fn update_cached_treemap_len(&mut self) {
        if let Some((node, _)) = self.get_view_root_node() {
            self.cached_treemap_len = node.children.len();
        } else {
            self.cached_treemap_len = 0;
        }
    }

    /// Get the selected treemap item (child node) if any.
    pub(super) fn get_selected_treemap_child(&self) -> Option<(&FileNode, PathBuf)> {
        let (node, root_path) = self.get_view_root_node()?;

        // Sort children by size descending (same order as treemap layout)
        let mut children: Vec<&FileNode> = node.children.iter().collect();
        children.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));

        let child = children.get(self.nav.treemap_state.selected)?;
        let path = root_path.join(child.file_name());
        Some((*child, path))
    }

    /// Find a node by path in the tree.
    pub(super) fn find_node_at_path<'a>(
        node: &'a gravityfile_core::FileNode,
        target_path: &Path,
        current_path: &Path,
    ) -> Option<&'a gravityfile_core::FileNode> {
        Self::find_node_at_path_while(node, target_path, current_path, || true)
    }

    pub(super) fn find_node_at_path_while<'a>(
        node: &'a gravityfile_core::FileNode,
        target_path: &Path,
        current_path: &Path,
        mut keep_going: impl FnMut() -> bool,
    ) -> Option<&'a gravityfile_core::FileNode> {
        if !keep_going() {
            return None;
        }
        let relative = target_path.strip_prefix(current_path).ok()?;
        let mut found = node;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            let mut matched = None;
            for child in &found.children {
                if !keep_going() {
                    return None;
                }
                if child.file_name() == name {
                    matched = Some(child);
                    break;
                }
            }
            found = matched?;
        }
        Some(found)
    }

    /// Get the current view root node.
    pub(super) fn get_view_root_node(&self) -> Option<(&gravityfile_core::FileNode, PathBuf)> {
        let tree = self.tree.as_ref()?;
        let node = Self::find_node_at_path(&tree.root, &self.nav.view_root, &tree.root_path)?;
        Some((node, self.nav.view_root.clone()))
    }

    /// Get the parent node of the current view root (for Miller columns).
    pub(super) fn get_parent_node(&self) -> Option<&gravityfile_core::FileNode> {
        let tree = self.tree.as_ref()?;
        let parent_path = self.nav.view_root.parent()?.to_path_buf();

        // First, check if parent is within the main tree
        if parent_path.starts_with(&tree.root_path) || parent_path == tree.root_path {
            return Self::find_node_at_path(&tree.root, &parent_path, &tree.root_path);
        }

        // If we're at the tree root and have a cached parent, use that
        if self.nav.view_root == tree.root_path
            && let Some(cached) = &self.cached_parent_tree
        {
            return Some(&cached.root);
        }

        None
    }

    /// Get the current directory name for highlighting in parent column.
    pub(super) fn get_current_dir_name(&self) -> Option<String> {
        self.nav
            .view_root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }

    /// Cycle to the next sort mode and apply sorting.
    pub(super) fn cycle_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.apply_sort();
    }

    /// Reverse the current sort direction.
    pub(super) fn reverse_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.reverse();
        self.apply_sort();
    }

    /// Display `tree`, ordered by the current sort mode. Every new tree goes
    /// through here so a chosen order survives rescans and navigation.
    pub(super) fn install_tree(&mut self, mut tree: gravityfile_core::FileTree) {
        Self::order_new_subtree(&mut tree.root, self.sort_mode);
        self.tree = Some(Arc::new(tree));
    }

    /// Order a subtree that just arrived from a scan, listing or cache.
    pub(super) fn order_new_subtree(node: &mut gravityfile_core::FileNode, mode: SortMode) {
        // Scans and listings already arrive in the default order.
        if mode != SortMode::SizeDescending {
            Self::sort_node_tree(node, mode);
        }
    }

    /// Switch every size between disk usage and file lengths. The tree keeps
    /// both measures, so this re-totals it in place without rescanning, and
    /// later scans and listings use the same measure.
    pub(super) fn toggle_size_mode(&mut self) {
        self.apparent_size = !self.apparent_size;
        let apparent = self.apparent_size;
        let remeasure = |tree: &mut gravityfile_core::FileTree| {
            let count_hard_links = tree.config.count_hard_links;
            tree.remeasure(apparent, count_hard_links);
        };
        let selected = self.get_selected_path();
        if let Some(tree) = &mut self.tree {
            remeasure(Arc::make_mut(tree));
        }
        for tree in self
            .scanned_cache
            .iter_mut()
            .flat_map(|cache| cache.values_mut())
        {
            remeasure(Arc::make_mut(tree));
        }
        if let Some(parent) = &mut self.cached_parent_tree {
            remeasure(parent);
        }
        self.apply_sort();
        self.select_entry(selected.as_deref());
        self.view_changed();
        let measure = if apparent {
            "apparent sizes"
        } else {
            "disk usage"
        };
        self.operation_message = Some((true, format!("Showing {measure}")));
    }

    /// Apply the current sort mode to the file tree.
    pub(super) fn apply_sort(&mut self) {
        if let Some(ref mut tree) = self.tree {
            Self::sort_node_tree(&mut Arc::make_mut(tree).root, self.sort_mode);
            // Update cached lengths after sorting
            match self.layout_mode {
                LayoutMode::Tree => self.update_cached_tree_len(),
                LayoutMode::Miller => self.update_cached_miller_len(),
            }
            if self.view == View::Treemap {
                self.update_cached_treemap_len();
            }
        }
    }

    /// Sort a tree using one child iterator per depth instead of recursive calls.
    pub(super) fn sort_node_tree(node: &mut gravityfile_core::FileNode, mode: SortMode) {
        let compare = |a: &gravityfile_core::FileNode, b: &gravityfile_core::FileNode| {
            (match mode {
                SortMode::SizeDescending => b.size.cmp(&a.size),
                SortMode::SizeAscending => a.size.cmp(&b.size),
                SortMode::NameAscending => a.compare_name(b),
                SortMode::NameDescending => b.compare_name(a),
                SortMode::ModifiedDescending => b.timestamps.modified.cmp(&a.timestamps.modified),
                SortMode::ModifiedAscending => a.timestamps.modified.cmp(&b.timestamps.modified),
                SortMode::CountDescending => {
                    let a_count = a.children.len();
                    let b_count = b.children.len();
                    b_count.cmp(&a_count)
                }
                SortMode::CountAscending => {
                    let a_count = a.children.len();
                    let b_count = b.children.len();
                    a_count.cmp(&b_count)
                }
            })
            .then_with(|| a.compare_name(b))
        };
        node.children.sort_by(compare);
        let mut stack = vec![node.children.iter_mut()];
        while let Some(children) = stack.last_mut() {
            let Some(child) = children.next() else {
                stack.pop();
                continue;
            };
            child.children.sort_by(compare);
            stack.push(child.children.iter_mut());
        }
    }
}
