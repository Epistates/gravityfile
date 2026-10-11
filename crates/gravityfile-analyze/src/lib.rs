#![doc = include_str!("../README.md")]

pub mod age;
mod breakdown;
mod diff;
mod duplicates;
mod status;

pub use status::{AnalysisIssue, AnalysisState, AnalysisStatus};

pub use age::{
    AgeAnalyzer, AgeBucket, AgeBucketStats, AgeConfig, AgeReport, StaleDirectory,
    StaleDirectoryStatus, format_age,
};
pub use breakdown::{LargeFile, TypeStats, by_type, largest_files};
pub use diff::{Change, DiffEntry, DiffOptions, Omitted as DiffOmitted, TreeDiff, diff};
pub use duplicates::{
    DuplicateConfig, DuplicateFinder, DuplicateGroup, DuplicateReport, DuplicateTimings,
    HashProgress,
};

// Re-export core types
pub use gravityfile_core::{ContentHash, FileNode, FileTree};
