//! File system scanning engine for gravityfile.
//!
//! This crate provides high-performance parallel directory scanning
//! using jwalk for traversal.
//!
//! # Overview
//!
//! `gravityfile-scan` is responsible for traversing directories and building
//! the file tree structure. Key features:
//!
//! - **Parallel traversal** via jwalk/rayon
//! - **Progress updates** via broadcast channels
//! - **Hardlink detection** to avoid double-counting
//! - **Configurable** depth limits, ignore patterns, etc.
//!
//! # Example
//!
//! ```rust,no_run
//! use gravityfile_scan::{JwalkScanner, ScanConfig};
//!
//! let config = ScanConfig::new("/path/to/scan");
//! let scanner = JwalkScanner::new();
//! let tree = scanner.scan(&config).unwrap();
//!
//! println!("Total size: {} bytes", tree.total_size());
//! println!("Total files: {}", tree.total_files());
//! ```
//!
//! # Progress Monitoring
//!
//! Subscribe to real-time progress updates:
//!
//! ```rust,no_run
//! use gravityfile_scan::{JwalkScanner, ScanConfig};
//!
//! let scanner = JwalkScanner::new();
//! let mut progress_rx = scanner.subscribe();
//!
//! // Handle progress in a separate task
//! tokio::spawn(async move {
//!     while let Ok(progress) = progress_rx.recv().await {
//!         println!("Scanned {} files", progress.files_scanned);
//!     }
//! });
//! ```

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
