//! Moving between directories.
//!
//! Every change of the viewed directory goes through [`App::enter`],
//! [`App::go_back`], [`App::go_to`] or [`App::place`]. They keep the tab's
//! history, every layout's selection and the state derived from the view in
//! step, so no caller has to remember which caches to refresh.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gravityfile_core::FileNode;

use super::state::{LayoutMode, View, Visit};
use super::{App, quick_list_with_settings};
use crate::ui::{TreeState, TreeView};

impl App {
    /// Go into `dir`, recording the current place so going back returns to
    /// it. Selects `select` there; without one, entering a place that was
    /// left by going back resumes it, and anywhere else starts at the top.
    pub(super) fn enter(&mut self, dir: PathBuf, select: Option<PathBuf>) {
        let origin = self.current_visit();
        self.enter_from(origin, dir, select);
    }

    /// Go to `dir` from anywhere: a bookmark, a typed path or a search.
    /// A directory outside the current tree is listed, and scanned when
    /// scanning on startup is enabled, replacing the tree.
    pub(super) fn go_to(&mut self, dir: PathBuf) {
        let origin = self.current_visit();
        if let Some(dir) = self.reach(&dir, true) {
            self.enter_from(origin, dir, None);
        }
    }

    fn enter_from(&mut self, origin: Visit, dir: PathBuf, select: Option<PathBuf>) {
        if dir == origin.dir {
            if select.is_some() {
                self.place(dir, select);
            }
            return;
        }
        self.load_children(&dir);
        let resumed = match self.nav.forward.iter().rposition(|visit| visit.dir == dir) {
            Some(index) => {
                let visit = self.nav.forward.remove(index);
                self.nav.forward.truncate(index);
                Some(visit)
            }
            None => {
                self.nav.forward.clear();
                None
            }
        };
        self.nav.back.push(origin);
        let select = select.or_else(|| {
            let visit = resumed?;
            self.nav.tree_state.expanded = visit.expanded;
            visit.selected
        });
        self.place(dir, select);
    }

    /// Return to the previous place. With no history, go up to the parent
    /// directory, beyond the scan root if need be, selecting the directory
    /// just left.
    pub(super) fn go_back(&mut self) {
        let current = self.current_visit();
        if let Some(visit) = self.nav.back.last().cloned() {
            let Some(dir) = self.reach(&visit.dir, false) else {
                return;
            };
            self.nav.back.pop();
            self.nav.forward.push(current);
            self.nav.tree_state.expanded = visit.expanded;
            self.place(dir, visit.selected);
            return;
        }
        let Some(parent) = self.nav.view_root.parent().map(Path::to_path_buf) else {
            return;
        };
        let left = self.nav.view_root.clone();
        let Some(parent) = self.reach(&parent, false) else {
            return;
        };
        self.nav.tree_state.collapse(&left);
        self.nav.forward.push(current);
        self.place(parent, Some(left));
    }

    /// Return to the scan root, forgetting the history.
    pub(super) fn go_to_root(&mut self) {
        self.nav.back.clear();
        self.nav.forward.clear();
        self.place(self.path.clone(), None);
    }

    /// Enter the selected directory in the active layout or the treemap.
    pub(super) fn enter_selected(&mut self) {
        let Some(path) = self.get_selected_path() else {
            return;
        };
        let is_dir = self
            .tree
            .as_ref()
            .and_then(|tree| tree.find(&path))
            .is_some_and(FileNode::is_dir);
        if !is_dir {
            return;
        }
        if path == self.nav.view_root {
            self.nav.tree_state.expand(&path);
            self.update_cached_tree_len();
            return;
        }
        self.enter(path, None);
    }

    /// Go to a search result: into a directory, or to a file's directory
    /// with the file selected.
    pub(super) fn navigate_to_search_result(&mut self, target: &Path) {
        let is_dir = self
            .tree
            .as_ref()
            .and_then(|tree| tree.find(target))
            .is_some_and(FileNode::is_dir);
        if is_dir {
            self.enter(target.to_path_buf(), None);
        } else if let Some(parent) = target.parent() {
            self.enter(parent.to_path_buf(), Some(target.to_path_buf()));
        }
    }

    /// Switch between the tree and Miller layouts, keeping the selected
    /// entry selected. Miller lists one directory's children, so an entry
    /// deeper in the tree moves the view to its directory.
    pub(super) fn toggle_layout(&mut self) {
        let selected = self.get_selected_path();
        self.layout_mode = self.layout_mode.toggle();
        if self.layout_mode == LayoutMode::Miller
            && let Some(parent) = selected.as_deref().and_then(Path::parent)
            && parent != self.nav.view_root
            && parent.starts_with(&self.nav.view_root)
        {
            self.nav.view_root = parent.to_path_buf();
        }
        self.select_entry(selected.as_deref());
        self.view_changed();
    }

    /// Bring everything up to date after another tab became active. A tab
    /// whose directory has since been removed moves to its nearest surviving
    /// ancestor; one outside the current scan has its directory listed and
    /// scanned, as going there would.
    pub(super) fn after_tab_switch(&mut self) {
        let outside_scan = self
            .tree
            .as_ref()
            .is_some_and(|tree| !self.nav.view_root.starts_with(&tree.root_path));
        if outside_scan {
            let dir = self.nav.view_root.clone();
            if let Some(dir) = self.reach(&dir, true) {
                self.place(dir, None);
            }
        } else {
            self.leave_vanished_view();
        }
        self.view_changed();
    }

    /// Move off a viewed directory that no longer exists in the tree, to its
    /// nearest surviving ancestor.
    pub(super) fn leave_vanished_view(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        let mut view = self.nav.view_root.clone();
        while !tree.find(&view).is_some_and(FileNode::is_dir) {
            if !view.pop() || !view.starts_with(&tree.root_path) {
                view = tree.root_path.clone();
                break;
            }
        }
        if view != self.nav.view_root {
            self.place(view, None);
        }
    }

    /// Show `dir` without touching the history, selecting `select` there or
    /// the first entry.
    pub(super) fn place(&mut self, dir: PathBuf, select: Option<PathBuf>) {
        self.nav.view_root = dir;
        self.nav.tree_state.expand(&self.nav.view_root);
        self.select_entry(select.as_deref());
        self.view_changed();
    }

    /// The current place, to come back to.
    fn current_visit(&self) -> Visit {
        let selected = matches!(self.view, View::Explorer | View::Treemap)
            .then(|| self.get_selected_path())
            .flatten();
        Visit {
            dir: self.nav.view_root.clone(),
            selected,
            expanded: self.nav.tree_state.expanded.clone(),
        }
    }

    /// Select `target` in every layout, or the first entry where it is not
    /// shown. The tree selects its row; Miller and the treemap select the
    /// viewed directory's child that is or contains it.
    pub(super) fn select_entry(&mut self, target: Option<&Path>) {
        let (row, child, area) = match (self.get_view_root_node(), target) {
            (Some((node, root)), Some(target)) => {
                let row = TreeView::new(node, &root, &self.theme, &self.marked, &self.clipboard)
                    .position(&self.nav.tree_state, target);
                let name = target
                    .strip_prefix(&root)
                    .ok()
                    .and_then(|rest| rest.components().next())
                    .map(|component| component.as_os_str());
                let child = name.and_then(|name| {
                    node.children
                        .iter()
                        .position(|child| child.file_name() == name)
                });
                let area = name.and_then(|name| {
                    Self::treemap_order(node)
                        .iter()
                        .position(|child| child.file_name() == name)
                });
                (row, child, area)
            }
            _ => (None, None, None),
        };
        self.nav.tree_state.selected = row.unwrap_or(0);
        self.nav.tree_state.offset = 0;
        self.nav.miller_state.selected = child.unwrap_or(0);
        self.nav.miller_state.offset = 0;
        self.nav.treemap_state.selected = area.unwrap_or(0);
    }

    /// Bring what depends on the view up to date: row counts, selections
    /// kept in range and on screen, the Miller parent column and the preview.
    pub(super) fn view_changed(&mut self) {
        self.nav.tree_state.expand(&self.nav.view_root);
        self.update_cached_tree_len();
        self.update_cached_miller_len();
        self.update_cached_treemap_len();
        let clamp =
            |selected: &mut usize, len: usize| *selected = (*selected).min(len.saturating_sub(1));
        clamp(&mut self.nav.tree_state.selected, self.cached_tree_len);
        clamp(&mut self.nav.miller_state.selected, self.cached_miller_len);
        clamp(
            &mut self.nav.treemap_state.selected,
            self.cached_treemap_len,
        );
        self.nav.tree_state.ensure_visible(self.viewport_height);
        self.nav.miller_state.ensure_visible(self.viewport_height);
        if self.layout_mode == LayoutMode::Miller {
            self.update_cached_parent();
        }
        self.update_preview();
        self.needs_redraw = true;
    }

    /// The treemap's order: largest first, then by name.
    pub(super) fn treemap_order(node: &FileNode) -> Vec<&FileNode> {
        let mut children: Vec<&FileNode> = node.children.iter().collect();
        children.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));
        children
    }

    /// Load `dir`'s entries if the tree has it without them, as a quick
    /// listing does for directories it has not opened yet.
    pub(super) fn load_children(&mut self, dir: &Path) {
        // A snapshot is the whole record; the filesystem is not consulted.
        if self.snapshot.is_some() {
            return;
        }
        let unloaded = self
            .tree
            .as_ref()
            .and_then(|tree| tree.find(dir))
            .is_some_and(|node| node.is_dir() && node.children.is_empty());
        if unloaded {
            self.lazy_load_directory(dir);
        }
    }

    /// Make sure `dir` is in the tree. When it is not, the tree is replaced
    /// with a listing of `dir`, keeping a finished scan so its sizes show
    /// when coming back, and `scan` starts scanning it if scanning on
    /// startup is enabled. Returns the directory to view, or `None` when it
    /// cannot be listed, in which case nothing changes.
    fn reach(&mut self, dir: &Path, scan: bool) -> Option<PathBuf> {
        let loaded = self
            .tree
            .as_ref()
            .and_then(|tree| tree.find(dir))
            .is_some_and(FileNode::is_dir);
        if loaded {
            return Some(dir.to_path_buf());
        }
        if self.snapshot.is_some() {
            self.operation_message = Some((
                false,
                format!(
                    "{} is outside the snapshot",
                    gravityfile_core::escape_filename(dir.as_os_str())
                ),
            ));
            return None;
        }
        let mut listing = self.load_directory_listing(dir)?;
        if let Some(tree) = &self.tree
            && self.has_full_scan
            && let Some(cache) = &mut self.scanned_cache
        {
            cache.insert(tree.root_path.clone(), Arc::clone(tree));
        }
        let root = listing.root_path.clone();
        self.merge_cached_scans_into_tree(&mut listing.root, &root);
        self.cancel_scan();
        self.path = root.clone();
        self.install_tree(listing);
        self.nav.tree_state = TreeState::new(root.clone());
        self.duplicates = None;
        self.age_report = None;
        self.has_full_scan = false;
        if scan && self.scan_on_startup {
            self.start_scan();
        }
        Some(root)
    }

    /// Lazy load a directory's children using quick_list.
    /// This updates the tree in-place with the new children.
    pub(super) fn lazy_load_directory(&mut self, path: &Path) {
        // Captured up front: the mutable borrow taken by `Arc::make_mut` below
        // rules out reading the tree's own root path at the same time.
        let Some(root_path) = self.tree.as_ref().map(|tree| tree.root_path.clone()) else {
            return;
        };
        let sort_mode = self.sort_mode;

        // First check if we have cached scan data for this directory
        if let Some(cached_tree) = self
            .scanned_cache
            .as_ref()
            .and_then(|c| c.get(path))
            .cloned()
        {
            // Use cached scan data - it has full size/structure information
            if let Some(tree) = &mut self.tree
                && let Some(node) =
                    Self::find_node_mut(&mut Arc::make_mut(tree).root, path, &root_path)
            {
                // Replace with the cached node (preserves all scan data)
                *node = cached_tree.root.clone();
                Self::order_new_subtree(node, sort_mode);
            }
            return;
        }

        // Quick load directory contents for immediate display
        // When the full scan completes, it will replace this with accurate data
        if let Some(mut quick_tree) = self.load_directory_listing(path) {
            // Find the node in our tree and update its children
            if let Some(tree) = &mut self.tree
                && let Some(node) =
                    Self::find_node_mut(&mut Arc::make_mut(tree).root, path, &root_path)
            {
                node.children = std::mem::take(&mut quick_tree.root.children);
                Self::order_new_subtree(node, sort_mode);
            }
        }
    }

    pub(super) fn load_directory_listing(
        &mut self,
        path: &Path,
    ) -> Option<gravityfile_core::FileTree> {
        match quick_list_with_settings(path, &self.user_settings, self.apparent_size) {
            Ok(tree) => Some(tree),
            Err(error) => {
                self.error = Some(format!("Cannot list {}: {error}", path.display()));
                self.needs_redraw = true;
                None
            }
        }
    }

    /// Find a mutable reference to a node by path.
    pub(super) fn find_node_mut<'a>(
        node: &'a mut gravityfile_core::FileNode,
        target: &Path,
        current_path: &Path,
    ) -> Option<&'a mut gravityfile_core::FileNode> {
        let relative = target.strip_prefix(current_path).ok()?;
        let mut found = node;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            found = found
                .children
                .iter_mut()
                .find(|child| child.file_name() == name)?;
        }
        Some(found)
    }

    /// Merge cached scan data into a tree node's children.
    /// For each child directory in the node, if we have cached scan data,
    /// replace the placeholder child with the fully scanned version.
    pub(super) fn merge_cached_scans_into_tree(
        &self,
        node: &mut gravityfile_core::FileNode,
        node_path: &Path,
    ) {
        for child in &mut node.children {
            if child.is_dir() {
                let child_path = node_path.join(child.file_name());
                if let Some(cached_tree) =
                    self.scanned_cache.as_ref().and_then(|c| c.get(&child_path))
                {
                    // Replace this child with the cached scan data
                    *child = cached_tree.root.clone();
                }
            }
        }
    }

    /// Update the cached parent tree for Miller columns display.
    /// Called when navigating to ensure the parent column can be shown.
    pub(super) fn update_cached_parent(&mut self) {
        // Never retain a previous location's parent when refresh fails.
        self.cached_parent_tree = None;
        let Some(tree) = &self.tree else { return };
        if self.snapshot.is_some() {
            return;
        }

        // Only need to cache parent if we're at or near the tree root
        if self.nav.view_root == tree.root_path {
            if let Some(parent_path) = self.nav.view_root.parent() {
                let parent_path_buf = parent_path.to_path_buf();
                // Load the parent directory
                if let Ok(mut parent_tree) = quick_list_with_settings(
                    &parent_path_buf,
                    &self.user_settings,
                    self.apparent_size,
                ) {
                    // Merge any cached scans into the parent tree's children
                    // This ensures previously scanned directories show their data
                    self.merge_cached_scans_into_tree(&mut parent_tree.root, &parent_path_buf);
                    self.cached_parent_tree = Some(parent_tree);
                }
            }
        } else {
            // Don't need cached parent when navigating within the tree
            self.cached_parent_tree = None;
        }
    }

    /// Change directory within the loaded scan tree. Leading `/` is scan-root relative.
    pub(super) fn cd_to_path(&mut self, target: &str) {
        if target == "/" || target == "~" {
            self.go_to_root();
            return;
        }
        if target == ".." {
            self.go_back();
            return;
        }
        let Some(tree) = &self.tree else {
            self.error = Some("Cannot change directory: no directory tree is loaded".into());
            return;
        };
        let mut target_path = if target.starts_with('/') {
            self.path.clone()
        } else {
            self.nav.view_root.clone()
        };
        for component in Path::new(target.trim_start_matches('/')).components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::Normal(name) => target_path.push(name),
                std::path::Component::ParentDir => {
                    if target_path == self.path || !target_path.starts_with(&self.path) {
                        self.error =
                            Some("Cannot change directory: path leaves the scan root".into());
                        return;
                    }
                    target_path.pop();
                }
                _ => {
                    self.error = Some("Cannot change directory: unsupported path prefix".into());
                    return;
                }
            }
        }
        let Some(node) = Self::find_node_at_path(&tree.root, &target_path, &tree.root_path) else {
            self.error = Some(format!(
                "Directory is not in the loaded tree: {}",
                target_path.display()
            ));
            return;
        };
        if !node.is_dir() {
            self.error = Some(format!("Not a directory: {}", target_path.display()));
            return;
        }
        self.enter(target_path, None);
    }
}
