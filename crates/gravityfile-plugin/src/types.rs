//! Core types for the plugin system.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Result type for plugin operations.
pub type PluginResult<T> = Result<T, PluginError>;

/// Errors that can occur in the plugin system.
#[derive(Debug, Error)]
pub enum PluginError {
    /// Plugin file not found.
    #[error("Plugin not found: {path}")]
    NotFound { path: PathBuf },

    /// Failed to load plugin.
    #[error("Failed to load plugin '{name}': {message}")]
    LoadError { name: String, message: String },

    /// Plugin execution error.
    #[error("Plugin '{name}' execution error: {message}")]
    ExecutionError { name: String, message: String },

    /// Plugin was cancelled.
    #[error("Plugin '{name}' was cancelled")]
    Cancelled { name: String },

    /// Plugin timed out.
    #[error("Plugin '{name}' timed out after {timeout_ms}ms")]
    Timeout { name: String, timeout_ms: u64 },

    /// Invalid plugin configuration.
    #[error("Invalid plugin configuration: {message}")]
    ConfigError { message: String },

    /// Permission denied.
    #[error("Permission denied for plugin '{name}': {action}")]
    PermissionDenied { name: String, action: String },

    /// Runtime not available.
    #[error("Runtime '{runtime}' is not available")]
    RuntimeNotAvailable { runtime: String },

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Hook not implemented.
    #[error("Hook '{hook}' not implemented by plugin '{name}'")]
    HookNotImplemented { name: String, hook: String },
}

/// Categories of plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    #[default]
    /// Custom analysis plugins (find patterns, compute metrics).
    /// Runs asynchronously in the background.
    Analyzer,

    /// File preview generation plugins (syntax highlighting, thumbnails).
    /// Runs in an isolated context for safety.
    Previewer,

    /// Custom file operation plugins (compress, archive, transform).
    /// Runs asynchronously with progress reporting.
    Action,

    /// Custom column/cell rendering plugins (git status, permissions).
    /// Runs synchronously on the main thread.
    Renderer,

    /// Search and filter plugins (custom search algorithms).
    /// Can be sync or async depending on implementation.
    Filter,

    /// Event listener plugins (logging, notifications).
    /// Runs synchronously as callbacks.
    Hook,
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Analyzer => write!(f, "analyzer"),
            Self::Previewer => write!(f, "previewer"),
            Self::Action => write!(f, "action"),
            Self::Renderer => write!(f, "renderer"),
            Self::Filter => write!(f, "filter"),
            Self::Hook => write!(f, "hook"),
        }
    }
}

/// A dynamic value that can be passed between Rust and plugin runtimes.
///
/// This is a simplified representation that can be converted to/from
/// runtime-specific value types (mlua::Value, rhai::Dynamic, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    /// Null/nil value.
    Null,

    /// Boolean value.
    Bool(bool),

    /// Integer value.
    Integer(i64),

    /// Floating point value.
    Float(f64),

    /// String value.
    String(String),

    /// Array/list value.
    Array(Vec<Value>),

    /// Object/table/map value.
    Object(std::collections::HashMap<String, Value>),

    /// Binary data.
    Bytes(Vec<u8>),
}

impl Value {
    /// Create a null value.
    pub fn null() -> Self {
        Self::Null
    }

    /// Check if this value is null.
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Try to get this value as a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Try to get this value as an integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Try to get this value as a float.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            Self::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// Try to get this value as a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get this value as an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(arr) => Some(arr),
            _ => None,
        }
    }

    /// Try to get this value as an object.
    pub fn as_object(&self) -> Option<&std::collections::HashMap<String, Value>> {
        match self {
            Self::Object(obj) => Some(obj),
            _ => None,
        }
    }
}

impl Default for Value {
    fn default() -> Self {
        Self::Null
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Self::Integer(i)
    }
}

impl From<i32> for Value {
    fn from(i: i32) -> Self {
        Self::Integer(i as i64)
    }
}

impl From<u64> for Value {
    fn from(u: u64) -> Self {
        Self::Integer(u as i64)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Self::Float(f)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(arr: Vec<T>) -> Self {
        Self::Array(arr.into_iter().map(Into::into).collect())
    }
}

impl From<std::collections::HashMap<String, Value>> for Value {
    fn from(obj: std::collections::HashMap<String, Value>) -> Self {
        Self::Object(obj)
    }
}

impl From<PathBuf> for Value {
    fn from(path: PathBuf) -> Self {
        Self::String(path.to_string_lossy().to_string())
    }
}
