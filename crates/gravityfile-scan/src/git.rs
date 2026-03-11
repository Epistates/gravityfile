//! Git repository status detection.
//!
//! This module provides functionality to detect git status for files
//! within a repository. It caches repository state for efficient lookups,
//! and restricts the status query to the scanned subtree so that large
//! monorepos do not force a full-repo status load.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gravityfile_core::GitStatus;

/// Git status cache for efficient lookups.
#[derive(Debug, Default)]
pub struct GitStatusCache {
    /// Map of absolute file paths to their git status.
    statuses: HashMap<PathBuf, GitStatus>,
    /// Root path of the git repository (if found).
    repo_root: Option<PathBuf>,
}

impl GitStatusCache {
    /// Create a new empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Initialize the cache by discovering and scanning the git repository.
    ///
    /// `start_path` is the directory we are scanning (the subtree root). The
    /// git status query is restricted to that subtree via a pathspec so that
    /// only relevant entries are loaded, keeping memory usage proportional to
    /// the scanned directory rather than the whole repository.
    ///
    /// Returns true if a git repository was found and scanned.
    #[cfg(feature = "git")]
    pub fn initialize(&mut self, start_path: &Path) -> bool {
        use git2::{Repository, StatusOptions};

        // Try to find a git repository starting from the given path
        let repo = match Repository::discover(start_path) {
            Ok(repo) => repo,
            Err(_) => return false,
        };

        // Get the workdir (root of the working tree)
        let workdir = match repo.workdir() {
            Some(dir) => dir.to_path_buf(),
            None => return false, // Bare repository
        };

        self.repo_root = Some(workdir.clone());

        // Build a pathspec that restricts status to the scanned subtree.
        // We compute start_path relative to the repo workdir so git2 can
        // apply it as a standard pathspec pattern.
        let pathspec_str = start_path
            .strip_prefix(&workdir)
            .ok()
            .and_then(|rel| rel.to_str())
            .map(|s| {
                if s.is_empty() {
                    // Scanning the repo root — match everything.
                    "**".to_string()
                } else {
                    format!("{s}/**")
                }
            })
            .unwrap_or_else(|| "**".to_string());

        // Get file statuses restricted to the subtree.
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .include_ignored(true)
            .recurse_untracked_dirs(true)
            .include_unmodified(false)
            .pathspec(&pathspec_str);

        let statuses = match repo.statuses(Some(&mut opts)) {
            Ok(s) => s,
            Err(_) => return true, // Repository found but status failed
        };

        // Build the status map
        for entry in statuses.iter() {
            let status = entry.status();
            let path = match entry.path() {
                Some(p) => workdir.join(p),
                None => continue,
            };

            let git_status = if status.is_conflicted() {
                GitStatus::Conflict
            } else if status.is_index_new()
                || status.is_index_modified()
                || status.is_index_deleted()
                || status.is_index_renamed()
                || status.is_index_typechange()
            {
                GitStatus::Staged
            } else if status.is_wt_new() {
                GitStatus::Untracked
            } else if status.is_wt_modified()
                || status.is_wt_deleted()
                || status.is_wt_renamed()
                || status.is_wt_typechange()
            {
                GitStatus::Modified
            } else if status.is_ignored() {
                GitStatus::Ignored
            } else {
                continue; // Skip clean files
            };

            self.statuses.insert(path, git_status);
        }

        true
    }

    /// Initialize without git feature (no-op).
    #[cfg(not(feature = "git"))]
    pub fn initialize(&mut self, _start_path: &Path) -> bool {
        false
    }

    /// Get the git status for a path.
    pub fn get_status(&self, path: &Path) -> Option<GitStatus> {
        self.statuses.get(path).copied()
    }

    /// Check if the path is within a git repository.
    pub fn is_in_repo(&self, path: &Path) -> bool {
        if let Some(ref root) = self.repo_root {
            path.starts_with(root)
        } else {
            false
        }
    }

    /// Get the repository root path.
    pub fn repo_root(&self) -> Option<&Path> {
        self.repo_root.as_deref()
    }

    /// Get the number of cached statuses.
    pub fn len(&self) -> usize {
        self.statuses.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty()
    }
}

/// Apply git statuses to a file tree in-place.
pub fn apply_git_status(tree: &mut gravityfile_core::FileTree) {
    let mut cache = GitStatusCache::new();
    if !cache.initialize(&tree.root_path) {
        return; // Not a git repository
    }

    apply_status_recursive(&mut tree.root, &tree.root_path, &cache);
}

/// Recursively apply git status to a node and its children.
fn apply_status_recursive(
    node: &mut gravityfile_core::FileNode,
    current_path: &Path,
    cache: &GitStatusCache,
) {
    // Apply status to this node
    node.git_status = cache.get_status(current_path);

    // Recursively apply to children
    for child in &mut node.children {
        let child_path = current_path.join(&*child.name);
        apply_status_recursive(child, &child_path, cache);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_cache() {
        let cache = GitStatusCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert!(cache.repo_root().is_none());
    }

    #[test]
    fn test_get_status_nonexistent() {
        let cache = GitStatusCache::new();
        assert!(cache.get_status(Path::new("/nonexistent")).is_none());
    }
}
