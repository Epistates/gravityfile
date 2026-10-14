#![doc = include_str!("../README.md")]

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
