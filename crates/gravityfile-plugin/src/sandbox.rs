//! Security sandboxing for plugins.
//!
//! This module provides sandboxing capabilities to restrict what plugins
//! can access and do.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Permission types that can be granted to plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Read files from the filesystem.
    Read,

    /// Write files to the filesystem.
    Write,

    /// Execute external commands.
    Execute,

    /// Access network resources.
    Network,

    /// Access environment variables.
    Environment,

    /// Access clipboard.
    Clipboard,

    /// Modify UI elements.
    Ui,

    /// Send notifications.
    Notify,
}

/// Configuration for plugin sandboxing.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Paths the plugin is allowed to read from.
    pub allowed_read_paths: Vec<PathBuf>,

    /// Paths the plugin is allowed to write to.
    pub allowed_write_paths: Vec<PathBuf>,

    /// Commands the plugin is allowed to execute.
    pub allowed_commands: HashSet<String>,

    /// Whether network access is allowed.
    pub allow_network: bool,

    /// Whether environment access is allowed.
    pub allow_env: bool,

    /// Maximum execution time in milliseconds.
    pub timeout_ms: u64,

    /// Maximum memory in bytes (0 = unlimited).
    pub max_memory: usize,

    /// Maximum file size that can be read.
    pub max_read_size: usize,

    /// Granted permissions.
    pub permissions: HashSet<Permission>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        let mut permissions = HashSet::new();
        permissions.insert(Permission::Read);
        permissions.insert(Permission::Notify);

        Self {
            allowed_read_paths: vec![],
            allowed_write_paths: vec![],
            allowed_commands: HashSet::new(),
            allow_network: false,
            allow_env: false,
            timeout_ms: 5000,
            max_memory: 256 * 1024 * 1024, // 256 MB
            max_read_size: 10 * 1024 * 1024, // 10 MB
            permissions,
        }
    }
}

impl SandboxConfig {
    /// Create a minimal sandbox with no permissions.
    pub fn minimal() -> Self {
        Self {
            allowed_read_paths: vec![],
            allowed_write_paths: vec![],
            allowed_commands: HashSet::new(),
            allow_network: false,
            allow_env: false,
            timeout_ms: 1000,
            max_memory: 64 * 1024 * 1024,
            max_read_size: 1024 * 1024,
            permissions: HashSet::new(),
        }
    }

    /// Create a permissive sandbox (for trusted plugins).
    pub fn permissive() -> Self {
        let mut permissions = HashSet::new();
        permissions.insert(Permission::Read);
        permissions.insert(Permission::Write);
        permissions.insert(Permission::Execute);
        permissions.insert(Permission::Environment);
        permissions.insert(Permission::Ui);
        permissions.insert(Permission::Notify);

        Self {
            allowed_read_paths: vec![PathBuf::from("/")],
            allowed_write_paths: vec![],
            allowed_commands: HashSet::from(["*".to_string()]),
            allow_network: false,
            allow_env: true,
            timeout_ms: 30000,
            max_memory: 1024 * 1024 * 1024,
            max_read_size: 100 * 1024 * 1024,
            permissions,
        }
    }

    /// Add an allowed read path.
    pub fn allow_read(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_read_paths.push(path.into());
        self.permissions.insert(Permission::Read);
        self
    }

    /// Add an allowed write path.
    pub fn allow_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_write_paths.push(path.into());
        self.permissions.insert(Permission::Write);
        self
    }

    /// Allow a specific command.
    pub fn allow_command(mut self, cmd: impl Into<String>) -> Self {
        self.allowed_commands.insert(cmd.into());
        self.permissions.insert(Permission::Execute);
        self
    }

    /// Enable network access.
    pub fn allow_network(mut self) -> Self {
        self.allow_network = true;
        self.permissions.insert(Permission::Network);
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// Set memory limit.
    pub fn with_memory_limit(mut self, bytes: usize) -> Self {
        self.max_memory = bytes;
        self
    }

    /// Grant a permission.
    pub fn grant(mut self, permission: Permission) -> Self {
        self.permissions.insert(permission);
        self
    }

    /// Check if a permission is granted.
    pub fn has_permission(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }

    /// Check if reading a path is allowed.
    pub fn can_read(&self, path: &PathBuf) -> bool {
        if !self.has_permission(Permission::Read) {
            return false;
        }
        if self.allowed_read_paths.is_empty() {
            return true; // No restrictions
        }
        self.allowed_read_paths
            .iter()
            .any(|allowed| path.starts_with(allowed))
    }

    /// Check if writing to a path is allowed.
    pub fn can_write(&self, path: &PathBuf) -> bool {
        if !self.has_permission(Permission::Write) {
            return false;
        }
        self.allowed_write_paths
            .iter()
            .any(|allowed| path.starts_with(allowed))
    }

    /// Check if executing a command is allowed.
    pub fn can_execute(&self, command: &str) -> bool {
        if !self.has_permission(Permission::Execute) {
            return false;
        }
        self.allowed_commands.contains("*") || self.allowed_commands.contains(command)
    }
}

/// Sandbox violation that occurred during plugin execution.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SandboxViolation {
    /// Type of violation.
    pub kind: ViolationKind,

    /// Description of what was attempted.
    pub description: String,

    /// Path involved (if applicable).
    pub path: Option<PathBuf>,

    /// Command involved (if applicable).
    pub command: Option<String>,
}

/// Types of sandbox violations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ViolationKind {
    /// Attempted to read from disallowed path.
    ReadDenied,

    /// Attempted to write to disallowed path.
    WriteDenied,

    /// Attempted to execute disallowed command.
    ExecuteDenied,

    /// Attempted network access when not allowed.
    NetworkDenied,

    /// Attempted to access environment when not allowed.
    EnvDenied,

    /// Exceeded memory limit.
    MemoryExceeded,

    /// Exceeded execution timeout.
    TimeoutExceeded,

    /// Attempted to read file larger than limit.
    FileTooLarge,
}

impl std::fmt::Display for SandboxViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ViolationKind::ReadDenied => {
                write!(f, "Read denied: {}", self.description)
            }
            ViolationKind::WriteDenied => {
                write!(f, "Write denied: {}", self.description)
            }
            ViolationKind::ExecuteDenied => {
                write!(f, "Execute denied: {}", self.description)
            }
            ViolationKind::NetworkDenied => {
                write!(f, "Network access denied")
            }
            ViolationKind::EnvDenied => {
                write!(f, "Environment access denied")
            }
            ViolationKind::MemoryExceeded => {
                write!(f, "Memory limit exceeded")
            }
            ViolationKind::TimeoutExceeded => {
                write!(f, "Execution timeout exceeded")
            }
            ViolationKind::FileTooLarge => {
                write!(f, "File too large: {}", self.description)
            }
        }
    }
}

impl std::error::Error for SandboxViolation {}
