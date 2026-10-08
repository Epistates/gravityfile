//! Scan configuration types.

use std::path::PathBuf;

use derive_builder::Builder;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// Configuration for scanning operations.
#[derive(Debug, Clone, Builder, Serialize, Deserialize)]
#[builder(setter(into), build_fn(validate = "Self::validate"))]
pub struct ScanConfig {
    /// Root path to scan.
    #[serde(with = "crate::path_serde")]
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

    /// Maximum descendant entries consumed after name/depth filtering.
    /// `None` is unlimited; zero permits only an empty scope. The root does
    /// not count. Metadata failures and unsupported file types still count.
    /// Exceeding the limit fails the scan instead of returning a partial tree.
    /// This is not a bound on walker prefetching, Git enrichment, or total RAM.
    #[builder(default)]
    #[serde(default)]
    pub max_entries: Option<u64>,

    /// Patterns to ignore using globset syntax (not gitignore negation rules).
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

    /// Compiled glob patterns for ignore matching. Rebuilt from `ignore_patterns`.
    #[serde(skip)]
    #[builder(setter(skip))]
    #[builder(default)]
    compiled_ignore: Option<(Vec<String>, GlobSet)>,
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
        ScanConfig::build_ignore_globset(self.ignore_patterns.as_deref().unwrap_or_default())?;
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
            max_entries: None,
            ignore_patterns: Vec::new(),
            threads: 0,
            include_hidden: true,
            compute_hashes: false,
            min_hash_size: 4096,
            compiled_ignore: None,
        }
    }

    /// Validate a directly constructed or deserialized configuration before use.
    pub fn validate(&self) -> Result<(), String> {
        if self.root.as_os_str().is_empty() {
            return Err("Root path cannot be empty".into());
        }
        self.effective_ignore_globset()?;
        Ok(())
    }

    /// Compile every supplied pattern, rejecting invalid patterns as a unit.
    pub fn build_ignore_globset(patterns: &[String]) -> Result<Option<GlobSet>, String> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(
                Glob::new(pattern)
                    .map_err(|error| format!("Invalid ignore pattern {pattern:?}: {error}"))?,
            );
        }
        builder
            .build()
            .map(Some)
            .map_err(|error| format!("Invalid ignore pattern set: {error}"))
    }

    /// Return a compiled snapshot of the current patterns, never a stale cache.
    pub fn effective_ignore_globset(&self) -> Result<Option<GlobSet>, String> {
        match &self.compiled_ignore {
            Some((patterns, set)) if patterns == &self.ignore_patterns => Ok(Some(set.clone())),
            _ => Self::build_ignore_globset(&self.ignore_patterns),
        }
    }

    /// Cache the current valid patterns. Public-field mutations are detected
    /// automatically; callers need not manually invalidate this cache.
    pub fn compile_patterns(&mut self) -> Result<(), String> {
        self.compiled_ignore = Self::build_ignore_globset(&self.ignore_patterns)?
            .map(|set| (self.ignore_patterns.clone(), set));
        Ok(())
    }

    /// Match a name, returning an error if any configured pattern is invalid.
    pub fn should_ignore(&self, name: &str) -> Result<bool, String> {
        match &self.compiled_ignore {
            Some((patterns, set)) if patterns == &self.ignore_patterns => Ok(set.is_match(name)),
            _ => Ok(self
                .effective_ignore_globset()?
                .is_some_and(|set| set.is_match(name))),
        }
    }

    /// Check if hidden files should be skipped.
    #[inline]
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
    fn public_pattern_mutation_cannot_reuse_a_stale_cache() {
        let mut config = ScanConfig::new(".");
        config.ignore_patterns = vec!["*.log".into()];
        config.compile_patterns().unwrap();
        config.ignore_patterns = vec!["*.tmp".into()];
        assert!(!config.should_ignore("data.log").unwrap());
        assert!(config.should_ignore("data.tmp").unwrap());
        config.ignore_patterns = vec!["[".into()];
        assert!(config.should_ignore("data.log").is_err());
        assert!(config.validate().is_err());
        config.ignore_patterns.clear();
        assert!(!config.should_ignore("data.log").unwrap());
    }

    #[test]
    fn builder_rejects_invalid_ignore_scope() {
        assert!(
            ScanConfig::builder()
                .root(".")
                .ignore_patterns(vec!["[".into()])
                .build()
                .is_err()
        );
    }

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
    fn test_should_ignore_glob() {
        let mut config = ScanConfig::builder()
            .root("/test")
            .ignore_patterns(vec![
                "node_modules".to_string(),
                "*.log".to_string(),
                "**/*.tmp".to_string(),
            ])
            .build()
            .unwrap();
        config.compile_patterns().unwrap();

        assert!(config.should_ignore("node_modules").unwrap());
        assert!(config.should_ignore("test.log").unwrap());
        assert!(config.should_ignore("cache.tmp").unwrap());
        assert!(!config.should_ignore("src").unwrap());
        assert!(!config.should_ignore("test.txt").unwrap());
    }

    #[test]
    fn test_should_ignore_prefix_glob() {
        let mut config = ScanConfig::builder()
            .root("/test")
            .ignore_patterns(vec!["build*".to_string()])
            .build()
            .unwrap();
        config.compile_patterns().unwrap();

        assert!(config.should_ignore("build").unwrap());
        assert!(config.should_ignore("build-output").unwrap());
        assert!(!config.should_ignore("rebuild").unwrap());
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
