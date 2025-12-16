//! Plugin runtime trait and manager.
//!
//! This module defines the language-agnostic [`PluginRuntime`] trait that
//! all scripting language implementations must satisfy.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use tokio_util::sync::CancellationToken;

use crate::config::{PluginConfig, PluginMetadata};
use crate::hooks::{Hook, HookContext, HookResult};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginKind, PluginResult, Value};

/// A handle to a loaded plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginHandle(pub(crate) usize);

impl PluginHandle {
    /// Create a new plugin handle.
    pub fn new(id: usize) -> Self {
        Self(id)
    }

    /// Get the raw ID.
    pub fn id(&self) -> usize {
        self.0
    }
}

/// Type alias for boxed futures returned by async plugin methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait that all plugin runtime implementations must satisfy.
///
/// This trait abstracts over the specific scripting language (Lua, Rhai, WASM)
/// allowing plugins to be written in any supported language.
pub trait PluginRuntime: Send + Sync {
    /// Get the name of this runtime (e.g., "lua", "rhai", "wasm").
    fn name(&self) -> &'static str;

    /// Get the file extensions this runtime handles (e.g., [".lua"]).
    fn file_extensions(&self) -> &'static [&'static str];

    /// Initialize the runtime with configuration.
    fn init(&mut self, config: &PluginConfig) -> PluginResult<()>;

    /// Load a plugin from a file path.
    ///
    /// Returns a handle that can be used to interact with the plugin.
    fn load_plugin(&mut self, id: &str, source: &Path) -> PluginResult<PluginHandle>;

    /// Unload a previously loaded plugin.
    fn unload_plugin(&mut self, handle: PluginHandle) -> PluginResult<()>;

    /// Get metadata about a loaded plugin.
    fn get_metadata(&self, handle: PluginHandle) -> Option<&PluginMetadata>;

    /// Check if a plugin implements a specific hook.
    fn has_hook(&self, handle: PluginHandle, hook_name: &str) -> bool;

    /// Call a hook on a plugin synchronously.
    ///
    /// Used for hooks that must complete immediately (e.g., render hooks).
    fn call_hook_sync(
        &self,
        handle: PluginHandle,
        hook: &Hook,
        ctx: &HookContext,
    ) -> PluginResult<HookResult>;

    /// Call a hook on a plugin asynchronously.
    ///
    /// Used for hooks that may take time (e.g., scan complete hooks).
    fn call_hook_async<'a>(
        &'a self,
        handle: PluginHandle,
        hook: &'a Hook,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, PluginResult<HookResult>>;

    /// Call an arbitrary method on a plugin.
    fn call_method<'a>(
        &'a self,
        handle: PluginHandle,
        method: &'a str,
        args: Vec<Value>,
    ) -> BoxFuture<'a, PluginResult<Value>>;

    /// Create an isolated context for running async plugin code.
    ///
    /// Isolated contexts have limited API access and their own Lua/Rhai state,
    /// making them safe to run in background tasks.
    fn create_isolated_context(&self, sandbox: &SandboxConfig) -> PluginResult<Box<dyn IsolatedContext>>;

    /// Get the list of loaded plugin handles.
    fn loaded_plugins(&self) -> Vec<PluginHandle>;

    /// Shutdown the runtime and cleanup resources.
    fn shutdown(&mut self) -> PluginResult<()>;
}

/// An isolated execution context for running plugin code safely.
///
/// Isolated contexts are used for async plugins (previewers, analyzers) that
/// run in background tasks. They have:
/// - Their own script state (not shared with main runtime)
/// - Limited API access (no UI modification)
/// - Cancellation support
/// - Resource limits
pub trait IsolatedContext: Send {
    /// Execute a chunk of code in this isolated context.
    fn execute<'a>(
        &'a self,
        code: &'a [u8],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>>;

    /// Execute a named function with arguments.
    fn call_function<'a>(
        &'a self,
        name: &'a str,
        args: Vec<Value>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>>;

    /// Set a global variable in this context.
    fn set_global(&mut self, name: &str, value: Value) -> PluginResult<()>;

    /// Get a global variable from this context.
    fn get_global(&self, name: &str) -> PluginResult<Value>;
}

/// Information about a loaded plugin.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// Unique handle for this plugin.
    pub handle: PluginHandle,

    /// Plugin ID (usually directory name).
    pub id: String,

    /// Plugin metadata from plugin.toml.
    pub metadata: PluginMetadata,

    /// Path to the plugin directory.
    pub path: std::path::PathBuf,

    /// Hooks this plugin implements.
    pub hooks: Vec<String>,
}

/// Manager for all plugin runtimes and loaded plugins.
pub struct PluginManager {
    /// Available runtimes keyed by name.
    runtimes: HashMap<String, Box<dyn PluginRuntime>>,

    /// Loaded plugins keyed by handle.
    plugins: HashMap<PluginHandle, LoadedPlugin>,

    /// Plugin config.
    config: PluginConfig,

    /// Next plugin handle ID (reserved for future use).
    #[allow(dead_code)]
    next_handle: usize,
}

impl PluginManager {
    /// Create a new plugin manager with the given configuration.
    pub fn new(config: PluginConfig) -> Self {
        Self {
            runtimes: HashMap::new(),
            plugins: HashMap::new(),
            config,
            next_handle: 0,
        }
    }

    /// Register a plugin runtime.
    pub fn register_runtime(&mut self, runtime: Box<dyn PluginRuntime>) -> PluginResult<()> {
        let name = runtime.name().to_string();
        self.runtimes.insert(name, runtime);
        Ok(())
    }

    /// Get a runtime by name.
    pub fn get_runtime(&self, name: &str) -> Option<&dyn PluginRuntime> {
        self.runtimes.get(name).map(|r| r.as_ref())
    }

    /// Get a mutable runtime by name.
    pub fn get_runtime_mut(&mut self, name: &str) -> Option<&mut Box<dyn PluginRuntime>> {
        self.runtimes.get_mut(name)
    }

    /// Initialize all registered runtimes.
    pub fn init_runtimes(&mut self) -> PluginResult<()> {
        for runtime in self.runtimes.values_mut() {
            runtime.init(&self.config)?;
        }
        Ok(())
    }

    /// Discover and load plugins from the plugin directory.
    pub async fn discover_plugins(&mut self) -> PluginResult<Vec<LoadedPlugin>> {
        let plugin_dir = self.config.plugin_dir.clone();
        if !plugin_dir.exists() {
            return Ok(vec![]);
        }

        let mut loaded = vec![];

        // Read plugin directories
        let entries = std::fs::read_dir(&plugin_dir).map_err(|e| PluginError::Io(e))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Look for plugin.toml
            let toml_path = path.join("plugin.toml");
            if !toml_path.exists() {
                continue;
            }

            // Parse metadata
            let toml_content = std::fs::read_to_string(&toml_path)?;
            let metadata: PluginMetadata = toml::from_str(&toml_content)
                .map_err(|e| PluginError::ConfigError { message: e.to_string() })?;

            // Find the appropriate runtime
            let runtime_name = &metadata.runtime;
            let runtime = self.runtimes.get_mut(runtime_name).ok_or_else(|| {
                PluginError::RuntimeNotAvailable {
                    runtime: runtime_name.clone(),
                }
            })?;

            // Find the entry point file
            let entry_file = path.join(&metadata.entry);
            if !entry_file.exists() {
                continue;
            }

            // Load the plugin
            let handle = runtime.load_plugin(&metadata.name, &entry_file)?;

            // Collect hooks
            let hooks: Vec<String> = [
                "on_navigate",
                "on_drill_down",
                "on_scan_start",
                "on_scan_complete",
                "on_delete_start",
                "on_delete_complete",
                "on_render",
                "on_action",
                "on_startup",
                "on_shutdown",
            ]
            .iter()
            .filter(|h| runtime.has_hook(handle, h))
            .map(|s| s.to_string())
            .collect();

            let loaded_plugin = LoadedPlugin {
                handle,
                id: metadata.name.clone(),
                metadata,
                path: path.clone(),
                hooks,
            };

            self.plugins.insert(handle, loaded_plugin.clone());
            loaded.push(loaded_plugin);
        }

        Ok(loaded)
    }

    /// Load a single plugin from a path.
    pub fn load_plugin(&mut self, path: &Path) -> PluginResult<LoadedPlugin> {
        // Parse metadata
        let toml_path = path.join("plugin.toml");
        let toml_content = std::fs::read_to_string(&toml_path)?;
        let metadata: PluginMetadata = toml::from_str(&toml_content)
            .map_err(|e| PluginError::ConfigError { message: e.to_string() })?;

        // Find runtime
        let runtime = self.runtimes.get_mut(&metadata.runtime).ok_or_else(|| {
            PluginError::RuntimeNotAvailable {
                runtime: metadata.runtime.clone(),
            }
        })?;

        // Load plugin
        let entry_file = path.join(&metadata.entry);
        let handle = runtime.load_plugin(&metadata.name, &entry_file)?;

        // Collect hooks
        let hooks: Vec<String> = [
            "on_navigate",
            "on_drill_down",
            "on_scan_start",
            "on_scan_complete",
            "on_render",
        ]
        .iter()
        .filter(|h| runtime.has_hook(handle, h))
        .map(|s| s.to_string())
        .collect();

        let loaded_plugin = LoadedPlugin {
            handle,
            id: metadata.name.clone(),
            metadata,
            path: path.to_path_buf(),
            hooks,
        };

        self.plugins.insert(handle, loaded_plugin.clone());
        Ok(loaded_plugin)
    }

    /// Unload a plugin.
    pub fn unload_plugin(&mut self, handle: PluginHandle) -> PluginResult<()> {
        if let Some(plugin) = self.plugins.remove(&handle) {
            if let Some(runtime) = self.runtimes.get_mut(&plugin.metadata.runtime) {
                runtime.unload_plugin(handle)?;
            }
        }
        Ok(())
    }

    /// Dispatch a hook to all plugins that implement it.
    pub async fn dispatch_hook(&self, hook: &Hook, ctx: &HookContext) -> Vec<HookResult> {
        let hook_name = hook.name();
        let mut results = vec![];

        for (handle, plugin) in &self.plugins {
            if !plugin.hooks.contains(&hook_name.to_string()) {
                continue;
            }

            if let Some(runtime) = self.runtimes.get(&plugin.metadata.runtime) {
                let result = if hook.is_sync() {
                    runtime.call_hook_sync(*handle, hook, ctx)
                } else {
                    runtime.call_hook_async(*handle, hook, ctx).await
                };

                match result {
                    Ok(r) => {
                        results.push(r.clone());
                        if r.stop_propagation {
                            break;
                        }
                    }
                    Err(e) => {
                        // Log error but continue to other plugins
                        eprintln!("Plugin {} hook error: {}", plugin.id, e);
                    }
                }
            }
        }

        results
    }

    /// Get all loaded plugins.
    pub fn plugins(&self) -> impl Iterator<Item = &LoadedPlugin> {
        self.plugins.values()
    }

    /// Get a loaded plugin by handle.
    pub fn get_plugin(&self, handle: PluginHandle) -> Option<&LoadedPlugin> {
        self.plugins.get(&handle)
    }

    /// Get plugins of a specific kind.
    pub fn plugins_of_kind(&self, kind: PluginKind) -> impl Iterator<Item = &LoadedPlugin> {
        self.plugins.values().filter(move |p| p.metadata.kind == kind)
    }

    /// Shutdown all runtimes.
    pub fn shutdown(&mut self) -> PluginResult<()> {
        for runtime in self.runtimes.values_mut() {
            runtime.shutdown()?;
        }
        self.plugins.clear();
        Ok(())
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new(PluginConfig::default())
    }
}
