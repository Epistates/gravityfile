//! Analysis algorithms for gravityfile.
//!
//! This crate provides analysis capabilities for scanned file trees:
//!
//! - **Duplicate detection** - Find duplicate files using BLAKE3 hashing
//! - **Age analysis** - Categorize files by age, find stale directories
//!
//! # Duplicate Detection
//!
//! Uses a three-phase algorithm for efficiency:
//!
//! 1. Group files by size (instant)
//! 2. Compute partial hash (first + last 4KB) for size-matched files
//! 3. Compute full BLAKE3 hash for partial-hash matches
//!
//! ```rust,ignore
//! use gravityfile_analyze::{DuplicateFinder, DuplicateConfig};
//! use gravityfile_scan::{JwalkScanner, ScanConfig};
//!
//! let scan_config = ScanConfig::new("/path/to/scan");
//! let tree = JwalkScanner::new().scan(&scan_config).unwrap();
//!
//! let finder = DuplicateFinder::new();
//! let report = finder.find_duplicates(&tree);
//!
//! println!("Found {} duplicate groups", report.group_count);
//! println!("Wasted space: {} bytes", report.total_wasted_space);
//! ```
//!
//! # Age Analysis
//!
//! Categorizes files into age buckets and identifies stale directories:
//!
//! ```rust,ignore
//! use gravityfile_analyze::{AgeAnalyzer, format_age};
//! use gravityfile_scan::{JwalkScanner, ScanConfig};
//!
//! let scan_config = ScanConfig::new("/path/to/scan");
//! let tree = JwalkScanner::new().scan(&scan_config).unwrap();
//!
//! let analyzer = AgeAnalyzer::new();
//! let report = analyzer.analyze(&tree);
//!
//! for bucket in &report.buckets {
//!     println!("{}: {} files", bucket.name, bucket.file_count);
//! }
//!
//! for dir in &report.stale_directories {
//!     println!("Stale: {} ({} old)", dir.path.display(), format_age(dir.newest_file_age));
//! }
//! ```

pub mod age;
mod duplicates;

pub use age::{AgeBucket, AgeConfig, AgeReport, AgeBucketStats, StaleDirectory, AgeAnalyzer, format_age};
pub use duplicates::{
    DuplicateConfig, DuplicateFinder, DuplicateGroup, DuplicateReport, HashProgress,
};

// Re-export core types
pub use gravityfile_core::{FileNode, FileTree, ContentHash};
