//! File tree container and statistics.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::config::ScanConfig;
use crate::error::ScanWarning;
use crate::node::FileNode;

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
    pub largest_file: Option<(PathBuf, u64)>,
    /// Oldest file (path, time).
    pub oldest_file: Option<(PathBuf, SystemTime)>,
    /// Newest file (path, time).
    pub newest_file: Option<(PathBuf, SystemTime)>,
}

impl TreeStats {
    /// Create new empty stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update stats with a file entry. Only clones the path when updating a tracked field.
    pub fn record_file(&mut self, path: &Path, size: u64, modified: SystemTime, depth: u32) {
        self.total_files += 1;
        self.total_size += size;
        self.max_depth = self.max_depth.max(depth);

        // Track largest file
        if self.largest_file.as_ref().is_none_or(|(_, s)| size > *s) {
            self.largest_file = Some((path.to_path_buf(), size));
        }

        // Track oldest file
        if self.oldest_file.as_ref().is_none_or(|(_, t)| modified < *t) {
            self.oldest_file = Some((path.to_path_buf(), modified));
        }

        // Track newest file
        if self.newest_file.as_ref().is_none_or(|(_, t)| modified > *t) {
            self.newest_file = Some((path.to_path_buf(), modified));
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
}

/// Complete scanned file tree with metadata.
///
/// **Warning:** `Clone` performs a deep clone of the entire tree — O(n) in the number of nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTree {
    /// Root node of the tree.
    pub root: FileNode,

    /// Root path that was scanned.
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
    /// Create a new file tree. `scanned_at` should be captured by the caller at scan start time.
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

        stats.record_file(Path::new("/test/file.txt"), 1024, now, 2);

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

        stats.record_file(Path::new("/small_old"), 100, t1, 1);
        stats.record_file(Path::new("/big_new"), 999, t2, 2);

        assert_eq!(stats.largest_file.as_ref().unwrap().1, 999);
        assert_eq!(stats.oldest_file.as_ref().unwrap().1, t1);
        assert_eq!(stats.newest_file.as_ref().unwrap().1, t2);
    }
}
