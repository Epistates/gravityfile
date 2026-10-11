//! Hook definitions for plugin event system.
//!
//! The explorer fires these events. Every hook but [`Hook::OnDeleteStart`]
//! is an observer: it runs in the background and cannot hold up the
//! explorer. `on_delete_start` is awaited, and a plugin can veto the delete
//! by returning `{ prevent_default = true }`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::Value;

/// Events that plugins can hook into.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Hook {
    // ==================== Navigation Events ====================
    /// The explorer moved to another directory.
    OnNavigate {
        /// Previous directory path.
        from: PathBuf,
        /// New directory path.
        to: PathBuf,
    },

    // ==================== Scan Events ====================
    /// A full scan started.
    OnScanStart {
        /// Root path being scanned.
        path: PathBuf,
    },

    /// A full scan finished.
    OnScanComplete {
        /// Root path that was scanned.
        path: PathBuf,
        /// Total files found.
        total_files: u64,
        /// Total directories found.
        total_dirs: u64,
        /// Total size in bytes.
        total_size: u64,
    },

    /// A full scan failed.
    OnScanError {
        /// Root path that failed.
        path: PathBuf,
        /// Error message.
        error: String,
    },

    // ==================== File Operation Events ====================
    /// A delete is about to start. Awaited; a plugin can veto it.
    OnDeleteStart {
        /// Items to be deleted.
        items: Vec<PathBuf>,
        /// Whether using trash (recoverable).
        use_trash: bool,
    },

    /// A delete finished.
    OnDeleteComplete {
        /// Number of items successfully deleted.
        deleted: usize,
        /// Number of items that failed to delete.
        failed: usize,
        /// Total bytes freed.
        bytes_freed: u64,
    },

    /// A copy finished.
    OnCopyComplete {
        /// Number of items successfully copied.
        succeeded: usize,
        /// Number of items that failed.
        failed: usize,
        /// Total bytes copied.
        bytes_copied: u64,
    },

    /// A move finished.
    OnMoveComplete {
        /// Number of items successfully moved.
        succeeded: usize,
        /// Number of items that failed.
        failed: usize,
    },

    /// A rename finished.
    OnRenameComplete {
        /// Original path.
        source: PathBuf,
        /// New path after rename.
        new_path: PathBuf,
    },

    // ==================== Analysis Events ====================
    /// Duplicate analysis finished.
    OnDuplicatesFound {
        /// Number of duplicate groups found.
        group_count: usize,
        /// Total wasted space in bytes.
        wasted_bytes: u64,
    },

    /// Age analysis finished.
    OnAgeAnalysisComplete {
        /// Number of stale directories found.
        stale_dirs: usize,
        /// Files whose ages were counted.
        files: u64,
        /// Their average age, in seconds.
        average_age_secs: u64,
    },

    // ==================== Lifecycle Events ====================
    /// The explorer started.
    OnStartup,

    /// The explorer is about to quit; awaited briefly.
    OnShutdown,
}

impl Hook {
    /// The function a plugin defines to receive this hook.
    pub fn name(&self) -> &'static str {
        match self {
            Self::OnNavigate { .. } => "on_navigate",
            Self::OnScanStart { .. } => "on_scan_start",
            Self::OnScanComplete { .. } => "on_scan_complete",
            Self::OnScanError { .. } => "on_scan_error",
            Self::OnDeleteStart { .. } => "on_delete_start",
            Self::OnDeleteComplete { .. } => "on_delete_complete",
            Self::OnCopyComplete { .. } => "on_copy_complete",
            Self::OnMoveComplete { .. } => "on_move_complete",
            Self::OnRenameComplete { .. } => "on_rename_complete",
            Self::OnDuplicatesFound { .. } => "on_duplicates_found",
            Self::OnAgeAnalysisComplete { .. } => "on_age_analysis_complete",
            Self::OnStartup => "on_startup",
            Self::OnShutdown => "on_shutdown",
        }
    }
}

/// How prominent a plugin's notice is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    #[default]
    Info,
    Warn,
    Error,
}

impl NoticeLevel {
    /// The level named `name`; anything unknown is `Info`.
    pub fn parse(name: &str) -> Self {
        match name {
            "warn" | "warning" => Self::Warn,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }
}

/// A message a plugin asked the explorer to show, with `gf.notify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub level: NoticeLevel,
    pub message: String,
}

impl Notice {
    /// The most notices one hook call can raise.
    pub const MAX_PER_CALL: usize = 8;
    /// The longest notice, in bytes; longer ones are cut at a character.
    pub const MAX_BYTES: usize = 500;

    /// A notice, its message cut to [`Self::MAX_BYTES`].
    pub fn new(level: NoticeLevel, message: &str) -> Self {
        let mut end = message.len().min(Self::MAX_BYTES);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            level,
            message: message[..end].to_string(),
        }
    }
}

/// Context provided to plugins when a hook is invoked.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    /// Additional data passed to the hook.
    pub data: HashMap<String, Value>,

    /// Current working directory.
    pub cwd: Option<PathBuf>,

    /// Current view root (drill-down location).
    pub view_root: Option<PathBuf>,

    /// The colour theme's name.
    pub theme: Option<String>,
}

impl HookContext {
    /// Create a new empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a value in the context.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.data.insert(key.into(), value.into());
        self
    }

    /// Get a value from the context.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    /// Set the current working directory.
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }

    /// Set the view root.
    pub fn with_view_root(mut self, view_root: PathBuf) -> Self {
        self.view_root = Some(view_root);
        self
    }

    /// Set the theme.
    pub fn with_theme(mut self, theme: impl Into<String>) -> Self {
        self.theme = Some(theme.into());
        self
    }
}

/// Result returned by a plugin hook handler.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HookResult {
    /// Whether the hook was handled.
    pub handled: bool,

    /// Whether to prevent default behavior.
    pub prevent_default: bool,

    /// Whether to stop propagation to other plugins.
    pub stop_propagation: bool,

    /// Return value from the hook (if any).
    pub value: Option<Value>,

    /// Error message (if hook failed).
    pub error: Option<String>,

    /// Messages the plugin asked the explorer to show.
    #[serde(default)]
    pub notices: Vec<Notice>,

    /// The plugin that produced this result, filled in by the manager.
    #[serde(default)]
    pub plugin: Option<String>,
}

impl HookResult {
    /// Create a successful result.
    pub fn ok() -> Self {
        Self {
            handled: true,
            ..Default::default()
        }
    }

    /// Create a result with a return value.
    pub fn with_value(value: impl Into<Value>) -> Self {
        Self {
            handled: true,
            value: Some(value.into()),
            ..Default::default()
        }
    }

    /// Create an error result.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            handled: true,
            error: Some(message.into()),
            ..Default::default()
        }
    }

    /// Mark this result as preventing default behavior.
    pub fn prevent_default(mut self) -> Self {
        self.prevent_default = true;
        self
    }

    /// Mark this result as stopping propagation.
    pub fn stop_propagation(mut self) -> Self {
        self.stop_propagation = true;
        self
    }

    /// Check if the hook execution was successful.
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// Check if the hook had an error.
    pub fn is_err(&self) -> bool {
        self.error.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of every hook. The match makes a new variant fail to compile here
    /// until it is listed, and the test then checks plugin discovery knows it.
    fn every_hook() -> Vec<Hook> {
        let all = vec![
            Hook::OnNavigate {
                from: PathBuf::new(),
                to: PathBuf::new(),
            },
            Hook::OnScanStart {
                path: PathBuf::new(),
            },
            Hook::OnScanComplete {
                path: PathBuf::new(),
                total_files: 0,
                total_dirs: 0,
                total_size: 0,
            },
            Hook::OnScanError {
                path: PathBuf::new(),
                error: String::new(),
            },
            Hook::OnDeleteStart {
                items: Vec::new(),
                use_trash: true,
            },
            Hook::OnDeleteComplete {
                deleted: 0,
                failed: 0,
                bytes_freed: 0,
            },
            Hook::OnCopyComplete {
                succeeded: 0,
                failed: 0,
                bytes_copied: 0,
            },
            Hook::OnMoveComplete {
                succeeded: 0,
                failed: 0,
            },
            Hook::OnRenameComplete {
                source: PathBuf::new(),
                new_path: PathBuf::new(),
            },
            Hook::OnDuplicatesFound {
                group_count: 0,
                wasted_bytes: 0,
            },
            Hook::OnAgeAnalysisComplete {
                stale_dirs: 0,
                files: 0,
                average_age_secs: 0,
            },
            Hook::OnStartup,
            Hook::OnShutdown,
        ];
        for hook in &all {
            match hook {
                Hook::OnNavigate { .. }
                | Hook::OnScanStart { .. }
                | Hook::OnScanComplete { .. }
                | Hook::OnScanError { .. }
                | Hook::OnDeleteStart { .. }
                | Hook::OnDeleteComplete { .. }
                | Hook::OnCopyComplete { .. }
                | Hook::OnMoveComplete { .. }
                | Hook::OnRenameComplete { .. }
                | Hook::OnDuplicatesFound { .. }
                | Hook::OnAgeAnalysisComplete { .. }
                | Hook::OnStartup
                | Hook::OnShutdown => {}
            }
        }
        all
    }

    #[test]
    fn plugin_discovery_knows_every_hook_and_nothing_else() {
        let names: Vec<&str> = every_hook().iter().map(Hook::name).collect();
        assert_eq!(names, crate::runtime::HOOK_NAMES);
    }

    #[test]
    fn long_notices_are_cut_at_a_character_boundary() {
        let notice = Notice::new(NoticeLevel::Info, &"é".repeat(Notice::MAX_BYTES));
        assert!(notice.message.len() <= Notice::MAX_BYTES);
        assert!(notice.message.chars().all(|c| c == 'é'));
        assert_eq!(NoticeLevel::parse("warning"), NoticeLevel::Warn);
        assert_eq!(NoticeLevel::parse("loud"), NoticeLevel::Info);
    }
}
