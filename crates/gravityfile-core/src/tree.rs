//! File tree container and statistics.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::config::ScanConfig;
use crate::error::ScanWarning;
use crate::node::{FileNode, NodeKind};

/// Summary statistics for a scanned tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeStats {
    /// Total size in bytes.
    pub total_size: u64,
    /// Total number of files.
    pub total_files: u64,
    /// Total number of directories.
    pub total_dirs: u64,
    /// Total number of symbolic links.
    pub total_symlinks: u64,
    /// Maximum depth reached.
    pub max_depth: u32,
    /// Largest file (path, size).
    #[serde(default, with = "crate::path_serde::optional_pair")]
    pub largest_file: Option<(PathBuf, u64)>,
    /// Oldest file (path, time).
    #[serde(default, with = "crate::path_serde::optional_pair")]
    pub oldest_file: Option<(PathBuf, SystemTime)>,
    /// Newest file (path, time).
    #[serde(default, with = "crate::path_serde::optional_pair")]
    pub newest_file: Option<(PathBuf, SystemTime)>,
}

impl TreeStats {
    /// Create new empty stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update stats with a file entry. Only clones the path when updating a tracked field.
    /// Equal sizes/times choose the smallest native path, independent of arrival order.
    ///
    /// `modified` is `None` when the file's modification time could not be read;
    /// such files are excluded from oldest/newest tracking rather than being
    /// coerced to the Unix epoch (which would poison the "oldest file" result).
    pub fn record_file(
        &mut self,
        path: &Path,
        size: u64,
        modified: Option<SystemTime>,
        depth: u32,
    ) {
        self.record_file_with(size, modified, depth, || path.to_path_buf());
    }

    /// [`Self::record_file`] that builds the path only when it is needed:
    /// when this file may become the largest, oldest or newest.
    pub fn record_file_with(
        &mut self,
        size: u64,
        modified: Option<SystemTime>,
        depth: u32,
        path: impl FnOnce() -> PathBuf,
    ) {
        self.total_files += 1;
        self.total_size = self.total_size.saturating_add(size);
        self.max_depth = self.max_depth.max(depth);

        let largest = self.largest_file.as_ref().is_none_or(|(_, s)| size >= *s);
        let (oldest, newest) = match modified {
            Some(modified) => (
                self.oldest_file
                    .as_ref()
                    .is_none_or(|(_, t)| modified <= *t),
                self.newest_file
                    .as_ref()
                    .is_none_or(|(_, t)| modified >= *t),
            ),
            None => (false, false),
        };
        if !(largest || oldest || newest) {
            return;
        }
        // Ties are broken by the smaller path, independent of arrival order.
        let path = path();
        if largest
            && self
                .largest_file
                .as_ref()
                .is_none_or(|(previous, s)| size > *s || path < *previous)
        {
            self.largest_file = Some((path.clone(), size));
        }
        if let Some(modified) = modified {
            if oldest
                && self
                    .oldest_file
                    .as_ref()
                    .is_none_or(|(previous, t)| modified < *t || path < *previous)
            {
                self.oldest_file = Some((path.clone(), modified));
            }
            if newest
                && self
                    .newest_file
                    .as_ref()
                    .is_none_or(|(previous, t)| modified > *t || path < *previous)
            {
                self.newest_file = Some((path, modified));
            }
        }
    }

    /// Record a directory.
    pub fn record_dir(&mut self, depth: u32) {
        self.total_dirs += 1;
        self.max_depth = self.max_depth.max(depth);
    }

    /// Record a symlink.
    pub fn record_symlink(&mut self) {
        self.total_symlinks += 1;
    }

    /// Statistics of an existing tree whose root is at `root_path`, as a scan
    /// of it would have recorded them.
    pub fn collect(root: &FileNode, root_path: &Path) -> Self {
        let mut stats = Self::new();
        stats.record_dir(0);
        let mut path = root_path.to_path_buf();
        let mut stack = vec![root.children.iter()];
        while let Some(children) = stack.last_mut() {
            let Some(node) = children.next() else {
                stack.pop();
                if !stack.is_empty() {
                    path.pop();
                }
                continue;
            };
            let depth = stack.len() as u32;
            match node.kind {
                NodeKind::Directory { .. } => {
                    stats.record_dir(depth);
                    if !node.children.is_empty() {
                        path.push(node.file_name());
                        stack.push(node.children.iter());
                    }
                }
                NodeKind::File { .. } => {
                    stats.record_file_with(
                        node.size,
                        Some(node.timestamps.modified),
                        depth,
                        || path.join(node.file_name()),
                    );
                }
                NodeKind::Symlink { .. } => stats.record_symlink(),
                NodeKind::Other => {}
            }
        }
        stats
    }
}

/// Complete scanned file tree with metadata.
///
/// **Warning:** `Clone` performs a deep clone of the entire tree — O(n) in the number of nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTree {
    /// Root node of the tree.
    pub root: FileNode,

    /// Root path that was scanned.
    #[serde(with = "crate::path_serde")]
    pub root_path: PathBuf,

    /// When this scan was performed.
    pub scanned_at: SystemTime,

    /// Duration of the scan.
    pub scan_duration: Duration,

    /// Scan configuration used.
    pub config: ScanConfig,

    /// Summary statistics.
    pub stats: TreeStats,

    /// Warnings encountered during scan.
    pub warnings: Vec<ScanWarning>,
}

impl FileTree {
    /// Create a new file tree.
    ///
    /// `scanned_at` is stamped at construction time, which is when the scan
    /// finished. `scan_duration` is supplied by the caller, so the scan's start
    /// time is `scanned_at - scan_duration`.
    pub fn new(
        root: FileNode,
        root_path: PathBuf,
        config: ScanConfig,
        stats: TreeStats,
        scan_duration: Duration,
        warnings: Vec<ScanWarning>,
    ) -> Self {
        Self {
            root,
            root_path,
            scanned_at: SystemTime::now(),
            scan_duration,
            config,
            stats,
            warnings,
        }
    }

    /// Get the total size of the tree.
    #[inline]
    pub fn total_size(&self) -> u64 {
        self.root.size
    }

    /// Get the total number of files.
    #[inline]
    pub fn total_files(&self) -> u64 {
        self.stats.total_files
    }

    /// Get the total number of directories.
    #[inline]
    pub fn total_dirs(&self) -> u64 {
        self.stats.total_dirs
    }

    /// Check if there were any warnings during scanning.
    #[inline]
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// Observed warning count, including explicitly omitted details.
    pub fn warning_count(&self) -> u64 {
        self.warnings.iter().fold(0u64, |count, warning| {
            count.saturating_add(warning.represented_count())
        })
    }

    /// The entry at `path`, if it is the root or below it.
    pub fn find(&self, path: &Path) -> Option<&FileNode> {
        let mut node = &self.root;
        for name in relative_names(&self.root_path, path)? {
            node = node
                .children
                .iter()
                .find(|child| child.file_name() == name)?;
        }
        Some(node)
    }

    /// Take the entry at `path` out of the tree, keeping every ancestor's size
    /// and counts consistent. The root cannot be removed. Call
    /// [`Self::recount`] after a batch of changes.
    pub fn remove(&mut self, path: &Path) -> Option<FileNode> {
        let mut names = relative_names(&self.root_path, path)?;
        let name = names.pop()?;
        let parent = descend_mut(&mut self.root, &names)?;
        let index = parent
            .children
            .iter()
            .position(|child| child.file_name() == name)?;
        let removed = parent.children.remove(index);
        adjust_chain(&mut self.root, &names, Contribution::of(&removed), false);
        Some(removed)
    }

    /// Put `node` into the directory at `parent`, replacing any entry with
    /// the same name, and keep every ancestor's size and counts consistent.
    /// Returns false, dropping `node`, if `parent` is not a directory in the
    /// tree. Call [`Self::recount`] after a batch of changes.
    pub fn insert(&mut self, parent: &Path, node: FileNode) -> bool {
        let Some(names) = relative_names(&self.root_path, parent) else {
            return false;
        };
        match descend_mut(&mut self.root, &names) {
            Some(directory) if directory.is_dir() => {}
            _ => return false,
        }
        let mut path = parent.to_path_buf();
        path.push(node.file_name());
        self.remove(&path);
        let added = Contribution::of(&node);
        let directory = descend_mut(&mut self.root, &names).expect("checked above");
        directory.children.push(node);
        adjust_chain(&mut self.root, &names, added, true);
        true
    }

    /// Recompute the summary statistics from the tree, after changes.
    pub fn recount(&mut self) {
        self.stats = TreeStats::collect(&self.root, &self.root_path);
    }

    /// Re-express every size in another measure without rescanning: apparent
    /// lengths or allocated blocks, with hard links counted once or each time.
    pub fn remeasure(&mut self, apparent: bool, count_hard_links: bool) {
        self.root.remeasure(apparent, count_hard_links);
        self.config.apparent_size = apparent;
        self.config.count_hard_links = count_hard_links;
        self.stats = TreeStats::collect(&self.root, &self.root_path);
    }
}

/// The names leading from `root` to `path`, or `None` if `path` is not
/// `root` or below it.
fn relative_names<'a>(root: &Path, path: &'a Path) -> Option<Vec<&'a std::ffi::OsStr>> {
    path.strip_prefix(root)
        .ok()?
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect()
}

fn descend_mut<'a>(
    mut node: &'a mut FileNode,
    names: &[&std::ffi::OsStr],
) -> Option<&'a mut FileNode> {
    for name in names {
        node = node
            .children
            .iter_mut()
            .find(|child| child.file_name() == *name)?;
    }
    Some(node)
}

/// What an entry adds to each of its ancestors.
#[derive(Clone, Copy)]
struct Contribution {
    size: u64,
    files: u64,
    dirs: u64,
}

impl Contribution {
    fn of(node: &FileNode) -> Self {
        Self {
            size: node.size,
            files: if node.is_dir() { node.file_count() } else { 1 },
            dirs: if node.is_dir() {
                node.dir_count() + 1
            } else {
                0
            },
        }
    }
}

/// Add or subtract `change` along the root and the directories named by
/// `names`, re-sorting each one's children by size, largest first.
fn adjust_chain(root: &mut FileNode, names: &[&std::ffi::OsStr], change: Contribution, add: bool) {
    let apply = |node: &mut FileNode| {
        let step = |value: u64, by: u64| {
            if add {
                value.saturating_add(by)
            } else {
                value.saturating_sub(by)
            }
        };
        node.size = step(node.size, change.size);
        if let NodeKind::Directory {
            file_count,
            dir_count,
        } = &mut node.kind
        {
            *file_count = step(*file_count, change.files);
            *dir_count = step(*dir_count, change.dirs);
        }
    };
    let sort = |node: &mut FileNode| {
        node.children
            .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));
    };
    // Sizes first, top-down; then each level's order, which depends on the
    // updated sizes of its children.
    let mut node = &mut *root;
    apply(node);
    for name in names {
        node = node
            .children
            .iter_mut()
            .find(|child| child.file_name() == *name)
            .expect("the chain exists");
        apply(node);
    }
    for depth in (0..=names.len()).rev() {
        sort(descend_mut(root, &names[..depth]).expect("the chain exists"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Timestamps;

    fn sample() -> FileTree {
        let time = Timestamps::with_modified(SystemTime::UNIX_EPOCH);
        let file = |name: &str, size| FileNode::new_file(name, size, 0, time, false);
        let mut sub = FileNode::new_directory("sub", time);
        sub.children = vec![file("a", 300), file("b", 100)];
        let mut root = FileNode::new_directory("root", time);
        root.children = vec![sub, file("c", 200)];
        root.update_counts();
        root.size = 600;
        root.children[0].size = 400;
        let mut tree = FileTree::new(
            root,
            PathBuf::from("/root"),
            ScanConfig::new("/root"),
            TreeStats::new(),
            Duration::ZERO,
            Vec::new(),
        );
        tree.recount();
        tree
    }

    #[test]
    fn removing_an_entry_updates_every_ancestor_and_order() {
        let mut tree = sample();
        let removed = tree.remove(Path::new("/root/sub/a")).unwrap();
        assert_eq!(removed.name, "a");
        assert_eq!(tree.root.size, 300);
        let sub = tree.find(Path::new("/root/sub")).unwrap();
        assert_eq!((sub.size, sub.file_count()), (100, 1));
        assert_eq!(tree.root.file_count(), 2);
        assert_eq!(
            tree.root.children[0].name, "c",
            "c (200) now outranks sub (100)"
        );
        tree.recount();
        assert_eq!(tree.stats.total_files, 2);
        assert_eq!(tree.stats.total_size, 300);
        // A directory takes its whole subtree with it.
        tree.remove(Path::new("/root/sub")).unwrap();
        assert_eq!(
            (
                tree.root.size,
                tree.root.file_count(),
                tree.root.dir_count()
            ),
            (200, 1, 0)
        );
        assert!(tree.remove(Path::new("/root")).is_none());
        assert!(tree.remove(Path::new("/elsewhere/c")).is_none());
        assert!(tree.remove(Path::new("/root/missing")).is_none());
    }

    #[test]
    fn inserting_an_entry_replaces_one_of_the_same_name() {
        let mut tree = sample();
        let time = Timestamps::with_modified(SystemTime::UNIX_EPOCH);
        let mut grown = FileNode::new_directory("sub", time);
        grown.children = vec![FileNode::new_file("x", 1000, 0, time, false)];
        grown.update_counts();
        grown.size = 1000;
        assert!(tree.insert(Path::new("/root"), grown));
        assert_eq!(tree.root.size, 1200);
        assert_eq!(tree.root.file_count(), 2);
        assert_eq!(tree.find(Path::new("/root/sub/x")).unwrap().size, 1000);
        assert!(tree.find(Path::new("/root/sub/a")).is_none());

        let new = FileNode::new_file("d", 50, 0, time, false);
        assert!(tree.insert(Path::new("/root/sub"), new));
        assert_eq!(tree.root.size, 1250);
        assert_eq!(tree.find(Path::new("/root/sub")).unwrap().file_count(), 2);
        let file = FileNode::new_file("e", 1, 0, time, false);
        assert!(!tree.insert(Path::new("/root/c"), file), "c is a file");
    }

    #[test]
    fn tied_extremes_are_independent_of_arrival_order() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        for order in [["c", "b", "a"], ["a", "c", "b"], ["b", "a", "c"]] {
            let mut stats = TreeStats::new();
            for path in order {
                stats.record_file(Path::new(path), 42, Some(time), 1);
            }
            assert_eq!(stats.largest_file, Some((PathBuf::from("a"), 42)));
            assert_eq!(stats.oldest_file, Some((PathBuf::from("a"), time)));
            assert_eq!(stats.newest_file, Some((PathBuf::from("a"), time)));
            assert_eq!(stats.total_files, 3);
            assert_eq!(stats.total_size, 126);
        }
    }

    #[cfg(unix)]
    #[test]
    fn tied_extremes_compare_native_paths_not_lossy_labels() {
        use std::os::unix::ffi::OsStrExt;
        let paths = [b"/file-\xfe".as_slice(), b"/file-\xff".as_slice()]
            .map(|bytes| PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
        assert_eq!(paths[0].to_string_lossy(), paths[1].to_string_lossy());
        for order in [[0, 1], [1, 0]] {
            let mut stats = TreeStats::new();
            for index in order {
                stats.record_file(&paths[index], 10, Some(SystemTime::UNIX_EPOCH), 1);
            }
            assert_eq!(stats.largest_file.unwrap().0, paths[0]);
            assert_eq!(stats.oldest_file.unwrap().0, paths[0]);
            assert_eq!(stats.newest_file.unwrap().0, paths[0]);
        }
    }

    #[test]
    fn test_tree_stats_default() {
        let stats = TreeStats::default();
        assert_eq!(stats.total_size, 0);
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_dirs, 0);
    }

    #[test]
    fn test_tree_stats_record_file() {
        let mut stats = TreeStats::new();
        let now = SystemTime::now();

        stats.record_file(Path::new("/test/file.txt"), 1024, Some(now), 2);

        assert_eq!(stats.total_files, 1);
        assert_eq!(stats.total_size, 1024);
        assert_eq!(stats.max_depth, 2);
        assert!(stats.largest_file.is_some());
    }

    #[test]
    fn test_tree_stats_record_file_tracks_extremes() {
        let mut stats = TreeStats::new();
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);

        stats.record_file(Path::new("/small_old"), 100, Some(t1), 1);
        stats.record_file(Path::new("/big_new"), 999, Some(t2), 2);

        assert_eq!(stats.largest_file.as_ref().unwrap().1, 999);
        assert_eq!(stats.oldest_file.as_ref().unwrap().1, t1);
        assert_eq!(stats.newest_file.as_ref().unwrap().1, t2);
    }

    #[test]
    fn test_record_file_unknown_mtime_does_not_poison_extremes() {
        let mut stats = TreeStats::new();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        // A file with a real mtime, then one with an unreadable mtime (None).
        stats.record_file(Path::new("/has_time"), 10, Some(t), 1);
        stats.record_file(Path::new("/no_time"), 20, None, 1);

        // Both files counted, but the unknown-mtime file must not become the
        // oldest/newest (which previously happened via a Unix-epoch fallback).
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_size, 30);
        assert_eq!(
            stats.oldest_file.as_ref().unwrap().0,
            Path::new("/has_time")
        );
        assert_eq!(
            stats.newest_file.as_ref().unwrap().0,
            Path::new("/has_time")
        );
    }
}
