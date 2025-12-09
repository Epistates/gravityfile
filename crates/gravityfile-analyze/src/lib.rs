//! Analysis algorithms for gravityfile.
//!
//! This crate provides analysis capabilities including:
//! - Duplicate file detection
//! - Age-based analysis
//! - Growth/trend analysis

pub mod age;
mod duplicates;

pub use age::{AgeBucket, AgeConfig, AgeReport, AgeBucketStats, StaleDirectory, AgeAnalyzer, format_age};
pub use duplicates::{
    DuplicateConfig, DuplicateFinder, DuplicateGroup, DuplicateReport, HashProgress,
};

// Re-export core types
pub use gravityfile_core::{FileNode, FileTree, ContentHash};
