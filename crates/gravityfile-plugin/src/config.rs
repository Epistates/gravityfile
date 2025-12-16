//! Plugin configuration and metadata.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::PluginKind;

/// Global plugin system configuration.
#[derive(Debug, Clone)]
pub struct PluginConfig {
    /// Directory where plugins are stored.
    pub plugin_dir: PathBuf,

    /// Directory for plugin configuration files.
    pub config_dir: PathBuf,

    /// Whether to enable the plugin system.
    pub enabled: bool,

    /// Default timeout for plugin operations in milliseconds.
    pub default_timeout_ms: u64,

    /// Maximum memory per plugin in MB (0 = unlimited).
    pub max_memory_mb: usize,

    /// Whether to allow plugins to access the network.
    pub allow_network: bool,

    /// List of disabled plugin names.
    pub disabled_plugins: HashSet<String>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gravityfile");

        Self {
            plugin_dir: config_dir.join("plugins"),
            config_dir,
            enabled: true,
            default_timeout_ms: 5000,
            max_memory_mb: 256,
            allow_network: false,
            disabled_plugins: HashSet::new(),
        }
    }
}

impl PluginConfig {
    /// Create a new config with a custom plugin directory.
    pub fn with_plugin_dir(mut self, dir: PathBuf) -> Self {
        self.plugin_dir = dir;
        self
    }

    /// Set the default timeout.
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.default_timeout_ms = timeout_ms;
        self
    }

    /// Disable a specific plugin.
    pub fn disable_plugin(mut self, name: impl Into<String>) -> Self {
        self.disabled_plugins.insert(name.into());
        self
    }

    /// Check if a plugin is disabled.
    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled_plugins.contains(name)
    }
}

/// Metadata about a plugin from its plugin.toml file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    /// Plugin name (unique identifier).
    pub name: String,

    /// Plugin version.
    #[serde(default = "default_version")]
    pub version: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: String,

    /// Plugin author.
    #[serde(default)]
    pub author: String,

    /// Plugin homepage/repository URL.
    #[serde(default)]
    pub url: String,

    /// Runtime to use (lua, rhai, wasm).
    #[serde(default = "default_runtime")]
    pub runtime: String,

    /// Plugin kind/category.
    #[serde(default)]
    pub kind: PluginKind,

    /// Entry point file (relative to plugin directory).
    #[serde(default = "default_entry")]
    pub entry: String,

    /// Minimum gravityfile version required.
    #[serde(default)]
    pub min_version: Option<String>,

    /// Hooks this plugin wants to receive.
    #[serde(default)]
    pub hooks: PluginHooks,

    /// Permissions requested by this plugin.
    #[serde(default)]
    pub permissions: PluginPermissions,

    /// Plugin dependencies (other plugin names).
    #[serde(default)]
    pub dependencies: Vec<String>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_runtime() -> String {
    "lua".to_string()
}

fn default_entry() -> String {
    "main.lua".to_string()
}

impl Default for PluginMetadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: default_version(),
            description: String::new(),
            author: String::new(),
            url: String::new(),
            runtime: default_runtime(),
            kind: PluginKind::Hook,
            entry: default_entry(),
            min_version: None,
            hooks: PluginHooks::default(),
            permissions: PluginPermissions::default(),
            dependencies: vec![],
        }
    }
}

/// Hooks that a plugin wants to receive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginHooks {
    /// Navigation events.
    #[serde(default)]
    pub on_navigate: bool,
    #[serde(default)]
    pub on_drill_down: bool,
    #[serde(default)]
    pub on_back: bool,

    /// Scan events.
    #[serde(default)]
    pub on_scan_start: bool,
    #[serde(default)]
    pub on_scan_progress: bool,
    #[serde(default)]
    pub on_scan_complete: bool,

    /// File operation events.
    #[serde(default)]
    pub on_delete_start: bool,
    #[serde(default)]
    pub on_delete_complete: bool,
    #[serde(default)]
    pub on_copy_start: bool,
    #[serde(default)]
    pub on_copy_complete: bool,
    #[serde(default)]
    pub on_move_start: bool,
    #[serde(default)]
    pub on_move_complete: bool,

    /// UI events.
    #[serde(default)]
    pub on_render: bool,
    #[serde(default)]
    pub on_action: bool,
    #[serde(default)]
    pub on_mode_change: bool,

    /// Lifecycle events.
    #[serde(default)]
    pub on_startup: bool,
    #[serde(default)]
    pub on_shutdown: bool,
}

/// Permissions requested by a plugin.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginPermissions {
    /// Filesystem access level.
    #[serde(default)]
    pub filesystem: FilesystemPermission,

    /// Network access.
    #[serde(default)]
    pub network: bool,

    /// Allowed external commands.
    #[serde(default)]
    pub commands: Vec<String>,

    /// Environment variable access.
    #[serde(default)]
    pub env: bool,
}

/// Filesystem permission levels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemPermission {
    /// No filesystem access.
    #[default]
    None,

    /// Read-only access to scan root.
    Read,

    /// Read/write access to scan root.
    Write,

    /// Full filesystem access (dangerous).
    Full,
}

impl PluginPermissions {
    /// Check if plugin can read files.
    pub fn can_read(&self) -> bool {
        !matches!(self.filesystem, FilesystemPermission::None)
    }

    /// Check if plugin can write files.
    pub fn can_write(&self) -> bool {
        matches!(
            self.filesystem,
            FilesystemPermission::Write | FilesystemPermission::Full
        )
    }

    /// Check if plugin can run a command.
    pub fn can_run_command(&self, cmd: &str) -> bool {
        self.commands.iter().any(|c| c == cmd || c == "*")
    }
}
