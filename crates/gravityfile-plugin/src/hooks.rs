//! Hook definitions for plugin event system.
//!
//! Hooks allow plugins to respond to events in the gravityfile application.
//! Each hook has a specific context and expected result type.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::Value;

/// Events that plugins can hook into.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Hook {
    // ==================== Navigation Events ====================
    /// Fired when navigating to a new directory.
    OnNavigate {
        /// Previous directory path.
        from: PathBuf,
        /// New directory path.
        to: PathBuf,
    },

    /// Fired when drilling down into a directory.
    OnDrillDown {
        /// Directory being entered.
        path: PathBuf,
    },

    /// Fired when navigating back.
    OnBack {
        /// Directory being left.
        from: PathBuf,
        /// Directory being returned to.
        to: PathBuf,
    },

    // ==================== Scan Events ====================
    /// Fired when a scan operation starts.
    OnScanStart {
        /// Root path being scanned.
        path: PathBuf,
    },

    /// Fired periodically during scanning with progress info.
    OnScanProgress {
        /// Files scanned so far.
        files_scanned: u64,
        /// Directories scanned so far.
        dirs_scanned: u64,
        /// Bytes scanned so far.
        bytes_scanned: u64,
    },

    /// Fired when scan completes successfully.
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

    /// Fired when scan fails.
    OnScanError {
        /// Root path that failed.
        path: PathBuf,
        /// Error message.
        error: String,
    },

    // ==================== File Operation Events ====================
    /// Fired before deletion starts.
    OnDeleteStart {
        /// Items to be deleted.
        items: Vec<PathBuf>,
        /// Whether using trash (recoverable).
        use_trash: bool,
    },

    /// Fired after deletion completes.
    OnDeleteComplete {
        /// Number of items successfully deleted.
        deleted: usize,
        /// Number of items that failed to delete.
        failed: usize,
        /// Total bytes freed.
        bytes_freed: u64,
    },

    /// Fired before copy operation starts.
    OnCopyStart {
        /// Source paths.
        sources: Vec<PathBuf>,
        /// Destination directory.
        destination: PathBuf,
    },

    /// Fired after copy completes.
    OnCopyComplete {
        /// Number of items successfully copied.
        succeeded: usize,
        /// Number of items that failed.
        failed: usize,
        /// Total bytes copied.
        bytes_copied: u64,
    },

    /// Fired before move operation starts.
    OnMoveStart {
        /// Source paths.
        sources: Vec<PathBuf>,
        /// Destination directory.
        destination: PathBuf,
    },

    /// Fired after move completes.
    OnMoveComplete {
        /// Number of items successfully moved.
        succeeded: usize,
        /// Number of items that failed.
        failed: usize,
    },

    /// Fired before rename operation.
    OnRenameStart {
        /// Original path.
        source: PathBuf,
        /// New name.
        new_name: String,
    },

    /// Fired after rename completes.
    OnRenameComplete {
        /// Original path.
        source: PathBuf,
        /// New path after rename.
        new_path: PathBuf,
    },

    // ==================== Analysis Events ====================
    /// Fired when duplicate analysis completes.
    OnDuplicatesFound {
        /// Number of duplicate groups found.
        group_count: usize,
        /// Total wasted space in bytes.
        wasted_bytes: u64,
    },

    /// Fired when age analysis completes.
    OnAgeAnalysisComplete {
        /// Number of stale directories found.
        stale_dirs: usize,
        /// Oldest file age in seconds.
        oldest_age_secs: u64,
    },

    // ==================== UI Events ====================
    /// Fired before rendering a view.
    OnRender {
        /// Current view name.
        view: String,
        /// Render area dimensions.
        width: u16,
        height: u16,
    },

    /// Fired when user performs an action.
    OnAction {
        /// Action name (e.g., "delete", "copy", "move").
        action: String,
    },

    /// Fired when application mode changes.
    OnModeChange {
        /// Previous mode.
        from: String,
        /// New mode.
        to: String,
    },

    /// Fired when selection changes.
    OnSelectionChange {
        /// Currently selected paths.
        selected: Vec<PathBuf>,
        /// Number of items selected.
        count: usize,
    },

    // ==================== Lifecycle Events ====================
    /// Fired when application starts.
    OnStartup,

    /// Fired when application is about to quit.
    OnShutdown,

    /// Fired when a plugin is loaded.
    OnPluginLoad {
        /// Name of the plugin being loaded.
        name: String,
    },

    /// Fired when a plugin is unloaded.
    OnPluginUnload {
        /// Name of the plugin being unloaded.
        name: String,
    },
}

impl Hook {
    /// Get the hook name as a string (for matching in plugins).
    pub fn name(&self) -> &'static str {
        match self {
            Self::OnNavigate { .. } => "on_navigate",
            Self::OnDrillDown { .. } => "on_drill_down",
            Self::OnBack { .. } => "on_back",
            Self::OnScanStart { .. } => "on_scan_start",
            Self::OnScanProgress { .. } => "on_scan_progress",
            Self::OnScanComplete { .. } => "on_scan_complete",
            Self::OnScanError { .. } => "on_scan_error",
            Self::OnDeleteStart { .. } => "on_delete_start",
            Self::OnDeleteComplete { .. } => "on_delete_complete",
            Self::OnCopyStart { .. } => "on_copy_start",
            Self::OnCopyComplete { .. } => "on_copy_complete",
            Self::OnMoveStart { .. } => "on_move_start",
            Self::OnMoveComplete { .. } => "on_move_complete",
            Self::OnRenameStart { .. } => "on_rename_start",
            Self::OnRenameComplete { .. } => "on_rename_complete",
            Self::OnDuplicatesFound { .. } => "on_duplicates_found",
            Self::OnAgeAnalysisComplete { .. } => "on_age_analysis_complete",
            Self::OnRender { .. } => "on_render",
            Self::OnAction { .. } => "on_action",
            Self::OnModeChange { .. } => "on_mode_change",
            Self::OnSelectionChange { .. } => "on_selection_change",
            Self::OnStartup => "on_startup",
            Self::OnShutdown => "on_shutdown",
            Self::OnPluginLoad { .. } => "on_plugin_load",
            Self::OnPluginUnload { .. } => "on_plugin_unload",
        }
    }

    /// Check if this is a lifecycle event (startup/shutdown).
    pub fn is_lifecycle(&self) -> bool {
        matches!(
            self,
            Self::OnStartup | Self::OnShutdown | Self::OnPluginLoad { .. } | Self::OnPluginUnload { .. }
        )
    }

    /// Check if this hook should run synchronously (blocking).
    pub fn is_sync(&self) -> bool {
        // Render and action hooks should be sync to avoid UI lag
        matches!(
            self,
            Self::OnRender { .. }
                | Self::OnAction { .. }
                | Self::OnModeChange { .. }
                | Self::OnSelectionChange { .. }
        )
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

    /// Theme variant (dark/light).
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
#[derive(Debug, Clone, Default)]
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
