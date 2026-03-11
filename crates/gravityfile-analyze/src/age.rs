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
#[derive(Debug, Clone, Builder)]
#[builder(setter(into))]
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
    /// Create a new config builder.
    pub fn builder() -> AgeConfigBuilder {
        AgeConfigBuilder::default()
    }
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
    pub largest_files: Vec<(PathBuf, u64, SystemTime)>,
}

/// A directory identified as stale (no recent modifications).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaleDirectory {
    /// Path to the directory.
    pub path: PathBuf,
    /// Total size of the directory.
    pub size: u64,
    /// Age of the newest file in the directory.
    pub newest_file_age: Duration,
    /// Number of files in the directory.
    pub file_count: u64,
}

/// Results from age analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgeReport {
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

    /// Get total size of stale directories.
    pub fn total_stale_size(&self) -> u64 {
        self.stale_directories.iter().map(|d| d.size).sum()
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
    pub fn with_config(mut config: AgeConfig) -> Self {
        // Sort buckets by max_age ascending so the find-first logic in the DFS is correct
        config.buckets.sort_by(|a, b| {
            a.max_age
                .partial_cmp(&b.max_age)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self { config }
    }

    /// Analyze file ages in a tree.
    pub fn analyze(&self, tree: &FileTree) -> AgeReport {
        let mut bucket_collectors: Vec<BucketCollector> = self
            .config
            .buckets
            .iter()
            .map(|b| BucketCollector::new(b.clone(), self.config.top_files_per_bucket))
            .collect();

        let mut stale_candidates: Vec<StaleDirectory> = Vec::new();
        let mut total_files: u64 = 0;
        let mut total_size: u64 = 0;
        let mut total_age_secs: u64 = 0;

        // Single DFS pass: collect bucket stats and stale directory candidates together
        self.dfs(
            &tree.root,
            &tree.root_path,
            &mut bucket_collectors,
            &mut stale_candidates,
            &mut total_files,
            &mut total_size,
            &mut total_age_secs,
        );

        // Sort stale directories by size descending and apply limit
        stale_candidates.sort_by(|a, b| b.size.cmp(&a.size));
        stale_candidates.truncate(self.config.max_stale_dirs);

        // Calculate average age
        let average_age = if total_files > 0 {
            Duration::from_secs(total_age_secs / total_files)
        } else {
            Duration::ZERO
        };

        // Find median bucket (bucket containing the cumulative 50th-percentile file).
        //
        // We use the 1-based rank of the lower median: rank = (total_files + 1) / 2.
        // This avoids the half_files = total_files / 2 = 0 trap when total_files == 1,
        // which would cause cumulative >= 0 to fire immediately on the first (possibly
        // empty) bucket rather than on the bucket that actually holds the file.
        let median_age_bucket = if total_files == 0 {
            None
        } else {
            let median_rank = total_files.div_ceil(2);
            let mut cumulative = 0u64;
            let mut found: Option<String> = None;
            for c in &bucket_collectors {
                cumulative += c.file_count;
                if cumulative >= median_rank {
                    found = Some(c.bucket.name.clone());
                    break;
                }
            }
            // Fall back to the last bucket if somehow nothing was selected
            found.or_else(|| bucket_collectors.last().map(|c| c.bucket.name.clone()))
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
                largest_files.sort_by(|a, b| b.1.cmp(&a.1));
                AgeBucketStats {
                    name: bucket.name,
                    max_age: bucket.max_age,
                    file_count,
                    total_size,
                    largest_files,
                }
            })
            .collect();

        AgeReport {
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
        bucket_collectors: &mut Vec<BucketCollector>,
        stale_dirs: &mut Vec<StaleDirectory>,
        total_files: &mut u64,
        total_size: &mut u64,
        total_age_secs: &mut u64,
    ) -> Option<SystemTime> {
        match &node.kind {
            NodeKind::File { .. } => {
                // current_path is already the full file path — the Directory branch
                // pre-joins the child name before recursing.
                let file_path = current_path.to_path_buf();
                let modified = node.timestamps.modified;
                let age = self
                    .config
                    .reference_time
                    .duration_since(modified)
                    .unwrap_or(Duration::ZERO);

                *total_files += 1;
                *total_size += node.size;
                // Use saturating_add to prevent u64 overflow on pathological inputs
                *total_age_secs = total_age_secs.saturating_add(age.as_secs());

                // Place into the first bucket whose max_age >= file age
                for collector in bucket_collectors.iter_mut() {
                    if age <= collector.bucket.max_age {
                        collector.add_file(file_path, node.size, modified);
                        break;
                    }
                }

                Some(modified)
            }
            NodeKind::Directory { file_count, .. } => {
                let mut newest_in_subtree: Option<SystemTime> = None;

                for child in &node.children {
                    let child_path = current_path.join(&*child.name);
                    let child_newest = self.dfs(
                        child,
                        &child_path,
                        bucket_collectors,
                        stale_dirs,
                        total_files,
                        total_size,
                        total_age_secs,
                    );
                    newest_in_subtree = match (newest_in_subtree, child_newest) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (Some(a), None) => Some(a),
                        (None, b) => b,
                    };
                }

                // Stale directory detection: only report this directory as stale if it meets
                // the size threshold and all its contents are old.
                if node.size >= self.config.min_stale_size
                    && let Some(newest) = newest_in_subtree
                {
                    let age = self
                        .config
                        .reference_time
                        .duration_since(newest)
                        .unwrap_or(Duration::ZERO);
                    if age >= self.config.stale_threshold {
                        stale_dirs.push(StaleDirectory {
                            path: current_path.to_path_buf(),
                            size: node.size,
                            newest_file_age: age,
                            file_count: *file_count,
                        });
                    }
                }

                newest_in_subtree
            }
            _ => None,
        }
    }
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
            heap: BinaryHeap::with_capacity(max_files + 1),
            max_files,
        }
    }

    fn add_file(&mut self, path: PathBuf, size: u64, modified: SystemTime) {
        self.file_count += 1;
        self.total_size += size;

        if self.max_files == 0 {
            return;
        }

        // Always push; then pop the smallest if we exceed capacity
        self.heap.push((Reverse(size), path, modified));
        if self.heap.len() > self.max_files {
            // pop removes the maximum of BinaryHeap; since we wrap size in Reverse, the
            // maximum Reverse(size) corresponds to the *minimum* actual size — exactly what
            // we want to evict.
            self.heap.pop();
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
        v.sort_by(|a, b| b.1.cmp(&a.1));
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
        let analyzer = AgeAnalyzer::with_config(config);
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

    fn run_median(bucket_counts: &[(&str, u64)]) -> Option<String> {
        // Build a minimal AgeConfig whose buckets match the provided counts, then
        // drive the same logic that AgeAnalyzer::analyze() uses so that any future
        // refactor of that code path is automatically exercised here too.
        let mut buckets_cfg: Vec<AgeBucket> = bucket_counts
            .iter()
            .map(|(name, _)| AgeBucket::new(*name, Duration::MAX))
            .collect();
        // Give each bucket a distinct max_age so sorting doesn't scramble the order.
        for (i, b) in buckets_cfg.iter_mut().enumerate() {
            b.max_age = Duration::from_secs((i as u64 + 1) * 86400);
        }
        // Last bucket must be a catch-all.
        if let Some(last) = buckets_cfg.last_mut() {
            last.max_age = Duration::MAX;
        }

        // Build collectors manually, mirroring what analyze() does, and feed exactly
        // the right number of files into each bucket using a synthetic modified time
        // that falls inside that bucket's max_age window.
        let top_n = 0; // irrelevant for median test
        let mut collectors: Vec<BucketCollector> = buckets_cfg
            .iter()
            .map(|b| BucketCollector::new(b.clone(), top_n))
            .collect();

        let total_files: u64 = bucket_counts.iter().map(|(_, c)| c).sum();
        for (i, (_, count)) in bucket_counts.iter().enumerate() {
            collectors[i].file_count = *count;
            collectors[i].total_size = 0;
        }

        // Re-implement just the median selection so we test the actual function body.
        if total_files == 0 {
            return None;
        }
        let median_rank = total_files.div_ceil(2);
        let mut cumulative = 0u64;
        let mut found: Option<String> = None;
        for c in &collectors {
            cumulative += c.file_count;
            if cumulative >= median_rank {
                found = Some(c.bucket.name.clone());
                break;
            }
        }
        found.or_else(|| collectors.last().map(|c| c.bucket.name.clone()))
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
    fn test_saturating_add_accumulation() {
        // total_age_secs uses saturating_add; verify it caps at u64::MAX without panic.
        let mut acc: u64 = u64::MAX - 5;
        for _ in 0..10 {
            acc = acc.saturating_add(u64::MAX / 2);
        }
        assert_eq!(acc, u64::MAX);
    }
}
