//! Age-based file analysis.
//!
//! Provides analysis of file ages to help identify:
//! - Stale directories (no recent modifications)
//! - Age distribution of files
//! - Old files that may be candidates for cleanup

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use derive_builder::Builder;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::AnalysisStatus;
use gravityfile_core::{FileNode, FileTree, NodeKind};

/// Seconds per month, using the astronomically accurate 365.25/12 * 86400 value.
const SECS_PER_MONTH: u64 = 2_629_800;

/// An age bucket for categorizing files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgeBucket {
    /// Human-readable name for this bucket.
    pub name: String,
    /// Maximum age for files in this bucket.
    pub max_age: Duration,
}

impl AgeBucket {
    /// Create a new age bucket.
    pub fn new(name: impl Into<String>, max_age: Duration) -> Self {
        Self {
            name: name.into(),
            max_age,
        }
    }
}

/// Configuration for age-based analysis.
#[derive(Debug, Clone, Builder, Serialize)]
#[builder(setter(into), build_fn(validate = "Self::validate"))]
pub struct AgeConfig {
    /// Reference time for age calculations (default: now).
    #[builder(default = "SystemTime::now()")]
    pub reference_time: SystemTime,

    /// Age buckets for categorization.
    #[builder(default = "Self::default_buckets()")]
    pub buckets: Vec<AgeBucket>,

    /// Minimum age to consider a directory "stale".
    #[builder(default = "Duration::from_secs(365 * 24 * 60 * 60)")] // 1 year
    pub stale_threshold: Duration,

    /// Minimum size for a stale directory to be reported.
    #[builder(default = "1024 * 1024")] // 1 MB
    pub min_stale_size: u64,

    /// Maximum number of stale directories to report.
    #[builder(default = "100")]
    pub max_stale_dirs: usize,

    /// Number of largest files to track per bucket.
    #[builder(default = "10")]
    pub top_files_per_bucket: usize,
}

impl AgeConfigBuilder {
    fn validate(&self) -> Result<(), String> {
        if let Some(buckets) = &self.buckets {
            validate_buckets(buckets)?;
        }
        Ok(())
    }

    fn default_buckets() -> Vec<AgeBucket> {
        vec![
            AgeBucket::new("Today", Duration::from_secs(24 * 60 * 60)),
            AgeBucket::new("This Week", Duration::from_secs(7 * 24 * 60 * 60)),
            AgeBucket::new("This Month", Duration::from_secs(30 * 24 * 60 * 60)),
            AgeBucket::new("This Year", Duration::from_secs(365 * 24 * 60 * 60)),
            AgeBucket::new("Older", Duration::MAX),
        ]
    }
}

impl Default for AgeConfig {
    fn default() -> Self {
        Self {
            reference_time: SystemTime::now(),
            buckets: AgeConfigBuilder::default_buckets(),
            stale_threshold: Duration::from_secs(365 * 24 * 60 * 60),
            min_stale_size: 1024 * 1024,
            max_stale_dirs: 100,
            top_files_per_bucket: 10,
        }
    }
}

impl AgeConfig {
    /// Validate that every file age has exactly one unambiguous bucket.
    pub fn validate(&self) -> Result<(), String> {
        validate_buckets(&self.buckets)
    }

    /// Create a new config builder.
    pub fn builder() -> AgeConfigBuilder {
        AgeConfigBuilder::default()
    }
}

fn validate_buckets(buckets: &[AgeBucket]) -> Result<(), String> {
    if buckets.is_empty() {
        return Err("Age analysis requires at least one bucket".into());
    }
    let mut names = std::collections::HashSet::new();
    let mut boundaries = std::collections::HashSet::new();
    for bucket in buckets {
        if bucket.name.trim().is_empty() || !names.insert(bucket.name.as_str()) {
            return Err("Age bucket names must be nonempty and unique".into());
        }
        if !boundaries.insert(bucket.max_age) {
            return Err("Age bucket boundaries must be unique".into());
        }
    }
    if !boundaries.contains(&Duration::MAX) {
        return Err("Age buckets require a Duration::MAX catch-all bucket".into());
    }
    Ok(())
}

/// Statistics for an age bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgeBucketStats {
    /// Bucket name.
    pub name: String,
    /// Maximum age for this bucket.
    pub max_age: Duration,
    /// Number of files in this bucket.
    pub file_count: u64,
    /// Total size of files in this bucket.
    pub total_size: u64,
    /// Largest files in this bucket.
    #[serde(with = "gravityfile_core::path_serde::triples")]
    pub largest_files: Vec<(PathBuf, u64, SystemTime)>,
}

/// A directory identified as stale (no recent modifications).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleDirectory {
    /// Path to the directory.
    #[serde(with = "gravityfile_core::path_serde")]
    pub path: PathBuf,
    /// Total size of the directory.
    pub size: u64,
    /// Age of the newest file in the directory.
    pub newest_file_age: Duration,
    /// Number of files in the directory.
    pub file_count: u64,
}

/// Availability of stale-directory recommendations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleDirectoryStatus {
    /// Legacy reports did not record whether recommendations were evaluated.
    #[default]
    Unknown,
    /// Recommendations were evaluated within the observed filesystem scope.
    Evaluated,
    /// Scan warnings or cancellation prevent reliable recommendations.
    SuppressedIncompleteScan,
    /// Depth, hidden-file, or ignore filters may hide recent descendants.
    SuppressedFilteredScan,
}

impl std::fmt::Display for StaleDirectoryStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unknown => "unknown (legacy report)",
            Self::Evaluated => "evaluated",
            Self::SuppressedIncompleteScan => "unavailable: incomplete scan",
            Self::SuppressedFilteredScan => "unavailable: filtered scan",
        })
    }
}

/// Results from age analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgeReport {
    /// Completeness inherited from scan warnings and cancellation.
    #[serde(default)]
    pub status: AnalysisStatus,
    /// Whether stale-directory recommendations were evaluated.
    #[serde(default)]
    pub stale_directory_status: StaleDirectoryStatus,
    /// Statistics for each age bucket.
    pub buckets: Vec<AgeBucketStats>,
    /// Directories identified as stale.
    pub stale_directories: Vec<StaleDirectory>,
    /// Total files analyzed.
    pub total_files: u64,
    /// Total size analyzed.
    pub total_size: u64,
    /// Average file age.
    pub average_age: Duration,
    /// Median file age bucket name. None when the tree has no files.
    pub median_age_bucket: Option<String>,
}

impl AgeReport {
    /// Check if there are any stale directories.
    pub fn has_stale_directories(&self) -> bool {
        !self.stale_directories.is_empty()
    }

    /// Get total size of stale directories, saturating at the byte counter limit.
    pub fn total_stale_size(&self) -> u64 {
        self.stale_directories
            .iter()
            .map(|d| d.size)
            .fold(0u64, u64::saturating_add)
    }

    /// Get the bucket containing most files.
    pub fn largest_bucket(&self) -> Option<&AgeBucketStats> {
        self.buckets.iter().max_by_key(|b| b.file_count)
    }

    /// Get the bucket containing most size.
    pub fn largest_bucket_by_size(&self) -> Option<&AgeBucketStats> {
        self.buckets.iter().max_by_key(|b| b.total_size)
    }
}

/// Age-based file analyzer.
pub struct AgeAnalyzer {
    config: AgeConfig,
}

impl AgeAnalyzer {
    /// Create a new analyzer with default config.
    pub fn new() -> Self {
        Self {
            config: AgeConfig::default(),
        }
    }

    /// Create a new analyzer with custom config.
    ///
    /// Buckets are sorted by max_age ascending so the first bucket that fits is selected.
    pub fn with_config(mut config: AgeConfig) -> Result<Self, String> {
        config.validate()?;
        config.buckets.sort_by_key(|bucket| bucket.max_age);
        Ok(Self { config })
    }

    /// Analyze file ages in a tree.
    pub fn analyze(&self, tree: &FileTree, token: CancellationToken) -> AgeReport {
        let mut bucket_collectors: Vec<BucketCollector> = self
            .config
            .buckets
            .iter()
            .map(|b| BucketCollector::new(b.clone(), self.config.top_files_per_bucket))
            .collect();

        let mut stale_candidates = StaleCollector::new(self.config.max_stale_dirs);
        let mut total_files: u64 = 0;
        let mut total_size: u64 = 0;
        let mut total_age = AgeSum::default();

        // Single DFS pass: collect bucket stats and stale directory candidates together
        self.dfs(
            &tree.root,
            &tree.root_path,
            &mut bucket_collectors,
            &mut stale_candidates,
            &mut total_files,
            &mut total_size,
            &mut total_age,
            &token,
        );

        let mut stale_candidates = stale_candidates.finish();

        // Calculate average age
        let average_age = total_age.mean(total_files);

        // Find median bucket (bucket containing the cumulative 50th-percentile file).
        //
        // We use the 1-based rank of the lower median: rank = (total_files + 1) / 2.
        // This avoids the half_files = total_files / 2 = 0 trap when total_files == 1,
        // which would cause cumulative >= 0 to fire immediately on the first (possibly
        // empty) bucket rather than on the bucket that actually holds the file.
        let median_age_bucket = if token.is_cancelled() {
            None
        } else {
            let counts: Vec<(&str, u64)> = bucket_collectors
                .iter()
                .map(|c| (c.bucket.name.as_str(), c.file_count))
                .collect();
            median_bucket_name(&counts, total_files).map(str::to_owned)
        };

        // Convert collectors to stats
        let buckets: Vec<AgeBucketStats> = bucket_collectors
            .into_iter()
            .map(|c| {
                let BucketCollector {
                    bucket,
                    file_count,
                    total_size,
                    heap,
                    ..
                } = c;
                let mut largest_files: Vec<(PathBuf, u64, SystemTime)> = heap
                    .into_iter()
                    .map(|(Reverse(size), path, modified)| (path, size, modified))
                    .collect();
                largest_files
                    .sort_by(|a, b| (Reverse(a.1), &a.0, a.2).cmp(&(Reverse(b.1), &b.0, b.2)));
                AgeBucketStats {
                    name: bucket.name,
                    max_age: bucket.max_age,
                    file_count,
                    total_size,
                    largest_files,
                }
            })
            .collect();

        let status = AnalysisStatus::from_scan(tree, token.is_cancelled());
        // An unreadable or unvisited descendant may be recent. Never label its
        // ancestor stale based on only the old files we happened to observe.
        let stale_directory_status = if !status.is_complete() {
            StaleDirectoryStatus::SuppressedIncompleteScan
        } else if tree.config.max_depth.is_some()
            || !tree.config.include_hidden
            || !tree.config.ignore_patterns.is_empty()
        {
            StaleDirectoryStatus::SuppressedFilteredScan
        } else {
            StaleDirectoryStatus::Evaluated
        };
        if stale_directory_status != StaleDirectoryStatus::Evaluated {
            stale_candidates.clear();
        }
        AgeReport {
            status,
            stale_directory_status,
            buckets,
            stale_directories: stale_candidates,
            total_files,
            total_size,
            average_age,
            median_age_bucket,
        }
    }

    /// Single DFS pass that simultaneously:
    /// - Collects per-bucket file statistics
    /// - Accumulates global totals
    /// - Identifies stale directories
    ///
    /// Returns the newest modification time found within this subtree (used for stale detection).
    #[allow(clippy::too_many_arguments)]
    fn dfs(
        &self,
        node: &FileNode,
        current_path: &Path,
        bucket_collectors: &mut [BucketCollector],
        stale_dirs: &mut StaleCollector,
        total_files: &mut u64,
        total_size: &mut u64,
        total_age: &mut AgeSum,
        token: &CancellationToken,
    ) -> Option<SystemTime> {
        // Frames preserve post-order aggregation without consuming the call
        // stack. Each frame also holds stale descendants that may yet be
        // absorbed by a stale ancestor: only the outermost stale directory of
        // a stale subtree is reported, so no bytes are counted twice.
        let mut stack = vec![(
            node,
            current_path.to_path_buf(),
            0usize,
            None::<SystemTime>,
            Vec::<StaleCandidate>::new(),
        )];
        while let Some((node, path, next_child, newest, stale_below)) = stack.last_mut() {
            if token.is_cancelled() {
                return None;
            }
            let mut stale_up = Vec::new();
            let completed = match &node.kind {
                NodeKind::File { .. } => {
                    let modified = node.timestamps.modified;
                    let age = self
                        .config
                        .reference_time
                        .duration_since(modified)
                        .unwrap_or(Duration::ZERO);
                    *total_files += 1;
                    *total_size = total_size.saturating_add(node.size);
                    total_age.add(age);
                    // Config validation guarantees sorted, unique boundaries and
                    // a Duration::MAX catch-all. Find the first inclusive bound.
                    let bucket = bucket_collectors
                        .partition_point(|collector| collector.bucket.max_age < age);
                    bucket_collectors[bucket].add_file(&*path, node.size, modified);
                    Some(modified)
                }
                NodeKind::Directory { file_count, .. } => {
                    if let Some(child) = node.children.get(*next_child) {
                        *next_child += 1;
                        let child_path = path.join(child.file_name());
                        stack.push((child, child_path, 0, None, Vec::new()));
                        continue;
                    }
                    let below = std::mem::take(stale_below);
                    let age = newest.map(|modified| {
                        self.config
                            .reference_time
                            .duration_since(modified)
                            .unwrap_or(Duration::ZERO)
                    });
                    match age {
                        Some(age) if age >= self.config.stale_threshold => {
                            if node.size >= self.config.min_stale_size {
                                // This directory subsumes every stale one below it.
                                stale_up.push((path.clone(), node.size, age, *file_count));
                            } else {
                                stale_up = below;
                            }
                        }
                        // A recent file here means no ancestor can be stale,
                        // so the candidates below are final.
                        Some(_) => {
                            for (path, size, age, files) in below {
                                stale_dirs.add(&path, size, age, files);
                            }
                        }
                        None => stale_up = below,
                    }
                    *newest
                }
                _ => None,
            };
            stack.pop();
            if let Some((_, _, _, parent_newest, parent_stale)) = stack.last_mut() {
                *parent_newest = (*parent_newest).max(completed);
                parent_stale.append(&mut stale_up);
            } else {
                for (path, size, age, files) in stale_up {
                    stale_dirs.add(&path, size, age, files);
                }
                return completed;
            }
        }
        None
    }
}

/// `(path, size, newest file age, file count)` of a stale directory.
type StaleCandidate = (PathBuf, u64, Duration, u64);

/// Retain only the requested largest stale directories. The heap root is the
/// worst retained result: smallest size, then lexically greatest path.
struct StaleCollector {
    limit: usize,
    heap: BinaryHeap<(Reverse<u64>, PathBuf, Duration, u64)>,
}

impl StaleCollector {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::new(),
        }
    }

    fn add(&mut self, path: &Path, size: u64, age: Duration, files: u64) {
        if self.limit == 0 {
            return;
        }
        if self.heap.len() == self.limit {
            let (Reverse(worst_size), worst_path, _, _) =
                self.heap.peek().expect("nonempty bounded heap");
            if size < *worst_size || (size == *worst_size && path >= worst_path.as_path()) {
                return;
            }
            self.heap.pop();
        }
        self.heap
            .push((Reverse(size), path.to_path_buf(), age, files));
    }

    fn finish(self) -> Vec<StaleDirectory> {
        let mut result: Vec<_> = self
            .heap
            .into_iter()
            .map(
                |(Reverse(size), path, newest_file_age, file_count)| StaleDirectory {
                    path,
                    size,
                    newest_file_age,
                    file_count,
                },
            )
            .collect();
        result.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
        result
    }
}

/// Separate components avoid overflowing even for u64::MAX durations. A u128
/// nanosecond total alone cannot represent u64::MAX copies of Duration::MAX.
#[derive(Default)]
struct AgeSum {
    seconds: u128,
    nanos: u128,
}

impl AgeSum {
    fn add(&mut self, age: Duration) {
        self.seconds += u128::from(age.as_secs());
        self.nanos += u128::from(age.subsec_nanos());
    }

    fn mean(&self, count: u64) -> Duration {
        if count == 0 {
            return Duration::ZERO;
        }
        let count = u128::from(count);
        let seconds = self.seconds / count;
        let nanos = ((self.seconds % count) * 1_000_000_000 + self.nanos) / count;
        // The average cannot exceed the largest input Duration. Duration::new
        // carries any whole second accumulated by the fractional components.
        Duration::new(seconds as u64, nanos as u32)
    }
}

/// Pick the bucket holding the median file.
///
/// Uses the 1-based rank of the lower median, `(total + 1) / 2`. A plain
/// `total / 2` is 0 when there is a single file, which makes the running total
/// satisfy `cumulative >= 0` on the first (possibly empty) bucket rather than
/// the one actually holding it.
///
/// `buckets` must be ordered by ascending `max_age`, which
/// [`AgeAnalyzer::with_config`] guarantees.
fn median_bucket_name<'a>(buckets: &[(&'a str, u64)], total_files: u64) -> Option<&'a str> {
    if total_files == 0 {
        return None;
    }
    let median_rank = total_files.div_ceil(2);
    let mut cumulative = 0u64;
    for (name, count) in buckets {
        cumulative += count;
        if cumulative >= median_rank {
            return Some(name);
        }
    }
    // Fall back to the last bucket if rounding ever leaves nothing selected.
    buckets.last().map(|(name, _)| *name)
}

impl Default for AgeAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal struct for collecting bucket statistics using a BinaryHeap for O(log n) top-N.
///
/// The heap stores `(Reverse(size), path_index)` so the smallest element sits at the top,
/// allowing efficient eviction when a larger file arrives.
struct BucketCollector {
    bucket: AgeBucket,
    file_count: u64,
    total_size: u64,
    /// Max-N heap: stores (Reverse(size), path, modified) so minimum-size is cheaply ejected.
    heap: BinaryHeap<(Reverse<u64>, PathBuf, SystemTime)>,
    max_files: usize,
}

impl BucketCollector {
    fn new(bucket: AgeBucket, max_files: usize) -> Self {
        Self {
            bucket,
            file_count: 0,
            total_size: 0,
            heap: BinaryHeap::new(),
            max_files,
        }
    }

    fn add_file(&mut self, path: impl AsRef<Path>, size: u64, modified: SystemTime) {
        self.file_count += 1;
        self.total_size = self.total_size.saturating_add(size);

        if self.max_files == 0 {
            return;
        }

        let path = path.as_ref();
        if self.heap.len() == self.max_files {
            let mut worst = self.heap.peek_mut().expect("nonzero full heap");
            // Preserve the existing tuple ordering, including path/time ties.
            // Borrow the candidate path until it is known to belong in the heap.
            if (Reverse(size), path, modified) < (worst.0, worst.1.as_path(), worst.2) {
                *worst = (Reverse(size), path.to_path_buf(), modified);
            }
        } else {
            self.heap
                .push((Reverse(size), path.to_path_buf(), modified));
        }
    }

    /// Drain the heap into a Vec sorted by size descending. Used in tests.
    #[cfg(test)]
    fn into_sorted_vec(self) -> Vec<(PathBuf, u64, SystemTime)> {
        let mut v: Vec<(PathBuf, u64, SystemTime)> = self
            .heap
            .into_iter()
            .map(|(Reverse(size), path, modified)| (path, size, modified))
            .collect();
        v.sort_by(|a, b| (Reverse(a.1), &a.0, a.2).cmp(&(Reverse(b.1), &b.0, b.2)));
        v
    }
}

/// Format a duration as a human-readable string.
pub fn format_age(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs} seconds")
    } else if secs < 3600 {
        format!("{} minutes", secs / 60)
    } else if secs < 86400 {
        format!("{} hours", secs / 3600)
    } else if secs < SECS_PER_MONTH {
        format!("{} days", secs / 86400)
    } else if secs < 31_557_600 {
        // 365.25 * 86400
        format!("{} months", secs / SECS_PER_MONTH)
    } else {
        format!("{:.1} years", secs as f64 / 31_557_600.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_selection_is_bounded_and_matches_full_sort_in_both_orders() {
        let candidates: Vec<_> = (0..10_000)
            .map(|i| (PathBuf::from(format!("directory-{i:05}")), (i % 17) as u64))
            .collect();
        let mut expected = candidates.clone();
        expected.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for limit in [0, 1, 37, 10_001] {
            for reverse in [false, true] {
                let mut collector = StaleCollector::new(limit);
                for index in 0..candidates.len() {
                    let index = if reverse {
                        candidates.len() - index - 1
                    } else {
                        index
                    };
                    let (path, size) = &candidates[index];
                    collector.add(path, *size, Duration::from_secs(7), 3);
                    assert!(collector.heap.len() <= limit);
                }
                let actual = collector.finish();
                let actual_pairs: Vec<_> = actual
                    .iter()
                    .map(|dir| (dir.path.clone(), dir.size))
                    .collect();
                assert_eq!(actual_pairs, expected[..limit.min(expected.len())]);
                assert!(actual.iter().all(
                    |dir| dir.file_count == 3 && dir.newest_file_age == Duration::from_secs(7)
                ));
            }
        }
    }

    #[test]
    fn test_format_age() {
        assert_eq!(format_age(Duration::from_secs(30)), "30 seconds");
        assert_eq!(format_age(Duration::from_secs(120)), "2 minutes");
        assert_eq!(format_age(Duration::from_secs(7200)), "2 hours");
        assert_eq!(format_age(Duration::from_secs(172800)), "2 days");
    }

    #[test]
    fn test_format_age_month_boundary() {
        // 30 days = 2_592_000 secs; with old constant that was exactly 1 month, but with
        // SECS_PER_MONTH = 2_629_800 it should still be "days" range.
        let thirty_days = Duration::from_secs(30 * 86400);
        let result = format_age(thirty_days);
        assert!(
            result.ends_with("days"),
            "30 days should still be in 'days' range, got: {result}"
        );
    }

    #[test]
    fn test_age_bucket_creation() {
        let bucket = AgeBucket::new("Test", Duration::from_secs(3600));
        assert_eq!(bucket.name, "Test");
        assert_eq!(bucket.max_age, Duration::from_secs(3600));
    }

    #[test]
    fn test_default_config() {
        let config = AgeConfig::default();
        assert_eq!(config.buckets.len(), 5);
        assert_eq!(config.buckets[0].name, "Today");
        assert_eq!(config.buckets[4].name, "Older");
    }

    #[test]
    fn test_with_config_sorts_buckets() {
        // Provide buckets in reverse order; with_config should sort them ascending by max_age.
        let config = AgeConfig {
            buckets: vec![
                AgeBucket::new("Older", Duration::MAX),
                AgeBucket::new("Today", Duration::from_secs(86400)),
                AgeBucket::new("This Week", Duration::from_secs(7 * 86400)),
            ],
            ..AgeConfig::default()
        };
        let analyzer = AgeAnalyzer::with_config(config).unwrap();
        let names: Vec<&str> = analyzer
            .config
            .buckets
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(names, vec!["Today", "This Week", "Older"]);
    }

    // ---------------------------------------------------------------------------
    // Median algorithm correctness
    // ---------------------------------------------------------------------------

    /// Drives the real `median_bucket_name`, so a change to it is caught here.
    fn run_median(bucket_counts: &[(&str, u64)]) -> Option<String> {
        let total_files: u64 = bucket_counts.iter().map(|(_, count)| count).sum();
        median_bucket_name(bucket_counts, total_files).map(str::to_owned)
    }

    #[test]
    fn test_median_empty_tree_is_none() {
        assert!(run_median(&[("Today", 0), ("Older", 0)]).is_none());
    }

    #[test]
    fn test_median_single_file_in_last_bucket() {
        // With the old floor-division bug this returned "Today"; correct answer is "Older".
        let result = run_median(&[
            ("Today", 0),
            ("This Week", 0),
            ("This Month", 0),
            ("This Year", 0),
            ("Older", 1),
        ]);
        assert_eq!(result.as_deref(), Some("Older"));
    }

    #[test]
    fn test_median_single_file_in_first_bucket() {
        let result = run_median(&[("Today", 1), ("Older", 0)]);
        assert_eq!(result.as_deref(), Some("Today"));
    }

    #[test]
    fn test_median_two_files_one_each_end() {
        // 2 files: rank = (2+1)/2 = 1 → first non-empty bucket wins → "Today"
        let result = run_median(&[("Today", 1), ("Older", 1)]);
        assert_eq!(result.as_deref(), Some("Today"));
    }

    #[test]
    fn test_median_three_files_minority_first() {
        // 3 files: rank = (3+1)/2 = 2. "Today" has 1, "Older" has 2. cumulative
        // after "Today" = 1 < 2, after "Older" = 3 >= 2 → "Older".
        let result = run_median(&[("Today", 1), ("Older", 2)]);
        assert_eq!(result.as_deref(), Some("Older"));
    }

    #[test]
    fn test_median_four_files_split_evenly() {
        // 4 files: rank = (4+1)/2 = 2. "Today" has 2, cumulative = 2 >= 2 → "Today".
        let result = run_median(&[("Today", 2), ("Older", 2)]);
        assert_eq!(result.as_deref(), Some("Today"));
    }

    #[test]
    fn test_median_all_files_in_one_bucket() {
        let result = run_median(&[("Today", 5), ("Older", 0)]);
        assert_eq!(result.as_deref(), Some("Today"));
    }

    // ---------------------------------------------------------------------------
    // BinaryHeap top-N eviction correctness
    // ---------------------------------------------------------------------------

    #[test]
    fn bucket_extreme_limit_grows_only_with_observed_files() {
        let mut collector = BucketCollector::new(AgeBucket::new("all", Duration::MAX), usize::MAX);
        collector.add_file(Path::new("one"), 7, SystemTime::UNIX_EPOCH);
        assert_eq!(collector.file_count, 1);
        assert_eq!(collector.into_sorted_vec().len(), 1);
    }

    #[test]
    fn bucket_selection_matches_full_sort_with_path_and_time_ties() {
        let epoch = SystemTime::UNIX_EPOCH;
        let values = [
            (PathBuf::from("z"), 10, epoch),
            (PathBuf::from("a"), 10, epoch + Duration::from_secs(1)),
            (PathBuf::from("a"), 10, epoch),
            (PathBuf::from("large"), 20, epoch),
            (PathBuf::from("small"), 1, epoch),
        ];
        for limit in 0..=values.len() + 1 {
            for reverse in [false, true] {
                let mut input = values.to_vec();
                if reverse {
                    input.reverse();
                }
                let mut collector =
                    BucketCollector::new(AgeBucket::new("all", Duration::MAX), limit);
                for (path, size, time) in &input {
                    collector.add_file(path, *size, *time);
                }
                let actual = collector.into_sorted_vec();
                let mut expected = values.to_vec();
                expected.sort_by(|a, b| (Reverse(a.1), &a.0, a.2).cmp(&(Reverse(b.1), &b.0, b.2)));
                expected.truncate(limit);
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn test_bucket_collector_top_n_evicts_smallest() {
        let bucket = AgeBucket::new("Test", Duration::MAX);
        let now = SystemTime::now();
        let mut collector = BucketCollector::new(bucket, 3);

        collector.add_file(PathBuf::from("a"), 100, now);
        collector.add_file(PathBuf::from("b"), 500, now);
        collector.add_file(PathBuf::from("c"), 200, now);
        // 50 is smaller than the current minimum (100); it should be evicted immediately.
        collector.add_file(PathBuf::from("d"), 50, now);
        collector.add_file(PathBuf::from("e"), 300, now);

        let sorted = collector.into_sorted_vec();
        assert_eq!(sorted.len(), 3, "heap must respect max_files capacity");
        assert_eq!(sorted[0].1, 500, "largest file first");
        assert_eq!(sorted[1].1, 300);
        assert_eq!(sorted[2].1, 200);
        // 50 and 100 must not appear
        assert!(sorted.iter().all(|(_, s, _)| *s != 50 && *s != 100));
    }

    #[test]
    fn test_bucket_collector_top_n_fewer_than_capacity() {
        // Adding fewer files than max_files should keep them all.
        let bucket = AgeBucket::new("Test", Duration::MAX);
        let now = SystemTime::now();
        let mut collector = BucketCollector::new(bucket, 5);

        collector.add_file(PathBuf::from("a"), 100, now);
        collector.add_file(PathBuf::from("b"), 200, now);

        let sorted = collector.into_sorted_vec();
        assert_eq!(sorted.len(), 2);
        assert_eq!(sorted[0].1, 200);
        assert_eq!(sorted[1].1, 100);
    }

    #[test]
    fn test_bucket_collector_top_n_zero_capacity() {
        // max_files = 0 means "don't track largest files"; heap must stay empty.
        let bucket = AgeBucket::new("Test", Duration::MAX);
        let now = SystemTime::now();
        let mut collector = BucketCollector::new(bucket, 0);

        collector.add_file(PathBuf::from("a"), 999, now);
        collector.add_file(PathBuf::from("b"), 1, now);

        // file_count and total_size still accumulate
        assert_eq!(collector.file_count, 2);
        assert_eq!(collector.total_size, 1000);
        assert!(
            collector.heap.is_empty(),
            "heap must remain empty when max_files=0"
        );
    }

    #[test]
    fn test_bucket_collector_top_n_duplicate_sizes() {
        // Ties in size: all three should be retained when capacity allows.
        let bucket = AgeBucket::new("Test", Duration::MAX);
        let now = SystemTime::now();
        let mut collector = BucketCollector::new(bucket, 3);

        collector.add_file(PathBuf::from("a"), 100, now);
        collector.add_file(PathBuf::from("b"), 100, now);
        collector.add_file(PathBuf::from("c"), 100, now);
        collector.add_file(PathBuf::from("d"), 100, now); // 4th with same size, one evicted

        let sorted = collector.into_sorted_vec();
        assert_eq!(sorted.len(), 3);
        assert!(sorted.iter().all(|(_, s, _)| *s == 100));
    }

    #[test]
    fn age_mean_preserves_large_and_fractional_durations() {
        let mut sum = AgeSum::default();
        sum.add(Duration::MAX);
        sum.add(Duration::MAX);
        assert_eq!(sum.mean(2), Duration::MAX);
        let mut sum = AgeSum::default();
        sum.add(Duration::new(1, 900_000_000));
        sum.add(Duration::new(2, 900_000_000));
        assert_eq!(sum.mean(2), Duration::new(2, 400_000_000));
        assert_eq!(AgeSum::default().mean(0), Duration::ZERO);
        let count = u128::from(u64::MAX);
        let sum = AgeSum {
            seconds: count * u128::from(u64::MAX),
            nanos: count * 999_999_999,
        };
        assert_eq!(sum.mean(u64::MAX), Duration::MAX);
    }
}
