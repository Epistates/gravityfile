//! Duplicate file detection using content hashing.
//!
//! Uses a three-phase algorithm for efficiency:
//! 1. Group files by size (instant, O(n))
//! 2. Compute partial hash for size-matched files (first + last 4KB)
//! 3. Compute full BLAKE3 hash for partial-hash matches
//!
//! This minimizes disk I/O by eliminating non-duplicates early.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use blake3::Hasher;
use derive_builder::Builder;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use gravityfile_core::{ContentHash, FileNode, FileTree, NodeKind};

/// Configuration for duplicate detection.
#[derive(Debug, Clone, Builder)]
#[builder(setter(into))]
pub struct DuplicateConfig {
    /// Minimum file size to consider (skip tiny files).
    #[builder(default = "1024")]
    pub min_size: u64,

    /// Maximum file size to consider (skip huge files).
    #[builder(default = "u64::MAX")]
    pub max_size: u64,

    /// Use quick comparison (size + partial hash) before full hash.
    #[builder(default = "true")]
    pub quick_compare: bool,

    /// Number of bytes for partial hash from start of file.
    #[builder(default = "4096")]
    pub partial_hash_head: usize,

    /// Number of bytes for partial hash from end of file.
    #[builder(default = "4096")]
    pub partial_hash_tail: usize,

    /// Patterns to exclude from duplicate detection (glob syntax).
    #[builder(default)]
    pub exclude_patterns: Vec<String>,

    /// Maximum number of groups to return (0 = unlimited).
    #[builder(default = "0")]
    pub max_groups: usize,
}

impl Default for DuplicateConfig {
    fn default() -> Self {
        Self {
            min_size: 1024,
            max_size: u64::MAX,
            quick_compare: true,
            partial_hash_head: 4096,
            partial_hash_tail: 4096,
            exclude_patterns: Vec::new(),
            max_groups: 0,
        }
    }
}

impl DuplicateConfig {
    /// Create a new config builder.
    pub fn builder() -> DuplicateConfigBuilder {
        DuplicateConfigBuilder::default()
    }
}

/// A group of duplicate files sharing the same content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    /// Content hash shared by all files in this group.
    pub hash: ContentHash,

    /// Size of each file in bytes.
    pub size: u64,

    /// Paths to all duplicate files.
    pub paths: Vec<PathBuf>,

    /// Wasted space: size * (count - 1).
    pub wasted_bytes: u64,
}

impl DuplicateGroup {
    /// Get the number of duplicate files.
    pub fn count(&self) -> usize {
        self.paths.len()
    }

    /// Check if keeping one file, how many could be deleted.
    pub fn deletable_count(&self) -> usize {
        self.paths.len().saturating_sub(1)
    }
}

/// Results from duplicate analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateReport {
    /// Groups of duplicate files, sorted by wasted space descending.
    pub groups: Vec<DuplicateGroup>,

    /// Total size of all duplicate files (computed before max_groups truncation).
    pub total_duplicate_size: u64,

    /// Total wasted space (could be reclaimed, computed before max_groups truncation).
    pub total_wasted_space: u64,

    /// Number of files analyzed.
    pub files_analyzed: u64,

    /// Number of files that have duplicates (computed before max_groups truncation).
    pub files_with_duplicates: u64,

    /// Number of unique duplicate groups (computed before max_groups truncation).
    pub group_count: usize,

    /// Number of groups omitted due to max_groups limit.
    pub groups_omitted: usize,
}

impl DuplicateReport {
    /// Check if any duplicates were found.
    pub fn has_duplicates(&self) -> bool {
        !self.groups.is_empty()
    }

    /// Get total number of duplicate files across all groups.
    pub fn total_duplicate_files(&self) -> usize {
        self.groups.iter().map(|g| g.paths.len()).sum()
    }
}

/// Progress information during duplicate detection.
#[derive(Debug, Clone)]
pub struct HashProgress {
    /// Files processed so far.
    pub files_processed: u64,
    /// Total files to process.
    pub total_files: u64,
    /// Bytes hashed so far.
    pub bytes_hashed: u64,
    /// Current file being processed.
    pub current_file: Option<PathBuf>,
    /// Duplicates found so far.
    pub duplicates_found: u64,
}

/// Duplicate file finder.
pub struct DuplicateFinder {
    config: DuplicateConfig,
    /// Compiled globset for exclude patterns. Built once at construction.
    exclude_globset: Option<GlobSet>,
}

impl DuplicateFinder {
    /// Create a new duplicate finder with default config.
    pub fn new() -> Self {
        Self {
            config: DuplicateConfig::default(),
            exclude_globset: None,
        }
    }

    /// Create a new duplicate finder with custom config.
    pub fn with_config(config: DuplicateConfig) -> Self {
        let exclude_globset = build_globset(&config.exclude_patterns);
        Self {
            config,
            exclude_globset,
        }
    }

    /// Find duplicates in a scanned file tree.
    pub fn find_duplicates(&self, tree: &FileTree) -> DuplicateReport {
        // Phase 1: Collect all files with their paths and sizes.
        // Pass seen_inodes to skip hardlinked copies of the same data.
        let mut files: Vec<FileInfo> = Vec::new();
        let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
        let mut seen_paths: HashSet<PathBuf> = HashSet::new();

        // Handle root-is-file case
        if tree.root.is_file() {
            let path = tree.root_path.clone();
            let should_exclude = self.is_excluded(&path);
            if !should_exclude && tree.root.size > 0 {
                if let Some(inode) = &tree.root.inode {
                    if seen_inodes.insert((inode.inode, inode.device))
                        && seen_paths.insert(path.clone())
                    {
                        files.push(FileInfo {
                            path,
                            size: tree.root.size,
                        });
                    }
                } else if seen_paths.insert(path.clone()) {
                    files.push(FileInfo {
                        path,
                        size: tree.root.size,
                    });
                }
            }
        } else {
            self.collect_files(
                &tree.root,
                &tree.root_path,
                &mut files,
                &mut seen_inodes,
                &mut seen_paths,
            );
        }

        // Filter by size constraints
        files.retain(|f| f.size >= self.config.min_size && f.size <= self.config.max_size);

        let files_analyzed = files.len() as u64;

        // Phase 2: Group by size
        let size_groups = self.group_by_size(files);

        // Phase 3: For groups with 2+ files, compute hashes.
        // Only outer iterator is parallelised; inner stays sequential to avoid nested par_iter.
        let duplicate_groups: Vec<DuplicateGroup> = if self.config.quick_compare {
            size_groups
                .into_par_iter()
                .flat_map(|(_size, files)| self.find_dups_in_size_group_partial(files))
                .collect()
        } else {
            size_groups
                .into_par_iter()
                .flat_map(|(_size, files)| self.find_dups_in_size_group_full(files))
                .collect()
        };

        // Sort by wasted space descending
        let mut groups = duplicate_groups;
        groups.sort_by(|a, b| b.wasted_bytes.cmp(&a.wasted_bytes));

        // Compute all summary stats from the full (untruncated) groups list
        let total_duplicate_size: u64 = groups.iter().map(|g| g.size * g.paths.len() as u64).sum();
        let total_wasted_space: u64 = groups.iter().map(|g| g.wasted_bytes).sum();
        let files_with_duplicates: u64 = groups.iter().map(|g| g.paths.len() as u64).sum();
        let group_count = groups.len();

        // Truncate AFTER computing stats
        let groups_omitted = if self.config.max_groups > 0 && groups.len() > self.config.max_groups
        {
            let omitted = groups.len() - self.config.max_groups;
            groups.truncate(self.config.max_groups);
            omitted
        } else {
            0
        };

        DuplicateReport {
            groups,
            total_duplicate_size,
            total_wasted_space,
            files_analyzed,
            files_with_duplicates,
            group_count,
            groups_omitted,
        }
    }

    /// Collect all files from the tree, deduplicating by inode and path.
    fn collect_files(
        &self,
        node: &FileNode,
        current_path: &Path,
        files: &mut Vec<FileInfo>,
        seen_inodes: &mut HashSet<(u64, u64)>,
        seen_paths: &mut HashSet<PathBuf>,
    ) {
        match &node.kind {
            NodeKind::File { .. } => {
                // current_path is already the full file path — the Directory branch
                // pre-joins the child name before recursing.
                let file_path = current_path.to_path_buf();

                // Skip duplicate paths
                if !seen_paths.insert(file_path.clone()) {
                    return;
                }

                // Skip hardlinks to already-seen inodes
                if let Some(inode) = &node.inode
                    && !seen_inodes.insert((inode.inode, inode.device))
                {
                    return;
                }

                // Apply glob exclude patterns
                if self.is_excluded(&file_path) {
                    return;
                }

                if node.size > 0 {
                    files.push(FileInfo {
                        path: file_path,
                        size: node.size,
                    });
                }
            }
            NodeKind::Directory { .. } => {
                for child in &node.children {
                    let child_path = current_path.join(&*child.name);
                    self.collect_files(child, &child_path, files, seen_inodes, seen_paths);
                }
            }
            _ => {}
        }
    }

    /// Check whether a path matches any configured exclude glob patterns.
    fn is_excluded(&self, path: &Path) -> bool {
        if let Some(gs) = &self.exclude_globset {
            // Match against the full path and also just the file name component
            if gs.is_match(path) {
                return true;
            }
            if let Some(name) = path.file_name()
                && gs.is_match(Path::new(name))
            {
                return true;
            }
        }
        false
    }

    /// Group files by size.
    fn group_by_size(&self, files: Vec<FileInfo>) -> HashMap<u64, Vec<FileInfo>> {
        let mut groups: HashMap<u64, Vec<FileInfo>> = HashMap::new();
        for file in files {
            groups.entry(file.size).or_default().push(file);
        }
        // Remove groups with only one file — they cannot be duplicates
        groups.retain(|_, v| v.len() > 1);
        groups
    }

    /// Find duplicates in a single size group using partial hash then full hash.
    fn find_dups_in_size_group_partial(&self, files: Vec<FileInfo>) -> Vec<DuplicateGroup> {
        if files.len() < 2 {
            return Vec::new();
        }

        let mut result = Vec::new();

        // Compute partial hashes sequentially within the group
        let partial_hashes: Vec<(PathBuf, u64, Option<[u8; 32]>)> = files
            .iter()
            .map(|f| {
                let hash = self.compute_partial_hash(&f.path);
                (f.path.clone(), f.size, hash)
            })
            .collect();

        // Group by partial hash
        let mut partial_groups: HashMap<[u8; 32], Vec<(PathBuf, u64)>> = HashMap::new();
        for (path, size, hash) in partial_hashes {
            if let Some(h) = hash {
                partial_groups.entry(h).or_default().push((path, size));
            }
        }

        // For groups with 2+ matching partial hashes, compute full hash (sequential)
        for (_partial_hash, candidates) in partial_groups {
            if candidates.len() < 2 {
                continue;
            }

            let full_hashes: Vec<(PathBuf, u64, Option<ContentHash>)> = candidates
                .iter()
                .map(|(path, size)| {
                    let hash = compute_full_hash(path);
                    (path.clone(), *size, hash)
                })
                .collect();

            // Group by full hash
            let mut full_groups: HashMap<ContentHash, Vec<PathBuf>> = HashMap::new();
            let mut size_for_hash: HashMap<ContentHash, u64> = HashMap::new();

            for (path, size, hash) in full_hashes {
                if let Some(h) = hash {
                    full_groups.entry(h).or_default().push(path);
                    size_for_hash.insert(h, size);
                }
            }

            // Create duplicate groups
            for (hash, paths) in full_groups {
                if paths.len() >= 2 {
                    let size = size_for_hash[&hash];
                    let wasted_bytes = size * (paths.len() as u64 - 1);
                    result.push(DuplicateGroup {
                        hash,
                        size,
                        paths,
                        wasted_bytes,
                    });
                }
            }
        }

        result
    }

    /// Find duplicates in a single size group using only full hash (sequential within group).
    fn find_dups_in_size_group_full(&self, files: Vec<FileInfo>) -> Vec<DuplicateGroup> {
        if files.len() < 2 {
            return Vec::new();
        }

        let hashes: Vec<(PathBuf, u64, Option<ContentHash>)> = files
            .iter()
            .map(|f| {
                let hash = compute_full_hash(&f.path);
                (f.path.clone(), f.size, hash)
            })
            .collect();

        // Group by hash
        let mut groups: HashMap<ContentHash, Vec<PathBuf>> = HashMap::new();
        let mut size_for_hash: HashMap<ContentHash, u64> = HashMap::new();

        for (path, size, hash) in hashes {
            if let Some(h) = hash {
                groups.entry(h).or_default().push(path);
                size_for_hash.insert(h, size);
            }
        }

        // Create duplicate groups
        let mut result = Vec::new();
        for (hash, paths) in groups {
            if paths.len() >= 2 {
                let size = size_for_hash[&hash];
                let wasted_bytes = size * (paths.len() as u64 - 1);
                result.push(DuplicateGroup {
                    hash,
                    size,
                    paths,
                    wasted_bytes,
                });
            }
        }

        result
    }

    /// Compute a partial hash (first + last N bytes).
    fn compute_partial_hash(&self, path: &Path) -> Option<[u8; 32]> {
        let mut file = File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        let file_size = metadata.len();

        let mut hasher = Hasher::new();

        // Read from start
        let head_size = (self.config.partial_hash_head as u64).min(file_size);
        let mut head_buf = vec![0u8; head_size as usize];
        file.read_exact(&mut head_buf).ok()?;
        hasher.update(&head_buf);

        // Read from end (if file is large enough)
        if file_size > head_size {
            let tail_size = (self.config.partial_hash_tail as u64).min(file_size - head_size);
            if tail_size > 0 {
                file.seek(SeekFrom::End(-(tail_size as i64))).ok()?;
                let mut tail_buf = vec![0u8; tail_size as usize];
                file.read_exact(&mut tail_buf).ok()?;
                hasher.update(&tail_buf);
            }
        }

        // Include file size in hash to differentiate files with same head/tail
        hasher.update(&file_size.to_le_bytes());

        Some(*hasher.finalize().as_bytes())
    }
}

/// Compute full BLAKE3 hash of a file using buffered streaming I/O.
fn compute_full_hash(path: &Path) -> Option<ContentHash> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(ContentHash::new(*hasher.finalize().as_bytes()))
}

/// Build a GlobSet from a list of glob pattern strings.
/// Returns None if the pattern list is empty or all patterns are invalid.
fn build_globset(patterns: &[String]) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    let mut any_valid = false;
    for p in patterns {
        match Glob::new(p) {
            Ok(g) => {
                builder.add(g);
                any_valid = true;
            }
            Err(e) => {
                tracing::warn!("Invalid exclude glob pattern {:?}: {}", p, e);
            }
        }
    }
    if any_valid {
        builder.build().ok()
    } else {
        None
    }
}

impl Default for DuplicateFinder {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal struct for file information.
#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_files() -> TempDir {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create duplicate files
        fs::write(root.join("file1.txt"), "duplicate content here").unwrap();
        fs::write(root.join("file2.txt"), "duplicate content here").unwrap();
        fs::write(root.join("file3.txt"), "unique content").unwrap();

        // Create subdirectory with another duplicate
        fs::create_dir(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/file4.txt"), "duplicate content here").unwrap();

        temp
    }

    #[test]
    fn test_compute_full_hash() {
        let temp = create_test_files();

        let hash1 = compute_full_hash(&temp.path().join("file1.txt"));
        let hash2 = compute_full_hash(&temp.path().join("file2.txt"));
        let hash3 = compute_full_hash(&temp.path().join("file3.txt"));

        assert!(hash1.is_some());
        assert!(hash2.is_some());
        assert!(hash3.is_some());

        // file1 and file2 should have same hash
        assert_eq!(hash1, hash2);
        // file3 should be different
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_partial_hash() {
        let temp = create_test_files();
        let finder = DuplicateFinder::new();

        let hash1 = finder.compute_partial_hash(&temp.path().join("file1.txt"));
        let hash2 = finder.compute_partial_hash(&temp.path().join("file2.txt"));

        assert!(hash1.is_some());
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_groups_omitted_field() {
        // Build a report with max_groups limiting the output
        let report = DuplicateReport {
            groups: Vec::new(),
            total_duplicate_size: 0,
            total_wasted_space: 0,
            files_analyzed: 0,
            files_with_duplicates: 0,
            group_count: 5,
            groups_omitted: 3,
        };
        assert_eq!(report.groups_omitted, 3);
        assert_eq!(report.group_count, 5);
    }

    #[test]
    fn test_glob_exclude() {
        let config = DuplicateConfig {
            exclude_patterns: vec!["*.log".to_string()],
            ..Default::default()
        };
        let finder = DuplicateFinder::with_config(config);
        assert!(finder.is_excluded(Path::new("/some/path/debug.log")));
        assert!(!finder.is_excluded(Path::new("/some/path/main.rs")));
    }
}
