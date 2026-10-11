//! Core types and traits for gravityfile.
//!
//! This crate provides the fundamental data structures used throughout
//! the gravityfile ecosystem, including file nodes, trees, and configuration.
//!
//! # Overview
//!
//! `gravityfile-core` is the foundation crate containing shared types:
//!
//! - [`FileNode`] - Represents files, directories, and symlinks
//! - [`FileTree`] - Container for scanned directory trees
//! - [`ScanConfig`] - Configuration for scanning operations
//! - [`TreeStats`] - Summary statistics for a scanned tree
//! - [`ContentHash`] - BLAKE3 content hash for duplicate detection
//!
//! # Example
//!
//! ```rust
//! use gravityfile_core::{FileNode, Timestamps, ScanConfig};
//! use std::time::SystemTime;
//!
//! // Create a file node
//! let file = FileNode::new_file(
//!     "example.txt",
//!     1024,
//!     2,
//!     Timestamps::with_modified(SystemTime::now()),
//!     false,
//! );
//!
//! // Create scan configuration
//! let config = ScanConfig::builder()
//!     .root("/path/to/scan")
//!     .max_depth(Some(10))
//!     .include_hidden(false)
//!     .build()
//!     .unwrap();
//! ```

mod config;
mod error;
pub mod history;
mod node;
pub mod path_serde;
pub mod paths;
mod terminal;
mod tree;
pub mod units;

pub use config::{ScanConfig, ScanConfigBuilder};
pub use error::{ScanError, ScanWarning, WarningKind};
pub use node::{ContentHash, FileNode, GitStatus, InodeInfo, NodeKind, Timestamps};
pub use terminal::{escape_filename, escape_terminal_text};
pub use tree::{FileTree, TreeStats};
