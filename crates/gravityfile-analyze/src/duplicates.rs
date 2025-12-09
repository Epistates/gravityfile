//! Duplicate file detection using content hashing.
//!
//! Uses a three-phase algorithm for efficiency:
//! 1. Group files by size (instant, O(n))
//! 2. Compute partial hash for size-matched files (first + last 4KB)
//! 3. Compute full BLAKE3 hash for partial-hash matches
//!
//! This minimizes disk I/O by eliminating non-duplicates early.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use blake3::Hasher;
use derive_builder::Builder;
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

    /// Patterns to exclude from duplicate detection.
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

    /// Total size of all duplicate files.
    pub total_duplicate_size: u64,

    /// Total wasted space (could be reclaimed).
    pub total_wasted_space: u64,

    /// Number of files analyzed.
    pub files_analyzed: u64,

    /// Number of files that have duplicates.
    pub files_with_duplicates: u64,

    /// Number of unique duplicate groups.
    pub group_count: usize,
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
}

impl DuplicateFinder {
    /// Create a new duplicate finder with default config.
    pub fn new() -> Self {
        Self {
            config: DuplicateConfig::default(),
        }
    }

    /// Create a new duplicate finder with custom config.
    pub fn with_config(config: DuplicateConfig) -> Self {
        Self { config }
    }

    /// Find duplicates in a scanned file tree.
    pub fn find_duplicates(&self, tree: &FileTree) -> DuplicateReport {
        // Phase 1: Collect all files with their paths and sizes
        let mut files: Vec<FileInfo> = Vec::new();
        self.collect_files(&tree.root, &tree.root_path, &mut files);

        // Filter by size constraints
        files.retain(|f| f.size >= self.config.min_size && f.size <= self.config.max_size);

        let files_analyzed = files.len() as u64;

        // Phase 2: Group by size
        let size_groups = self.group_by_size(files);

        // Phase 3: For groups with 2+ files, compute hashes (parallelized across size groups)
        let duplicate_groups: Vec<DuplicateGroup> = if self.config.quick_compare {
            // Process size groups in parallel
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

        // Apply max_groups limit if set
        if self.config.max_groups > 0 && groups.len() > self.config.max_groups {
            groups.truncate(self.config.max_groups);
        }

        // Calculate totals
        let total_duplicate_size: u64 = groups.iter().map(|g| g.size * g.paths.len() as u64).sum();
        let total_wasted_space: u64 = groups.iter().map(|g| g.wasted_bytes).sum();
        let files_with_duplicates: u64 = groups.iter().map(|g| g.paths.len() as u64).sum();
        let group_count = groups.len();

        DuplicateReport {
            groups,
            total_duplicate_size,
            total_wasted_space,
            files_analyzed,
            files_with_duplicates,
            group_count,
        }
    }

    /// Collect all files from the tree.
    fn collect_files(&self, node: &FileNode, current_path: &Path, files: &mut Vec<FileInfo>) {
        match &node.kind {
            NodeKind::File { .. } => {
                // Check exclusions
                let name = node.name.as_str();
                let should_exclude = self.config.exclude_patterns.iter().any(|p| {
                    name.contains(p) || current_path.to_string_lossy().contains(p)
                });

                if !should_exclude && node.size > 0 {
                    files.push(FileInfo {
                        path: current_path.to_path_buf(),
                        size: node.size,
                    });
                }
            }
            NodeKind::Directory { .. } => {
                for child in &node.children {
                    let child_path = current_path.join(&*child.name);
                    self.collect_files(child, &child_path, files);
                }
            }
            _ => {}
        }
    }

    /// Group files by size.
    fn group_by_size(&self, files: Vec<FileInfo>) -> HashMap<u64, Vec<FileInfo>> {
        let mut groups: HashMap<u64, Vec<FileInfo>> = HashMap::new();
        for file in files {
            groups.entry(file.size).or_default().push(file);
        }
        // Remove groups with only one file
        groups.retain(|_, v| v.len() > 1);
        groups
    }

    /// Find duplicates in a single size group using partial hash then full hash.
    fn find_dups_in_size_group_partial(&self, files: Vec<FileInfo>) -> Vec<DuplicateGroup> {
        if files.len() < 2 {
            return Vec::new();
        }

        let mut result = Vec::new();

        // Compute partial hashes in parallel
        let partial_hashes: Vec<(PathBuf, u64, Option<[u8; 32]>)> = files
            .par_iter()
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

        // For groups with 2+ matching partial hashes, compute full hash
        for (_partial_hash, candidates) in partial_groups {
            if candidates.len() < 2 {
                continue;
            }

            // Compute full hashes in parallel
            let full_hashes: Vec<(PathBuf, u64, Option<ContentHash>)> = candidates
                .par_iter()
                .map(|(path, size)| {
                    let hash = self.compute_full_hash(path);
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

    /// Find duplicates in a single size group using only full hash.
    fn find_dups_in_size_group_full(&self, files: Vec<FileInfo>) -> Vec<DuplicateGroup> {
        if files.len() < 2 {
            return Vec::new();
        }

        // Compute full hashes in parallel
        let hashes: Vec<(PathBuf, u64, Option<ContentHash>)> = files
            .par_iter()
            .map(|f| {
                let hash = self.compute_full_hash(&f.path);
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

    /// Compute full BLAKE3 hash of a file.
    fn compute_full_hash(&self, path: &Path) -> Option<ContentHash> {
        // Try memory-mapped I/O for larger files (faster)
        let file = File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        let file_size = metadata.len();

        // Use mmap for files > 128KB, buffered read for smaller
        if file_size > 128 * 1024 {
            // Memory-mapped hashing (much faster for large files)
            let mmap = unsafe { memmap2::Mmap::map(&file).ok()? };
            let hash = blake3::hash(&mmap);
            Some(ContentHash::new(*hash.as_bytes()))
        } else {
            // Buffered read for small files
            let mut hasher = Hasher::new();
            let mut buffer = vec![0u8; 64 * 1024];
            let mut file = file;

            loop {
                let bytes_read = file.read(&mut buffer).ok()?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
            }

            Some(ContentHash::new(*hasher.finalize().as_bytes()))
        }
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
        let finder = DuplicateFinder::new();

        let hash1 = finder.compute_full_hash(&temp.path().join("file1.txt"));
        let hash2 = finder.compute_full_hash(&temp.path().join("file2.txt"));
        let hash3 = finder.compute_full_hash(&temp.path().join("file3.txt"));

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
}
