#![doc = include_str!("../README.md")]

mod git;
mod inode;
pub mod ncdu;
mod progress;
mod scanner;
pub mod snapshot;

pub use git::{GitStatusCache, apply_git_status};
pub use inode::InodeTracker;
pub use progress::ScanProgress;
pub use scanner::{JwalkScanner, ScannedEntry, quick_list, scan_entry};

// Re-export core types for convenience
pub use gravityfile_core::{
    FileNode, FileTree, NodeKind, ScanConfig, ScanError, ScanWarning, Timestamps, TreeStats,
    WarningKind,
};
