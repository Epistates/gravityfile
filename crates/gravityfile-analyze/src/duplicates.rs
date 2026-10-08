//! Duplicate file detection using content hashing.
//!
//! Uses a three-phase algorithm for efficiency:
//! 1. Group files by size (instant, O(n))
//! 2. Compute partial hash for size-matched files (first + last 4KB)
//! 3. Compute full BLAKE3 hash for partial-hash matches
//!
//! This minimizes disk I/O by eliminating non-duplicates early.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use blake3::Hasher;
use derive_builder::Builder;
use globset::GlobSet;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::status::{AnalysisStatus, IssueCollector};
use gravityfile_core::{ContentHash, FileTree};

/// Configuration for duplicate detection.
#[derive(Debug, Clone, Builder, Serialize)]
#[builder(setter(into), build_fn(validate = "Self::validate"))]
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

impl DuplicateConfigBuilder {
    fn validate(&self) -> Result<(), String> {
        if self.min_size.unwrap_or(1024) > self.max_size.unwrap_or(u64::MAX) {
            return Err("minimum duplicate size exceeds maximum size".into());
        }
        gravityfile_core::ScanConfig::build_ignore_globset(
            self.exclude_patterns.as_deref().unwrap_or_default(),
        )?;
        Ok(())
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
    #[serde(with = "gravityfile_core::path_serde::list")]
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
    /// Completeness and bounded diagnostics. Missing legacy data is unknown.
    #[serde(default)]
    pub status: AnalysisStatus,
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

/// Wall-clock phases of one duplicate analysis, excluding the preceding scan
/// and report serialization. Parallel hashing time is elapsed wall time, not
/// summed CPU time. Timings are diagnostic and do not describe completeness.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DuplicateTimings {
    /// Fresh metadata collection, exclusions and hard-link representative selection.
    pub collection: Duration,
    /// Size filtering and grouping.
    pub grouping: Duration,
    /// Partial/full hashing, including identity validation and issue collection.
    pub hashing: Duration,
    /// Deterministic ordering, summary accounting, truncation and status assembly.
    pub finalization: Duration,
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
    pub fn with_config(config: DuplicateConfig) -> Result<Self, String> {
        if config.min_size > config.max_size {
            return Err("minimum duplicate size exceeds maximum size".into());
        }
        let exclude_globset =
            gravityfile_core::ScanConfig::build_ignore_globset(&config.exclude_patterns)?;
        Ok(Self {
            config,
            exclude_globset,
        })
    }

    /// Find duplicates in a scanned file tree.
    pub fn find_duplicates(&self, tree: &FileTree, token: CancellationToken) -> DuplicateReport {
        self.find_duplicates_with_timings(tree, token).0
    }

    /// Analyze with phase timings without changing the report schema or scope.
    pub fn find_duplicates_with_timings(
        &self,
        tree: &FileTree,
        token: CancellationToken,
    ) -> (DuplicateReport, DuplicateTimings) {
        let started = Instant::now();
        let issues = IssueCollector::default();
        let mut files = self.collect_files(tree, &token, &issues);
        let collection = started.elapsed();
        let started = Instant::now();

        // Filter by size constraints
        files.retain(|f| f.size >= self.config.min_size && f.size <= self.config.max_size);

        let files_analyzed = files.len() as u64;

        // Phase 2: Group by size
        let size_groups = self.group_by_size(files);
        let grouping = started.elapsed();
        let started = Instant::now();

        // Phase 3: For groups with 2+ files, compute hashes.
        // Parallelize independent size groups. Unbounded file-level parallelism
        // regressed the measured small-file workload; see docs/BENCHMARKS.md.
        let duplicate_groups: Vec<DuplicateGroup> = if self.config.quick_compare {
            size_groups
                .into_par_iter()
                .flat_map(|(_size, files)| {
                    if token.is_cancelled() {
                        return Vec::new();
                    }
                    self.find_dups_in_size_group_partial(files, &token, &issues)
                })
                .collect()
        } else {
            size_groups
                .into_par_iter()
                .flat_map(|(_size, files)| {
                    if token.is_cancelled() {
                        return Vec::new();
                    }
                    self.find_dups_in_size_group_full(files, &token, &issues)
                })
                .collect()
        };
        let hashing = started.elapsed();
        let started = Instant::now();

        // Sort by wasted space descending
        let mut groups = duplicate_groups;
        for group in &mut groups {
            group.paths.sort();
        }
        groups.sort_by(|a, b| {
            Reverse(a.wasted_bytes)
                .cmp(&Reverse(b.wasted_bytes))
                .then_with(|| a.paths.cmp(&b.paths))
        });

        // Compute all summary stats from the full (untruncated) groups list.
        // Saturating arithmetic guards against overflow on pathological inputs
        // (huge files duplicated very many times).
        let total_duplicate_size: u64 = groups
            .iter()
            .map(|g| g.size.saturating_mul(g.paths.len() as u64))
            .fold(0u64, u64::saturating_add);
        let total_wasted_space: u64 = groups
            .iter()
            .map(|g| g.wasted_bytes)
            .fold(0u64, u64::saturating_add);
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

        let report = DuplicateReport {
            status: issues.finish(tree, token.is_cancelled()),
            groups,
            total_duplicate_size,
            total_wasted_space,
            files_analyzed,
            files_with_duplicates,
            group_count,
            groups_omitted,
        };
        let finalization = started.elapsed();
        (
            report,
            DuplicateTimings {
                collection,
                grouping,
                hashing,
                finalization,
            },
        )
    }

    /// Iterate without recursion and use fresh file identities for hardlinks.
    fn collect_files(
        &self,
        tree: &FileTree,
        token: &CancellationToken,
        issues: &IssueCollector,
    ) -> Vec<FileInfo> {
        let mut files: Vec<FileInfo> = Vec::new();
        // Retain one child iterator per directory depth rather than materializing
        // a path for every pending sibling (potentially millions in a flat tree).
        let mut pending = vec![(
            std::slice::from_ref(&tree.root).iter().rev(),
            tree.root_path.clone(),
            true,
        )];
        let mut seen_paths = HashSet::new();
        #[cfg(unix)]
        let mut seen_inodes = HashMap::new();
        while let Some((children, parent, is_root)) = pending.last_mut() {
            if token.is_cancelled() {
                break;
            }
            let Some(node) = children.next() else {
                pending.pop();
                continue;
            };
            let path = if *is_root {
                parent.clone()
            } else {
                parent.join(node.file_name())
            };
            if node.is_dir() {
                pending.push((node.children.iter().rev(), path, false));
            } else if node.is_file() && !self.is_excluded(&path) && seen_paths.insert(path.clone())
            {
                match FileInfo::read(&path) {
                    Ok(info) => {
                        #[cfg(unix)]
                        match seen_inodes.entry((info.stamp.device, info.stamp.inode)) {
                            std::collections::hash_map::Entry::Occupied(entry) => {
                                // Choose a stable representative among eligible aliases,
                                // rather than depending on tree/traversal arrival order.
                                let previous: &mut FileInfo = &mut files[*entry.get()];
                                if info.path < previous.path {
                                    *previous = info;
                                }
                                continue;
                            }
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                entry.insert(files.len());
                            }
                        }
                        files.push(info);
                    }
                    Err(error) => issues.record(&path, &error),
                }
            }
        }
        files
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

    /// Partial hashes only filter candidates; every reported match is fully hashed.
    fn find_dups_in_size_group_partial(
        &self,
        files: Vec<FileInfo>,
        token: &CancellationToken,
        issues: &IssueCollector,
    ) -> Vec<DuplicateGroup> {
        let covered_bytes = (self.config.partial_hash_head as u64)
            .saturating_add(self.config.partial_hash_tail as u64);
        if files.first().is_some_and(|file| file.size <= covered_bytes) {
            // A size group shares one length. If the partial ranges cover it,
            // filtering would read every byte and then reopen matching files.
            // Use the canonical full hash once, including all identity checks.
            return self.find_dups_in_size_group_full(files, token, issues);
        }
        let mut partial_groups: HashMap<[u8; 32], Vec<FileInfo>> = HashMap::new();
        let mut buffer = hash_buffer(covered_bytes.min(files.first().map_or(0, |file| file.size)));
        for info in files {
            if token.is_cancelled() {
                break;
            }
            match self.compute_partial_hash_with_buffer(&info, token, &mut buffer) {
                Ok(hash) => partial_groups.entry(hash).or_default().push(info),
                Err(error) if !token.is_cancelled() => issues.record(&info.path, &error),
                Err(_) => break,
            }
        }
        drop(buffer);
        partial_groups
            .into_values()
            .filter(|files| files.len() > 1)
            .flat_map(|files| self.find_dups_in_size_group_full(files, token, issues))
            .collect()
    }

    fn find_dups_in_size_group_full(
        &self,
        files: Vec<FileInfo>,
        token: &CancellationToken,
        issues: &IssueCollector,
    ) -> Vec<DuplicateGroup> {
        let mut groups: HashMap<(ContentHash, u64), Vec<PathBuf>> = HashMap::new();
        let mut buffer = hash_buffer(files.first().map_or(0, |file| file.size));
        for info in files {
            if token.is_cancelled() {
                break;
            }
            match compute_full_hash_with_buffer(&info, token, &mut buffer) {
                Ok(hash) => groups.entry((hash, info.size)).or_default().push(info.path),
                Err(error) if !token.is_cancelled() => issues.record(&info.path, &error),
                Err(_) => break,
            }
        }
        groups
            .into_iter()
            .filter(|(_, paths)| paths.len() > 1)
            .map(|((hash, size), paths)| DuplicateGroup {
                hash,
                size,
                wasted_bytes: size.saturating_mul(paths.len() as u64 - 1),
                paths,
            })
            .collect()
    }

    #[cfg(test)]
    fn compute_partial_hash(
        &self,
        info: &FileInfo,
        token: &CancellationToken,
    ) -> io::Result<[u8; 32]> {
        self.compute_partial_hash_with_buffer(info, token, &mut hash_buffer(info.size))
    }

    fn compute_partial_hash_with_buffer(
        &self,
        info: &FileInfo,
        token: &CancellationToken,
        buffer: &mut [u8],
    ) -> io::Result<[u8; 32]> {
        let mut file = info.open()?;
        let mut hasher = Hasher::new();
        let head = (self.config.partial_hash_head as u64).min(info.size);
        hash_bytes(&mut file, head, &mut hasher, token, buffer)?;
        let tail = (self.config.partial_hash_tail as u64).min(info.size - head);
        if tail > 0 {
            file.seek(SeekFrom::Start(info.size - tail))?;
            hash_bytes(&mut file, tail, &mut hasher, token, buffer)?;
        }
        info.verify(&file)?;
        hasher.update(&info.size.to_le_bytes());
        Ok(*hasher.finalize().as_bytes())
    }
}

/// One scratch allocation per active group, rather than a zeroed 64 KiB stack
/// array per file. Small-file groups need only their observed logical length.
fn hash_buffer(length: u64) -> Vec<u8> {
    vec![0; length.clamp(1, 64 * 1024) as usize]
}

/// Read exactly the observed length with bounded memory, even if a file grows.
fn hash_bytes(
    file: &mut File,
    mut remaining: u64,
    hasher: &mut Hasher,
    token: &CancellationToken,
    buffer: &mut [u8],
) -> io::Result<()> {
    while remaining > 0 {
        if token.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "analysis cancelled",
            ));
        }
        let length = remaining.min(buffer.len() as u64) as usize;
        let count = match file.read(&mut buffer[..length]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Err(changed_file());
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(())
}

#[cfg(test)]
fn compute_full_hash(info: &FileInfo, token: &CancellationToken) -> io::Result<ContentHash> {
    compute_full_hash_with_buffer(info, token, &mut hash_buffer(info.size))
}

fn compute_full_hash_with_buffer(
    info: &FileInfo,
    token: &CancellationToken,
    buffer: &mut [u8],
) -> io::Result<ContentHash> {
    let mut file = info.open()?;
    let mut hasher = Hasher::new();
    hash_bytes(&mut file, info.size, &mut hasher, token, buffer)?;
    info.verify(&file)?;
    Ok(ContentHash::new(*hasher.finalize().as_bytes()))
}

impl Default for DuplicateFinder {
    fn default() -> Self {
        Self::new()
    }
}

/// Observation shared by the partial and full reads. This detects ordinary
/// concurrent mutation; it does not turn a live filesystem into a snapshot.
#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    size: u64,
    stamp: FileStamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}

fn changed_file() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "file changed or is no longer a regular file during analysis",
    )
}

impl FileStamp {
    fn read(metadata: &Metadata) -> io::Result<Self> {
        if !metadata.is_file() {
            return Err(changed_file());
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

impl FileInfo {
    fn read(path: &Path) -> io::Result<Self> {
        let stamp = FileStamp::read(&std::fs::symlink_metadata(path)?)?;
        Ok(Self {
            path: path.to_path_buf(),
            size: stamp.length,
            stamp,
        })
    }

    fn open(&self) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Refuse substituted symlinks and avoid blocking on a substituted FIFO.
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&self.path)?;
        self.verify(&file)?;
        Ok(file)
    }

    fn verify(&self, file: &File) -> io::Result<()> {
        if FileStamp::read(&file.metadata()?)? != self.stamp
            || FileStamp::read(&std::fs::symlink_metadata(&self.path)?)? != self.stamp
        {
            return Err(changed_file());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn reused_hash_buffer_preserves_exact_hashes_across_read_boundaries() {
        let temp = TempDir::new().unwrap();
        let mut buffer = vec![0xa5; 257];
        for length in [65_539, 1, 4097, 0, 256, 65_536, 255] {
            let contents: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let path = temp.path().join("file");
            fs::write(&path, &contents).unwrap();
            let info = FileInfo::read(&path).unwrap();
            let actual =
                compute_full_hash_with_buffer(&info, &CancellationToken::new(), &mut buffer)
                    .unwrap();
            assert_eq!(
                actual,
                ContentHash::new(*blake3::hash(&contents).as_bytes())
            );
        }
        assert_eq!(hash_buffer(0).len(), 1);
        assert_eq!(hash_buffer(256).len(), 256);
        assert_eq!(hash_buffer(u64::MAX).len(), 64 * 1024);
    }

    #[test]
    fn quick_and_full_reports_agree_across_partial_coverage_boundary() {
        for size in [0, 4, 8, 9, 64] {
            let root = TempDir::new().unwrap();
            let bytes = vec![b'x'; size];
            fs::write(root.path().join("a"), &bytes).unwrap();
            fs::write(root.path().join("b"), &bytes).unwrap();
            if size > 0 {
                let mut different = bytes.clone();
                different[size / 2] = b'y';
                fs::write(root.path().join("different"), different).unwrap();
            }
            let tree = gravityfile_scan::JwalkScanner::new()
                .scan(
                    &gravityfile_core::ScanConfig::new(root.path()),
                    CancellationToken::new(),
                )
                .unwrap();
            let config = DuplicateConfig {
                min_size: 0,
                partial_hash_head: 4,
                partial_hash_tail: 4,
                ..Default::default()
            };
            let quick = DuplicateFinder::with_config(config.clone())
                .unwrap()
                .find_duplicates(&tree, CancellationToken::new());
            let full = DuplicateFinder::with_config(DuplicateConfig {
                quick_compare: false,
                ..config
            })
            .unwrap()
            .find_duplicates(&tree, CancellationToken::new());
            assert_eq!(quick.status.state, full.status.state);
            assert_eq!(quick.files_analyzed, full.files_analyzed);
            assert_eq!(quick.total_wasted_space, full.total_wasted_space);
            assert_eq!(quick.total_duplicate_size, full.total_duplicate_size);
            assert_eq!(quick.files_with_duplicates, full.files_with_duplicates);
            assert_eq!(quick.group_count, full.group_count);
            assert_eq!(quick.groups_omitted, full.groups_omitted);
            let group_values = |report: &DuplicateReport| {
                report
                    .groups
                    .iter()
                    .map(|group| {
                        (
                            group.hash,
                            group.size,
                            group.paths.clone(),
                            group.wasted_bytes,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(group_values(&quick), group_values(&full));
            assert_eq!(quick.groups.len(), 1);
            assert_eq!(quick.groups[0].paths.len(), 2);
            assert_eq!(
                quick.groups[0].hash,
                ContentHash::new(*blake3::hash(&bytes).as_bytes())
            );
        }
    }

    #[test]
    fn fully_covered_candidates_still_reject_changed_observations() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("data");
        fs::write(&path, b"original").unwrap();
        let observed = FileInfo::read(&path).unwrap();
        fs::write(&path, b"different length").unwrap();
        let issues = IssueCollector::default();
        let finder = DuplicateFinder::new();
        assert!(
            finder
                .find_dups_in_size_group_partial(vec![observed], &CancellationToken::new(), &issues)
                .is_empty()
        );
        let tree = gravityfile_scan::JwalkScanner::new()
            .scan(
                &gravityfile_core::ScanConfig::new(root.path()),
                CancellationToken::new(),
            )
            .unwrap();
        let status = issues.finish(&tree, false);
        assert_eq!(status.state, crate::AnalysisState::Partial);
        assert_eq!(status.issue_count, 1);
    }

    #[test]
    fn invalid_analysis_scope_is_rejected_before_any_read() {
        assert!(
            DuplicateConfig::builder()
                .min_size(10u64)
                .max_size(1u64)
                .build()
                .is_err()
        );
        assert!(
            DuplicateConfig::builder()
                .exclude_patterns(vec!["[".into()])
                .build()
                .is_err()
        );
        assert!(
            DuplicateFinder::with_config(DuplicateConfig {
                min_size: 10,
                max_size: 1,
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            DuplicateFinder::with_config(DuplicateConfig {
                exclude_patterns: vec!["[".into()],
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn changed_file_is_rejected_between_hash_phases() {
        let temp = create_test_files();
        let path = temp.path().join("file1.txt");
        let observed = FileInfo::read(&path).unwrap();
        let token = CancellationToken::new();
        DuplicateFinder::new()
            .compute_partial_hash(&observed, &token)
            .unwrap();
        fs::write(path, b"replacement content with another length").unwrap();
        assert!(compute_full_hash(&observed, &token).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn substituted_symlink_is_rejected_without_hashing_target() {
        let temp = create_test_files();
        let path = temp.path().join("file1.txt");
        let observed = FileInfo::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(temp.path().join("file2.txt"), &path).unwrap();
        assert!(compute_full_hash(&observed, &CancellationToken::new()).is_err());
    }

    #[test]
    fn missing_candidates_produce_bounded_deterministic_diagnostics() {
        let temp = TempDir::new().unwrap();
        for i in 0..110 {
            fs::write(temp.path().join(format!("{i:03}")), b"duplicate").unwrap();
        }
        let tree = gravityfile_scan::JwalkScanner::new()
            .scan(
                &gravityfile_core::ScanConfig::new(temp.path()),
                CancellationToken::new(),
            )
            .unwrap();
        for i in 0..110 {
            fs::remove_file(temp.path().join(format!("{i:03}"))).unwrap();
        }
        let report = DuplicateFinder::new().find_duplicates(&tree, CancellationToken::new());
        assert_eq!(report.status.state, crate::AnalysisState::Partial);
        assert_eq!(report.status.issue_count, 110);
        assert_eq!(report.status.issues.len(), 100);
        assert_eq!(report.status.issues_omitted, 10);
        assert!(
            report
                .status
                .issues
                .windows(2)
                .all(|pair| pair[0].path < pair[1].path)
        );
        assert!(!report.has_duplicates());
    }

    #[test]
    fn cancelled_report_cannot_be_mistaken_for_a_complete_empty_report() {
        let temp = create_test_files();
        let tree = gravityfile_scan::JwalkScanner::new()
            .scan(
                &gravityfile_core::ScanConfig::new(temp.path()),
                CancellationToken::new(),
            )
            .unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let report = DuplicateFinder::new().find_duplicates(&tree, token);
        assert_eq!(report.status.state, crate::AnalysisState::Cancelled);
        assert!(!report.status.is_complete());
        assert_eq!(report.files_analyzed, 0);
    }

    #[test]
    fn display_limit_does_not_mark_a_successful_analysis_partial() {
        let temp = create_test_files();
        let tree = gravityfile_scan::JwalkScanner::new()
            .scan(
                &gravityfile_core::ScanConfig::new(temp.path()),
                CancellationToken::new(),
            )
            .unwrap();
        let report = DuplicateFinder::with_config(DuplicateConfig {
            min_size: 0,
            max_groups: 1,
            ..Default::default()
        })
        .unwrap()
        .find_duplicates(&tree, CancellationToken::new());
        assert!(report.status.is_complete());
        assert_eq!(report.groups.len(), 1);
        assert!(
            report.groups[0]
                .paths
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
    }

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

        let token = CancellationToken::new();
        let hash1 = compute_full_hash(
            &FileInfo::read(&temp.path().join("file1.txt")).unwrap(),
            &token,
        );
        let hash2 = compute_full_hash(
            &FileInfo::read(&temp.path().join("file2.txt")).unwrap(),
            &token,
        );
        let hash3 = compute_full_hash(
            &FileInfo::read(&temp.path().join("file3.txt")).unwrap(),
            &token,
        );

        assert!(hash1.is_ok());
        assert!(hash2.is_ok());
        assert!(hash3.is_ok());

        // file1 and file2 should have same hash
        assert_eq!(hash1.unwrap(), hash2.unwrap());
        // file3 should be different
        assert_ne!(
            compute_full_hash(
                &FileInfo::read(&temp.path().join("file1.txt")).unwrap(),
                &token
            )
            .unwrap(),
            hash3.unwrap()
        );
    }

    #[test]
    fn test_partial_hash() {
        let temp = create_test_files();
        let finder = DuplicateFinder::new();

        let hash1 = finder.compute_partial_hash(
            &FileInfo::read(&temp.path().join("file1.txt")).unwrap(),
            &CancellationToken::new(),
        );
        let hash2 = finder.compute_partial_hash(
            &FileInfo::read(&temp.path().join("file2.txt")).unwrap(),
            &CancellationToken::new(),
        );

        assert!(hash1.is_ok());
        assert_eq!(hash1.unwrap(), hash2.unwrap());
    }

    #[test]
    fn test_groups_omitted_field() {
        // Build a report with max_groups limiting the output
        let report = DuplicateReport {
            status: AnalysisStatus::default(),
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
        let finder = DuplicateFinder::with_config(config).unwrap();
        assert!(finder.is_excluded(Path::new("/some/path/debug.log")));
        assert!(!finder.is_excluded(Path::new("/some/path/main.rs")));
    }
}
