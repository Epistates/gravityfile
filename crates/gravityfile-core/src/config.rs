//! Scan configuration types.

use std::path::PathBuf;

use derive_builder::Builder;
use serde::{Deserialize, Serialize};

/// Configuration for scanning operations.
#[derive(Debug, Clone, Builder, Serialize, Deserialize)]
#[builder(setter(into), build_fn(validate = "Self::validate"))]
pub struct ScanConfig {
    /// Root path to scan.
    pub root: PathBuf,

    /// Follow symbolic links.
    #[builder(default = "false")]
    #[serde(default)]
    pub follow_symlinks: bool,

    /// Cross filesystem boundaries.
    #[builder(default = "false")]
    #[serde(default)]
    pub cross_filesystems: bool,

    /// Use apparent size vs disk usage.
    #[builder(default = "false")]
    #[serde(default)]
    pub apparent_size: bool,

    /// Maximum depth to traverse (None = unlimited).
    #[builder(default)]
    #[serde(default)]
    pub max_depth: Option<u32>,

    /// Patterns to ignore (gitignore syntax).
    #[builder(default)]
    #[serde(default)]
    pub ignore_patterns: Vec<String>,

    /// Number of threads for scanning (0 = auto-detect).
    #[builder(default = "0")]
    #[serde(default)]
    pub threads: usize,

    /// Include hidden files (starting with .).
    #[builder(default = "true")]
    #[serde(default = "default_true")]
    pub include_hidden: bool,

    /// Compute content hashes during scan.
    #[builder(default = "false")]
    #[serde(default)]
    pub compute_hashes: bool,

    /// Minimum file size to hash (skip tiny files).
    #[builder(default = "4096")]
    #[serde(default = "default_min_hash_size")]
    pub min_hash_size: u64,
}

fn default_true() -> bool {
    true
}

fn default_min_hash_size() -> u64 {
    4096
}

impl ScanConfigBuilder {
    fn validate(&self) -> Result<(), String> {
        if let Some(ref root) = self.root {
            if root.as_os_str().is_empty() {
                return Err("Root path cannot be empty".to_string());
            }
        } else {
            return Err("Root path is required".to_string());
        }
        Ok(())
    }
}

impl ScanConfig {
    /// Create a new scan config builder.
    pub fn builder() -> ScanConfigBuilder {
        ScanConfigBuilder::default()
    }

    /// Create a simple config for scanning a path.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            follow_symlinks: false,
            cross_filesystems: false,
            apparent_size: false,
            max_depth: None,
            ignore_patterns: Vec::new(),
            threads: 0,
            include_hidden: true,
            compute_hashes: false,
            min_hash_size: 4096,
        }
    }

    /// Check if a path should be ignored based on patterns.
    pub fn should_ignore(&self, name: &str) -> bool {
        // Simple pattern matching for now
        // TODO: Use gitignore-style matching
        for pattern in &self.ignore_patterns {
            if name == pattern {
                return true;
            }
            // Handle glob patterns
            if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len() - 1];
                if name.starts_with(prefix) {
                    return true;
                }
            }
            if pattern.starts_with('*') {
                let suffix = &pattern[1..];
                if name.ends_with(suffix) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if hidden files should be skipped.
    pub fn should_skip_hidden(&self, name: &str) -> bool {
        !self.include_hidden && name.starts_with('.')
    }
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self::new(".")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = ScanConfig::builder()
            .root("/home/user")
            .threads(4usize)
            .follow_symlinks(true)
            .build()
            .unwrap();

        assert_eq!(config.root, PathBuf::from("/home/user"));
        assert_eq!(config.threads, 4);
        assert!(config.follow_symlinks);
    }

    #[test]
    fn test_config_simple() {
        let config = ScanConfig::new("/home/user");
        assert_eq!(config.root, PathBuf::from("/home/user"));
        assert!(!config.follow_symlinks);
        assert_eq!(config.threads, 0);
    }

    #[test]
    fn test_should_ignore() {
        let config = ScanConfig::builder()
            .root("/test")
            .ignore_patterns(vec!["node_modules".to_string(), "*.log".to_string()])
            .build()
            .unwrap();

        assert!(config.should_ignore("node_modules"));
        assert!(config.should_ignore("test.log"));
        assert!(!config.should_ignore("src"));
    }

    #[test]
    fn test_should_skip_hidden() {
        let mut config = ScanConfig::new("/test");

        // By default, hidden files are included
        assert!(!config.should_skip_hidden(".git"));

        // When hidden files are excluded
        config.include_hidden = false;
        assert!(config.should_skip_hidden(".git"));
        assert!(!config.should_skip_hidden("src"));
    }
}
