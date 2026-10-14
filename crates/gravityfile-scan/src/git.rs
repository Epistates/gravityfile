//! Git repository status detection.
//!
//! This module provides functionality to detect git status for files
//! within a repository. It caches repository state for efficient lookups,
//! and restricts the status query to the scanned subtree so that large
//! monorepos do not force a full-repo status load.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gravityfile_core::GitStatus;
use tokio_util::sync::CancellationToken;

/// Git status cache for efficient lookups.
#[derive(Debug, Default)]
pub struct GitStatusCache {
    /// Map of absolute file paths to their git status.
    statuses: HashMap<PathBuf, GitStatus>,
    /// Ignored directories represent all untracked descendants without enumeration.
    ignored_directories: HashSet<PathBuf>,
    /// Clean indexed files beneath ignored directories must not inherit Ignored.
    clean_tracked: HashSet<PathBuf>,
    scope_root: Option<PathBuf>,
    /// Root path of the git repository (if found).
    repo_root: Option<PathBuf>,
    /// Most recent initialization failure, distinct from no repository/clean state.
    initialization_error: Option<String>,
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
    /// Returns true if a worktree was found, even if querying its status failed.
    /// Inspect [`initialization_error`](Self::initialization_error) for failures.
    #[cfg(feature = "git")]
    pub fn initialize(&mut self, start_path: &Path) -> bool {
        self.initialize_cancellable(start_path, &CancellationToken::new())
    }

    #[cfg(feature = "git")]
    fn initialize_cancellable(&mut self, start_path: &Path, token: &CancellationToken) -> bool {
        use git2::{Repository, StatusOptions};

        // A cache may be reused for a different directory. Never leak stale
        // statuses or an old repository root into the next initialization.
        // Drop the table too: clear() keeps the previous query's allocation
        // even when the next subtree is tiny or initialization is cancelled.
        self.statuses = HashMap::new();
        self.ignored_directories = HashSet::new();
        self.clean_tracked = HashSet::new();
        self.scope_root = None;
        self.repo_root = None;
        self.initialization_error = None;
        if token.is_cancelled() {
            return false;
        }

        // Try to find a git repository starting from the given path
        let repo = match Repository::discover(start_path) {
            Ok(repo) => repo,
            Err(error) => {
                if error.code() != git2::ErrorCode::NotFound {
                    self.initialization_error = Some(error.to_string());
                }
                return false;
            }
        };

        // The working tree's root, in the same canonical form as the start
        // path and every lookup: libgit2 keeps the form it was given, which on
        // Windows may be a short (8.3) name or use forward slashes.
        let workdir = match repo.workdir().map(dunce::canonicalize) {
            Some(Ok(dir)) => dir,
            Some(Err(error)) => {
                self.initialization_error = Some(error.to_string());
                return false;
            }
            None => return false, // Bare repository
        };
        let normalized_start = match dunce::canonicalize(start_path) {
            Ok(path) => path,
            Err(error) => {
                self.initialization_error = Some(error.to_string());
                return false;
            }
        };
        let relative = match normalized_start.strip_prefix(&workdir) {
            Ok(path) => path,
            Err(error) => {
                self.initialization_error = Some(error.to_string());
                return false;
            }
        };
        self.repo_root = Some(workdir.clone());
        self.scope_root = Some(normalized_start.clone());

        // Treat subtree names literally (including brackets and wildcard bytes).
        // An empty pathspec list selects the repository root; failed path
        // normalization must never silently widen a subtree query to the repo.
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .include_ignored(true)
            .recurse_ignored_dirs(false)
            .recurse_untracked_dirs(true)
            // Clean tracked files must override inherited ignored-directory
            // status. This visits index entries, not every ignored build file.
            .include_unmodified(true)
            .disable_pathspec_match(true);
        if !relative.as_os_str().is_empty() {
            opts.pathspec(relative);
        }

        if token.is_cancelled() {
            return false;
        }
        let statuses = match repo.statuses(Some(&mut opts)) {
            Ok(s) => s,
            Err(error) => {
                self.initialization_error = Some(error.to_string());
                return true;
            }
        };

        // Build the status map
        for entry in statuses.iter() {
            if token.is_cancelled() {
                self.statuses = HashMap::new();
                self.ignored_directories = HashSet::new();
                self.clean_tracked = HashSet::new();
                return false;
            }
            let status = entry.status();
            #[cfg(unix)]
            let path = {
                use std::os::unix::ffi::OsStrExt;
                workdir.join(std::ffi::OsStr::from_bytes(entry.path_bytes()))
            };
            #[cfg(not(unix))]
            let path = match self.utf8_status_path(&workdir, entry.path_bytes()) {
                Some(path) => path,
                None => return true, // Worktree found, but enrichment is incomplete.
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
                self.clean_tracked.insert(path);
                continue; // Skip clean files
            };

            if git_status == GitStatus::Ignored && entry.path_bytes().last() == Some(&b'/') {
                self.ignored_directories.insert(path.clone());
            }
            self.statuses.insert(path, git_status);
        }

        self.compact_clean_tracked(&|| token.is_cancelled())
    }

    #[cfg(any(feature = "git", test))]
    fn compact_clean_tracked(&mut self, cancelled: &impl Fn() -> bool) -> bool {
        // Move retained paths into a fresh set so discarded index entries do
        // not leave their capacity behind. Check between paths rather than
        // using retain/shrink_to_fit, which cannot stop midway through the set.
        let candidates = std::mem::take(&mut self.clean_tracked);
        for path in candidates {
            if cancelled() {
                self.statuses = HashMap::new();
                self.ignored_directories = HashSet::new();
                self.clean_tracked = HashSet::new();
                return false;
            }
            if path
                .ancestors()
                .skip(1)
                .any(|ancestor| self.ignored_directories.contains(ancestor))
            {
                self.clean_tracked.insert(path);
            }
        }
        // Includes empty caches and cancellation while processing the last path.
        if cancelled() {
            self.statuses = HashMap::new();
            self.ignored_directories = HashSet::new();
            self.clean_tracked = HashSet::new();
            return false;
        }
        true
    }

    /// Initialize without git feature (no-op).
    #[cfg(not(feature = "git"))]
    pub fn initialize(&mut self, _start_path: &Path) -> bool {
        false
    }

    #[cfg(not(feature = "git"))]
    fn initialize_cancellable(&mut self, _start_path: &Path, _token: &CancellationToken) -> bool {
        false
    }

    /// Get the git status for a path.
    ///
    /// Falls back to canonicalizing the path so callers can pass a
    /// non-canonical form (a `/var` path on macOS, say, where the repository
    /// workdir resolves to `/private/var`). That fallback costs a syscall, so
    /// bulk callers should use [`get_status_canonical`](Self::get_status_canonical).
    pub fn get_status(&self, path: &Path) -> Option<GitStatus> {
        self.get_status_canonical(path).or_else(|| {
            dunce::canonicalize(path)
                .ok()
                .and_then(|normalized| self.get_status_canonical(&normalized))
        })
    }

    /// Non-Unix paths must decode before they can be represented by PathBuf.
    /// Keep this boundary testable on Unix without pretending to run Windows.
    #[cfg(any(all(feature = "git", not(unix)), test))]
    fn utf8_status_path(&mut self, workdir: &Path, bytes: &[u8]) -> Option<PathBuf> {
        match std::str::from_utf8(bytes) {
            Ok(path) => Some(workdir.join(path)),
            Err(error) => {
                self.statuses = HashMap::new();
                self.ignored_directories = HashSet::new();
                self.clean_tracked = HashSet::new();
                self.initialization_error = Some(format!("Cannot decode Git status path: {error}"));
                None
            }
        }
    }

    /// Look up a path that is already known to be canonical.
    ///
    /// Unlike [`get_status`](Self::get_status) this never touches the
    /// filesystem, which matters when annotating a whole tree: clean files miss
    /// the map, and the canonicalizing fallback would then cost one syscall per
    /// node across the entire scan.
    pub fn get_status_canonical(&self, path: &Path) -> Option<GitStatus> {
        if self
            .scope_root
            .as_ref()
            .is_some_and(|scope| !path.starts_with(scope))
        {
            return None;
        }
        if let Some(status) = self.statuses.get(path) {
            return Some(*status);
        }
        if self.clean_tracked.contains(path) {
            return None;
        }
        path.ancestors()
            .skip(1)
            .any(|ancestor| self.ignored_directories.contains(ancestor))
            .then_some(GitStatus::Ignored)
    }

    /// Check if the path is within a git repository.
    pub fn is_in_repo(&self, path: &Path) -> bool {
        if let Some(ref root) = self.repo_root {
            path.starts_with(root)
                || dunce::canonicalize(path).is_ok_and(|normalized| normalized.starts_with(root))
        } else {
            false
        }
    }

    /// Get the repository root path.
    pub fn repo_root(&self) -> Option<&Path> {
        self.repo_root.as_deref()
    }

    /// Failure from the last initialization; absence of a repository is not an error.
    pub fn initialization_error(&self) -> Option<&str> {
        self.initialization_error.as_deref()
    }

    /// Get the number of explicit cached statuses. An ignored directory may
    /// represent many descendants; clean tracked exceptions are not counted.
    pub fn len(&self) -> usize {
        self.statuses.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty()
    }
}

/// Refresh git statuses in-place, clearing stale annotations on reused trees.
pub fn apply_git_status(tree: &mut gravityfile_core::FileTree) {
    annotate_git_status(tree, &CancellationToken::new(), true);
}

/// Scan-owned annotation may stop partway; the scanner discards the tree on cancellation.
#[cfg(feature = "git")]
pub(crate) fn apply_git_status_cancellable(
    tree: &mut gravityfile_core::FileTree,
    token: &CancellationToken,
) {
    // Scanner-owned nodes start without annotations. A clean repository needs
    // no extra full-tree walk in this path.
    annotate_git_status(tree, token, false);
}

fn annotate_git_status(
    tree: &mut gravityfile_core::FileTree,
    token: &CancellationToken,
    clear_stale: bool,
) {
    let mut cache = GitStatusCache::new();
    cache.initialize_cancellable(&tree.root_path, token);
    if token.is_cancelled() {
        return;
    }
    if let Some(error) = cache.initialization_error() {
        let warning = gravityfile_core::ScanWarning::new(
            tree.root_path.clone(),
            gravityfile_core::WarningKind::ReadError,
            format!("Git status unavailable: {error}"),
        );
        crate::scanner::append_scan_warning(tree, warning);
        return; // Do not clear old annotations as if a failed refresh were clean.
    }
    if cache.is_empty() && !clear_stale {
        return;
    }

    // Resolve the root once. Every child path is built by joining onto it, so
    // the whole walk can then use the syscall-free canonical lookup.
    let root = dunce::canonicalize(&tree.root_path).unwrap_or_else(|_| tree.root_path.clone());

    apply_status_iterative(&mut tree.root, &root, &cache, &|| token.is_cancelled());
}

/// Apply statuses without consuming call stack proportional to tree depth.
fn apply_status_iterative(
    node: &mut gravityfile_core::FileNode,
    current_path: &Path,
    cache: &GitStatusCache,
    cancelled: &impl Fn() -> bool,
) -> bool {
    if cancelled() {
        return false;
    }
    node.git_status = cache.get_status_canonical(current_path);
    let mut path = current_path.to_path_buf();
    let mut stack = vec![(node.children.iter_mut(), None::<PathBuf>)];
    while let Some((children, _)) = stack.last_mut() {
        let Some(child) = children.next() else {
            let (_, previous) = stack.pop().expect("active annotation frame");
            if stack.is_empty() {
                break;
            }
            if let Some(previous) = previous {
                path = previous;
            } else {
                path.pop();
            }
            continue;
        };
        if cancelled() {
            return false;
        }
        // Scanned names are single components, so one shared path buffer is
        // sufficient. Publicly constructed trees can contain other names;
        // retain their original Path::join behavior with a saved parent path.
        let name = Path::new(child.file_name());
        let mut components = name.components();
        let single_component = matches!(components.next(), Some(std::path::Component::Normal(part)) if part == name.as_os_str())
            && components.next().is_none();
        let previous = (!single_component).then(|| path.clone());
        path.push(name);
        child.git_status = cache.get_status_canonical(&path);
        stack.push((child.children.iter_mut(), previous));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_handles_deep_trees_on_a_small_thread_stack() {
        use gravityfile_core::{FileNode, Timestamps};
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut root =
                    FileNode::new_directory("d", Timestamps::with_modified(std::time::UNIX_EPOCH));
                for _ in 1..4000 {
                    let mut parent = FileNode::new_directory(
                        "d",
                        Timestamps::with_modified(std::time::UNIX_EPOCH),
                    );
                    parent.children.push(root);
                    root = parent;
                }
                let mut path = PathBuf::from("root");
                for _ in 1..4000 {
                    path.push("d");
                }
                let mut cache = GitStatusCache::new();
                cache.statuses.insert(path, GitStatus::Modified);
                assert!(apply_status_iterative(
                    &mut root,
                    Path::new("root"),
                    &cache,
                    &|| false
                ));
                let mut leaf = &root;
                while let Some(child) = leaf.children.first() {
                    leaf = child;
                }
                assert_eq!(leaf.git_status, Some(GitStatus::Modified));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn annotation_restores_parent_path_after_non_component_names() {
        use gravityfile_core::{FileNode, Timestamps};
        let mut root =
            FileNode::new_directory("root", Timestamps::with_modified(std::time::UNIX_EPOCH));
        let mut cache = GitStatusCache::new();
        for name in ["nested/name", "", "../outside", "sibling"] {
            root.children.push(FileNode::new_directory(
                name,
                Timestamps::with_modified(std::time::UNIX_EPOCH),
            ));
            cache
                .statuses
                .insert(Path::new("root").join(name), GitStatus::Modified);
        }
        assert!(apply_status_iterative(
            &mut root,
            Path::new("root"),
            &cache,
            &|| false
        ));
        assert!(
            root.children
                .iter()
                .all(|node| node.git_status == Some(GitStatus::Modified))
        );
    }

    #[test]
    fn clean_tracking_compaction_cancellation_discards_all_partial_annotations() {
        for cancel_at in [1, 6, 101] {
            let mut cache = GitStatusCache::new();
            cache.ignored_directories.insert(PathBuf::from("generated"));
            cache
                .statuses
                .insert(PathBuf::from("modified"), GitStatus::Modified);
            for index in 0..100 {
                cache
                    .clean_tracked
                    .insert(PathBuf::from(format!("generated/{index}")));
            }
            let checks = std::cell::Cell::new(0);
            assert!(!cache.compact_clean_tracked(&|| {
                checks.set(checks.get() + 1);
                checks.get() == cancel_at
            }));
            assert_eq!(checks.get(), cancel_at);
            assert_eq!(cache.statuses.capacity(), 0);
            assert_eq!(cache.clean_tracked.capacity(), 0);
            assert_eq!(cache.ignored_directories.capacity(), 0);
            assert_eq!(
                cache.get_status_canonical(Path::new("generated/untracked")),
                None
            );
        }
    }

    #[test]
    fn clean_tracking_compaction_retains_only_component_scoped_exceptions() {
        let mut cache = GitStatusCache::new();
        cache.ignored_directories.insert(PathBuf::from("generated"));
        cache.clean_tracked.extend([
            PathBuf::from("generated/tracked"),
            PathBuf::from("generated-other/tracked"),
            PathBuf::from("ordinary/tracked"),
        ]);
        assert!(cache.compact_clean_tracked(&|| false));
        assert_eq!(
            cache.clean_tracked,
            HashSet::from([PathBuf::from("generated/tracked")])
        );
        assert_eq!(
            cache.get_status_canonical(Path::new("generated/tracked")),
            None
        );
        assert_eq!(
            cache.get_status_canonical(Path::new("generated/untracked")),
            Some(GitStatus::Ignored)
        );
        cache.clean_tracked.clear();
        assert!(!cache.compact_clean_tracked(&|| true));
        assert!(cache.ignored_directories.is_empty());
    }

    #[test]
    fn undecodable_status_path_discards_partial_cache_and_reports_failure() {
        let root = Path::new("repository");
        let mut cache = GitStatusCache::new();
        let path = cache
            .utf8_status_path(root, "nested/café".as_bytes())
            .unwrap();
        assert_eq!(path, root.join("nested/café"));
        assert!(cache.initialization_error().is_none());
        cache.statuses.insert(path, GitStatus::Modified);
        assert!(cache.utf8_status_path(root, b"invalid-\xff-name").is_none());
        assert!(cache.is_empty());
        assert_eq!(cache.statuses.capacity(), 0);
        let error = cache.initialization_error().unwrap();
        assert!(error.contains("Cannot decode Git status path"));
        assert!(!error.contains("invalid-"));
    }

    #[test]
    fn annotation_cancellation_stops_before_remaining_nodes() {
        use gravityfile_core::{FileNode, Timestamps};
        let mut root =
            FileNode::new_directory("root", Timestamps::with_modified(std::time::UNIX_EPOCH));
        for index in 1..1001 {
            root.children.push(FileNode::new_directory(
                index.to_string(),
                Timestamps::with_modified(std::time::UNIX_EPOCH),
            ));
        }
        let mut cache = GitStatusCache::new();
        for index in 1..1001 {
            cache.statuses.insert(
                PathBuf::from("root").join(index.to_string()),
                GitStatus::Modified,
            );
        }
        let checks = std::cell::Cell::new(0);
        let complete = apply_status_iterative(&mut root, Path::new("root"), &cache, &|| {
            checks.set(checks.get() + 1);
            checks.get() > 5
        });
        assert!(!complete);
        assert_eq!(checks.get(), 6);
        assert_eq!(
            root.children
                .iter()
                .filter(|child| child.git_status.is_some())
                .count(),
            4
        );
        assert!(apply_status_iterative(
            &mut root,
            Path::new("root"),
            &cache,
            &|| false
        ));
        assert!(
            root.children
                .iter()
                .all(|child| child.git_status == Some(GitStatus::Modified))
        );
    }

    #[cfg(feature = "git")]
    #[test]
    fn cancelled_initialization_clears_reused_statuses_before_discovery() {
        let mut cache = GitStatusCache::new();
        cache.statuses.reserve(100_000);
        cache
            .statuses
            .insert(PathBuf::from("old"), GitStatus::Modified);
        cache.repo_root = Some(PathBuf::from("old"));
        let token = CancellationToken::new();
        token.cancel();
        assert!(!cache.initialize_cancellable(Path::new("unused"), &token));
        assert!(cache.is_empty());
        assert_eq!(cache.statuses.capacity(), 0);
        assert!(cache.repo_root().is_none());
    }

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
