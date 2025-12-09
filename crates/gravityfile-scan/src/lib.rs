//! File system scanning engine for gravityfile.
//!
//! This crate provides high-performance parallel directory scanning
//! using jwalk for traversal.

mod inode;
mod progress;
mod scanner;

pub use inode::InodeTracker;
pub use progress::ScanProgress;
pub use scanner::JwalkScanner;

// Re-export core types for convenience
pub use gravityfile_core::{
    FileNode, FileTree, NodeId, NodeKind, ScanConfig, ScanError, ScanWarning, Timestamps,
    TreeStats, WarningKind,
};
