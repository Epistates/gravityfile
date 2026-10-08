//! Fuzzy search over the scanned tree.

use std::path::PathBuf;

use super::App;
use super::state::AppMode;

impl App {
    /// Start fuzzy search mode.
    pub(super) fn start_search(&mut self) {
        // Populate search paths from the current tree
        self.search_state.activate();
        self.update_search_paths();
        self.mode = AppMode::Search;
    }

    /// Update search paths from the selected subtree, preserving preorder ranking.
    pub(super) fn update_search_paths(&mut self) {
        let tree = self.tree.clone();
        let view_root = self.nav.view_root.clone();
        self.search_state.build_paths(move |cancelled| {
            tree.as_ref().map_or_else(Vec::new, |tree| {
                Self::collect_search_paths(tree, &view_root, cancelled)
            })
        });
    }

    pub(super) fn collect_search_paths(
        tree: &gravityfile_core::FileTree,
        view_root: &std::path::Path,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Vec<(PathBuf, String, bool)> {
        let Some(root) =
            Self::find_node_at_path_while(&tree.root, view_root, &tree.root_path, || {
                !cancelled.load(std::sync::atomic::Ordering::Relaxed)
            })
        else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        let mut current_path = view_root.to_path_buf();
        paths.push((current_path.clone(), ".".into(), root.is_dir()));
        // One iterator per depth avoids recursive calls and a width-sized stack
        // of allocated paths for directories with millions of children.
        let mut stack = vec![root.children.iter()];
        while let Some(children) = stack.last_mut() {
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return Vec::new();
            }
            if let Some(child) = children.next() {
                current_path.push(child.file_name());
                let display = gravityfile_core::escape_filename(
                    current_path
                        .strip_prefix(view_root)
                        .expect("search traversal stays inside its selected subtree")
                        .as_os_str(),
                )
                .into_owned();
                paths.push((current_path.clone(), display, child.is_dir()));
                stack.push(child.children.iter());
            } else {
                stack.pop();
                current_path.pop();
            }
        }
        paths
    }

    /// Handle key input during search mode.
    pub(super) fn handle_search_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        match (key.code, key.modifiers) {
            // Cancel search
            (KeyCode::Esc, _) => {
                self.search_state.deactivate();
                self.mode = AppMode::Normal;
            }
            // Execute search (navigate to selected)
            (KeyCode::Enter, _) => {
                if self.search_state.is_searching() {
                    return;
                }
                if let Some(result) = self.search_state.selected_result() {
                    let target_path = result.path.clone();
                    self.navigate_to_search_result(&target_path);
                }
                self.search_state.deactivate();
                self.mode = AppMode::Normal;
            }
            // Navigation in results
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.search_state.move_up();
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.search_state.move_down();
            }
            // Cycle search mode
            (KeyCode::Tab, _) => {
                self.search_state.cycle_mode();
            }
            // Cursor movement
            (KeyCode::Left, _) => {
                self.search_state.move_cursor_left();
            }
            (KeyCode::Right, _) => {
                self.search_state.move_cursor_right();
            }
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.search_state.move_cursor_start();
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.search_state.move_cursor_end();
            }
            // Delete
            (KeyCode::Backspace, _) => {
                self.search_state.delete_char_before();
            }
            (KeyCode::Delete, _) => {
                self.search_state.delete_char_at();
            }
            // Clear line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.search_state.clear_query();
            }
            // Type character
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.search_state.insert_char(c);
            }
            _ => {}
        }
    }
}
