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
//! use gravityfile_core::{FileNode, NodeId, Timestamps, ScanConfig};
//! use std::time::SystemTime;
//!
//! // Create a file node
//! let file = FileNode::new_file(
//!     NodeId::new(1),
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
mod node;
mod tree;

pub use config::{ScanConfig, ScanConfigBuilder};
pub use error::{ScanError, ScanWarning, WarningKind};
pub use node::{ContentHash, FileNode, InodeInfo, NodeId, NodeKind, Timestamps};
pub use tree::{FileTree, TreeStats};
