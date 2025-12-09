//! Core types and traits for gravityfile.
//!
//! This crate provides the fundamental data structures used throughout
//! the gravityfile ecosystem, including file nodes, trees, and configuration.

mod config;
mod error;
mod node;
mod tree;

pub use config::{ScanConfig, ScanConfigBuilder};
pub use error::{ScanError, ScanWarning, WarningKind};
pub use node::{ContentHash, FileNode, InodeInfo, NodeId, NodeKind, Timestamps};
pub use tree::{FileTree, TreeStats};
