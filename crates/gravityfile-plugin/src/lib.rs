//! Language-agnostic plugin system for gravityfile.
//!
//! This crate provides a trait-based plugin architecture that supports multiple
//! scripting language runtimes (Lua, Rhai, and potentially WASM in the future).
//!
//! # Architecture
//!
//! The plugin system is built around the [`PluginRuntime`] trait, which defines
//! the interface that any scripting language runtime must implement. This allows
//! users to write plugins in their preferred language.
//!
//! # Plugin Types
//!
//! - **Analyzers**: Custom file/directory analysis (async, returns data)
//! - **Previewers**: File content preview generation (async, isolated)
//! - **Actions**: Custom file operations (async with progress)
//! - **Renderers**: Custom column/cell rendering (sync)
//! - **Filters**: Search/filter plugins (sync/async)
//! - **Hooks**: Event listeners (sync callbacks)
//!
//! # Example
//!
//! ```ignore
//! use gravityfile_plugin::{PluginManager, PluginConfig};
//!
//! let config = PluginConfig::default();
//! let mut manager = PluginManager::new(config)?;
//!
//! // Load plugins from user config directory
//! manager.discover_plugins().await?;
//!
//! // Dispatch a hook to all registered plugins
//! manager.dispatch_hook(&Hook::OnScanComplete { tree }).await?;
//! ```

mod config;
mod hooks;
pub mod lua;
pub mod rhai;
mod runtime;
mod sandbox;
mod types;

pub use config::{PluginConfig, PluginMetadata};
pub use hooks::{Hook, HookContext, HookResult};
pub use runtime::{IsolatedContext, PluginHandle, PluginManager, PluginRuntime};
pub use sandbox::{Permission, SandboxConfig};
pub use types::{PluginError, PluginKind, PluginResult, Value};
