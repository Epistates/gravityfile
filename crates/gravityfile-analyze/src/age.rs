//! Age-based file analysis.
//!
//! Provides analysis of file ages to help identify:
//! - Stale directories (no recent modifications)
//! - Age distribution of files
//! - Old files that may be candidates for cleanup

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use derive_builder::Builder;
use serde::{Deserialize, Serialize};

use gravityfile_core::{FileNode, FileTree, NodeKind};

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
    /// Median file age (approximate).
    pub median_age_bucket: String,
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
    pub fn with_config(config: AgeConfig) -> Self {
        Self { config }
    }

    /// Analyze file ages in a tree.
    pub fn analyze(&self, tree: &FileTree) -> AgeReport {
        let mut bucket_stats: Vec<BucketCollector> = self
            .config
            .buckets
            .iter()
            .map(|b| BucketCollector::new(b.clone(), self.config.top_files_per_bucket))
            .collect();

        let mut stale_candidates: Vec<StaleDirectory> = Vec::new();
        let mut total_files: u64 = 0;
        let mut total_size: u64 = 0;
        let mut total_age_secs: u64 = 0;

        // Analyze all files
        self.analyze_node(
            &tree.root,
            &tree.root_path,
            &mut bucket_stats,
            &mut total_files,
            &mut total_size,
            &mut total_age_secs,
        );

        // Find stale directories
        self.find_stale_directories(&tree.root, &tree.root_path, &mut stale_candidates);

        // Sort stale directories by size
        stale_candidates.sort_by(|a, b| b.size.cmp(&a.size));
        stale_candidates.truncate(self.config.max_stale_dirs);

        // Calculate average age
        let average_age = if total_files > 0 {
            Duration::from_secs(total_age_secs / total_files)
        } else {
            Duration::ZERO
        };

        // Find median bucket (bucket containing cumulative 50% of files)
        let half_files = total_files / 2;
        let mut cumulative = 0u64;
        let mut median_bucket = self.config.buckets.first().map(|b| b.name.clone()).unwrap_or_default();
        for stats in &bucket_stats {
            cumulative += stats.file_count;
            if cumulative >= half_files {
                median_bucket = stats.bucket.name.clone();
                break;
            }
        }

        // Convert collectors to stats
        let buckets: Vec<AgeBucketStats> = bucket_stats
            .into_iter()
            .map(|c| AgeBucketStats {
                name: c.bucket.name,
                max_age: c.bucket.max_age,
                file_count: c.file_count,
                total_size: c.total_size,
                largest_files: c.largest_files,
            })
            .collect();

        AgeReport {
            buckets,
            stale_directories: stale_candidates,
            total_files,
            total_size,
            average_age,
            median_age_bucket: median_bucket,
        }
    }

    /// Analyze a single node and its children.
    fn analyze_node(
        &self,
        node: &FileNode,
        current_path: &Path,
        bucket_stats: &mut [BucketCollector],
        total_files: &mut u64,
        total_size: &mut u64,
        total_age_secs: &mut u64,
    ) {
        match &node.kind {
            NodeKind::File { .. } => {
                let age = self
                    .config
                    .reference_time
                    .duration_since(node.timestamps.modified)
                    .unwrap_or(Duration::ZERO);

                *total_files += 1;
                *total_size += node.size;
                *total_age_secs += age.as_secs();

                // Find the appropriate bucket
                for collector in bucket_stats.iter_mut() {
                    if age <= collector.bucket.max_age {
                        collector.add_file(current_path.to_path_buf(), node.size, node.timestamps.modified);
                        break;
                    }
                }
            }
            NodeKind::Directory { .. } => {
                for child in &node.children {
                    let child_path = current_path.join(&*child.name);
                    self.analyze_node(
                        child,
                        &child_path,
                        bucket_stats,
                        total_files,
                        total_size,
                        total_age_secs,
                    );
                }
            }
            _ => {}
        }
    }

    /// Find stale directories (all contents older than threshold).
    fn find_stale_directories(
        &self,
        node: &FileNode,
        current_path: &Path,
        stale_dirs: &mut Vec<StaleDirectory>,
    ) {
        if let NodeKind::Directory { file_count, .. } = &node.kind {
            // Skip if directory is too small
            if node.size < self.config.min_stale_size {
                // Still recurse to find stale subdirectories
                for child in &node.children {
                    if child.is_dir() {
                        let child_path = current_path.join(&*child.name);
                        self.find_stale_directories(child, &child_path, stale_dirs);
                    }
                }
                return;
            }

            // Find the newest file in this directory
            let newest_modified = self.find_newest_file(node);

            if let Some(newest) = newest_modified {
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
                    // Don't recurse into children - we've already captured this as stale
                    return;
                }
            }

            // Recurse into children
            for child in &node.children {
                if child.is_dir() {
                    let child_path = current_path.join(&*child.name);
                    self.find_stale_directories(child, &child_path, stale_dirs);
                }
            }
        }
    }

    /// Find the newest file modification time in a subtree.
    fn find_newest_file(&self, node: &FileNode) -> Option<SystemTime> {
        match &node.kind {
            NodeKind::File { .. } => Some(node.timestamps.modified),
            NodeKind::Directory { .. } => {
                let mut newest: Option<SystemTime> = None;
                for child in &node.children {
                    if let Some(child_newest) = self.find_newest_file(child) {
                        newest = Some(match newest {
                            Some(current) => current.max(child_newest),
                            None => child_newest,
                        });
                    }
                }
                newest
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

/// Internal struct for collecting bucket statistics.
struct BucketCollector {
    bucket: AgeBucket,
    file_count: u64,
    total_size: u64,
    largest_files: Vec<(PathBuf, u64, SystemTime)>,
    max_files: usize,
}

impl BucketCollector {
    fn new(bucket: AgeBucket, max_files: usize) -> Self {
        Self {
            bucket,
            file_count: 0,
            total_size: 0,
            largest_files: Vec::with_capacity(max_files),
            max_files,
        }
    }

    fn add_file(&mut self, path: PathBuf, size: u64, modified: SystemTime) {
        self.file_count += 1;
        self.total_size += size;

        // Track largest files
        if self.largest_files.len() < self.max_files {
            self.largest_files.push((path, size, modified));
            self.largest_files.sort_by(|a, b| b.1.cmp(&a.1));
        } else if let Some(smallest) = self.largest_files.last() {
            if size > smallest.1 {
                self.largest_files.pop();
                self.largest_files.push((path, size, modified));
                self.largest_files.sort_by(|a, b| b.1.cmp(&a.1));
            }
        }
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
    } else if secs < 2592000 {
        format!("{} days", secs / 86400)
    } else if secs < 31536000 {
        format!("{} months", secs / 2592000)
    } else {
        format!("{:.1} years", secs as f64 / 31536000.0)
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
}
