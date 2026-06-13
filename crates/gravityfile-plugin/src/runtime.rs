//! Plugin runtime trait and manager.
//!
//! This module defines the language-agnostic [`PluginRuntime`] trait that
//! all scripting language implementations must satisfy.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use indexmap::IndexMap;

/// All hook names recognized by the plugin system.
///
/// This is the single source of truth used by both `discover_plugins` and
/// `load_plugin` when collecting the hooks a plugin implements.
pub(crate) const HOOK_NAMES: &[&str] = &[
    "on_navigate",
    "on_drill_down",
    "on_back",
    "on_scan_start",
    "on_scan_progress",
    "on_scan_complete",
    "on_scan_error",
    "on_delete_start",
    "on_delete_complete",
    "on_copy_start",
    "on_copy_complete",
    "on_move_start",
    "on_move_complete",
    "on_rename_start",
    "on_rename_complete",
    "on_duplicates_found",
    "on_age_analysis_complete",
    "on_render",
    "on_action",
    "on_mode_change",
    "on_selection_change",
    "on_startup",
    "on_shutdown",
    "on_plugin_load",
    "on_plugin_unload",
];

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
    pub(crate) fn new(id: usize) -> Self {
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
/// This trait abstracts over the specific scripting language (Lua or Rhai)
/// allowing plugins to be written in any supported language.
pub trait PluginRuntime: Send + Sync {
    /// Get the name of this runtime (e.g., "lua" or "rhai").
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
    fn create_isolated_context(
        &self,
        sandbox: &SandboxConfig,
    ) -> PluginResult<Box<dyn IsolatedContext>>;

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
    /// Manager-global handle for this plugin (unique across all runtimes).
    pub handle: PluginHandle,

    /// Runtime-local handle, used to call back into the owning runtime.
    /// Distinct from `handle` because each runtime numbers its plugins from 0,
    /// so two runtimes can hand out the same local handle.
    pub(crate) runtime_handle: PluginHandle,

    /// Plugin ID (usually directory name).
    pub id: String,

    /// Plugin metadata from plugin.toml.
    pub metadata: PluginMetadata,

    /// Path to the plugin directory.
    pub path: std::path::PathBuf,

    /// Hooks this plugin implements.
    pub hooks: Vec<String>,
}

/// Owned request passed through the hook-dispatch Tower service.
struct HookDispatch {
    hook: Hook,
    ctx: HookContext,
}

/// Manager for all plugin runtimes and loaded plugins.
pub struct PluginManager {
    /// Available runtimes keyed by name.
    runtimes: IndexMap<String, Box<dyn PluginRuntime>>,

    /// Loaded plugins keyed by handle, insertion-ordered for deterministic dispatch.
    plugins: IndexMap<PluginHandle, LoadedPlugin>,

    /// Plugin config.
    config: PluginConfig,

    /// Next manager-global plugin handle ID.
    next_handle: usize,
}

impl PluginManager {
    /// Validate a plugin name against the allowed character set.
    ///
    /// Names must be 1–64 characters long and match `[a-zA-Z0-9_-]+`.
    fn validate_plugin_name(name: &str) -> PluginResult<()> {
        if name.is_empty() || name.len() > 64 {
            return Err(PluginError::ConfigError {
                message: format!("Plugin name '{}' must be 1–64 characters long", name),
            });
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(PluginError::ConfigError {
                message: format!(
                    "Plugin name '{}' contains invalid characters (allowed: [a-zA-Z0-9_-])",
                    name
                ),
            });
        }
        Ok(())
    }

    /// Create a new plugin manager with the given configuration.
    pub fn new(config: PluginConfig) -> Self {
        Self {
            runtimes: IndexMap::new(),
            plugins: IndexMap::new(),
            config,
            next_handle: 0,
        }
    }

    /// Allocate a fresh manager-global plugin handle.
    fn alloc_handle(&mut self) -> PluginHandle {
        let handle = PluginHandle(self.next_handle);
        self.next_handle += 1;
        handle
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
        // Respect the global enable switch — when off, the subsystem does nothing.
        if !self.config.enabled {
            tracing::debug!("Plugin system disabled; skipping discovery");
            return Ok(vec![]);
        }

        let plugin_dir = self.config.plugin_dir.clone();
        if !plugin_dir.exists() {
            return Ok(vec![]);
        }

        let mut loaded = vec![];

        // Read plugin directories
        let entries = std::fs::read_dir(&plugin_dir).map_err(PluginError::Io)?;

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

            // Parse metadata. One broken plugin must not abort discovery of the
            // rest — log and skip it.
            let metadata: PluginMetadata = match std::fs::read_to_string(&toml_path)
                .map_err(|e| e.to_string())
                .and_then(|content| toml::from_str(&content).map_err(|e| e.to_string()))
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        path = %toml_path.display(),
                        error = %e,
                        "Skipping plugin with unreadable or invalid plugin.toml"
                    );
                    continue;
                }
            };

            // Validate plugin name to prevent injection or path-manipulation attacks
            if let Err(e) = Self::validate_plugin_name(&metadata.name) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Skipping plugin with invalid name"
                );
                continue;
            }

            // Honour the per-plugin disable list.
            if self.config.is_disabled(&metadata.name) {
                tracing::info!(plugin = %metadata.name, "Skipping disabled plugin");
                continue;
            }

            // Find the appropriate runtime
            let runtime_name = &metadata.runtime;
            let runtime = match self.runtimes.get_mut(runtime_name) {
                Some(r) => r,
                None => {
                    tracing::warn!(
                        plugin = %metadata.name,
                        runtime = %runtime_name,
                        "Skipping plugin: no runtime registered for it"
                    );
                    continue;
                }
            };

            // Find the entry point file and verify it stays within the plugin dir
            let entry_file = path.join(&metadata.entry);
            let canonical_plugin_dir = match std::fs::canonicalize(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let canonical_entry = match std::fs::canonicalize(&entry_file) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !canonical_entry.starts_with(&canonical_plugin_dir) {
                tracing::warn!(
                    plugin = %metadata.name,
                    entry = %metadata.entry,
                    "Skipping plugin: entry point escapes plugin directory"
                );
                continue;
            }
            if !entry_file.exists() {
                continue;
            }

            // Load the plugin (runtime-local handle). A single plugin failing to
            // load must not abort discovery of the rest.
            let runtime_handle = match runtime.load_plugin(&metadata.name, &entry_file) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        plugin = %metadata.name,
                        error = %e,
                        "Failed to load plugin"
                    );
                    continue;
                }
            };

            // Collect hooks
            let hooks: Vec<String> = HOOK_NAMES
                .iter()
                .filter(|h| runtime.has_hook(runtime_handle, h))
                .map(|s| s.to_string())
                .collect();

            // The `runtime` borrow ends above; allocate the global handle now.
            let handle = self.alloc_handle();
            let loaded_plugin = LoadedPlugin {
                handle,
                runtime_handle,
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
        let metadata: PluginMetadata =
            toml::from_str(&toml_content).map_err(|e| PluginError::ConfigError {
                message: e.to_string(),
            })?;

        // Validate plugin name
        Self::validate_plugin_name(&metadata.name)?;

        // Find runtime
        let runtime = self.runtimes.get_mut(&metadata.runtime).ok_or_else(|| {
            PluginError::RuntimeNotAvailable {
                runtime: metadata.runtime.clone(),
            }
        })?;

        // Verify entry point stays within the plugin directory (path traversal guard)
        let entry_file = path.join(&metadata.entry);
        let canonical_plugin_dir = std::fs::canonicalize(path).map_err(PluginError::Io)?;
        let canonical_entry = std::fs::canonicalize(&entry_file).map_err(PluginError::Io)?;
        if !canonical_entry.starts_with(&canonical_plugin_dir) {
            return Err(PluginError::ConfigError {
                message: format!(
                    "Plugin '{}': entry point '{}' escapes the plugin directory",
                    metadata.name, metadata.entry
                ),
            });
        }

        // Load plugin (runtime-local handle).
        let runtime_handle = runtime.load_plugin(&metadata.name, &entry_file)?;

        // Collect hooks
        let hooks: Vec<String> = HOOK_NAMES
            .iter()
            .filter(|h| runtime.has_hook(runtime_handle, h))
            .map(|s| s.to_string())
            .collect();

        // The `runtime` borrow ends above; allocate the global handle now.
        let handle = self.alloc_handle();
        let loaded_plugin = LoadedPlugin {
            handle,
            runtime_handle,
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
        if let Some(plugin) = self.plugins.shift_remove(&handle)
            && let Some(runtime) = self.runtimes.get_mut(&plugin.metadata.runtime)
        {
            runtime.unload_plugin(plugin.runtime_handle)?;
        }
        Ok(())
    }

    /// Dispatch a hook to all plugins that implement it.
    ///
    /// The dispatch runs through a small Tower service stack: a wall-clock
    /// `Timeout` layer bounds the whole dispatch (defense-in-depth alongside the
    /// in-VM CPU/memory guards in each runtime), and the inner service runs the
    /// per-plugin loop. Composing it this way means cross-cutting concerns
    /// (timeout, and future additions like concurrency limiting or load
    /// shedding) are layers rather than inline code.
    pub async fn dispatch_hook(&self, hook: &Hook, ctx: &HookContext) -> Vec<HookResult> {
        use std::time::Duration;
        use tower::{ServiceBuilder, ServiceExt, service_fn};

        // Overall budget scales with the plugin count so each plugin still gets
        // roughly its per-call timeout even when several are registered.
        let plugin_budget = self.config.default_timeout_ms.max(1);
        let active = self.plugins.len().max(1) as u64;
        let budget = Duration::from_millis(plugin_budget.saturating_mul(active).saturating_add(500));

        let request = HookDispatch {
            hook: hook.clone(),
            ctx: ctx.clone(),
        };

        let inner = service_fn(|req: HookDispatch| async move {
            Ok::<_, std::convert::Infallible>(self.run_hook_dispatch(&req.hook, &req.ctx).await)
        });

        let svc = ServiceBuilder::new().timeout(budget).service(inner);

        match svc.oneshot(request).await {
            Ok(results) => results,
            Err(_) => {
                tracing::warn!(
                    hook = %hook.name(),
                    "Hook dispatch exceeded its {budget:?} time budget; discarding results"
                );
                Vec::new()
            }
        }
    }

    /// The per-plugin dispatch loop (the inner Tower service body).
    async fn run_hook_dispatch(&self, hook: &Hook, ctx: &HookContext) -> Vec<HookResult> {
        let hook_name = hook.name();
        let mut results = vec![];

        for (_handle, plugin) in &self.plugins {
            if !plugin.hooks.iter().any(|h| h == hook_name) {
                continue;
            }

            if let Some(runtime) = self.runtimes.get(&plugin.metadata.runtime) {
                let rh = plugin.runtime_handle;
                let result = if hook.is_sync() {
                    runtime.call_hook_sync(rh, hook, ctx)
                } else {
                    runtime.call_hook_async(rh, hook, ctx).await
                };

                match result {
                    Ok(r) => {
                        let stop = r.stop_propagation;
                        results.push(r);
                        if stop {
                            break;
                        }
                    }
                    Err(e) => {
                        // Log error but continue to other plugins
                        tracing::warn!(
                            plugin_id = %plugin.id,
                            error = %e,
                            "Plugin hook dispatch error"
                        );
                    }
                }
            }
        }

        results
    }

    /// Returns true if any hook result requested that the host's default action
    /// be prevented. Callers that gate a default action on plugins (navigation,
    /// deletion, …) can use this on the [`dispatch_hook`](Self::dispatch_hook)
    /// results.
    pub fn any_prevent_default(results: &[HookResult]) -> bool {
        results.iter().any(|r| r.prevent_default)
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
        self.plugins
            .values()
            .filter(move |p| p.metadata.kind == kind)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PluginMetadata;
    use crate::hooks::HookResult;
    use std::time::Duration;

    /// A minimal runtime whose async hook sleeps for a configurable delay,
    /// letting us exercise the Tower dispatch timeout budget deterministically.
    struct SleepRuntime {
        delay: Duration,
    }

    impl PluginRuntime for SleepRuntime {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &[".mock"]
        }
        fn init(&mut self, _config: &PluginConfig) -> PluginResult<()> {
            Ok(())
        }
        fn load_plugin(&mut self, _id: &str, _source: &Path) -> PluginResult<PluginHandle> {
            Ok(PluginHandle(0))
        }
        fn unload_plugin(&mut self, _handle: PluginHandle) -> PluginResult<()> {
            Ok(())
        }
        fn get_metadata(&self, _handle: PluginHandle) -> Option<&PluginMetadata> {
            None
        }
        fn has_hook(&self, _handle: PluginHandle, _hook_name: &str) -> bool {
            true
        }
        fn call_hook_sync(
            &self,
            _handle: PluginHandle,
            _hook: &Hook,
            _ctx: &HookContext,
        ) -> PluginResult<HookResult> {
            Ok(HookResult {
                handled: true,
                ..Default::default()
            })
        }
        fn call_hook_async<'a>(
            &'a self,
            _handle: PluginHandle,
            _hook: &'a Hook,
            _ctx: &'a HookContext,
        ) -> BoxFuture<'a, PluginResult<HookResult>> {
            let delay = self.delay;
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(HookResult {
                    handled: true,
                    ..Default::default()
                })
            })
        }
        fn call_method<'a>(
            &'a self,
            _handle: PluginHandle,
            _method: &'a str,
            _args: Vec<Value>,
        ) -> BoxFuture<'a, PluginResult<Value>> {
            Box::pin(async { Ok(Value::Null) })
        }
        fn create_isolated_context(
            &self,
            _sandbox: &SandboxConfig,
        ) -> PluginResult<Box<dyn IsolatedContext>> {
            Err(PluginError::RuntimeNotAvailable {
                runtime: "mock".into(),
            })
        }
        fn loaded_plugins(&self) -> Vec<PluginHandle> {
            vec![PluginHandle(0)]
        }
        fn shutdown(&mut self) -> PluginResult<()> {
            Ok(())
        }
    }

    fn mock_metadata() -> PluginMetadata {
        PluginMetadata {
            name: "mock".into(),
            version: "0.0.0".into(),
            description: String::new(),
            author: String::new(),
            url: String::new(),
            runtime: "mock".into(),
            kind: PluginKind::default(),
            entry: "main.mock".into(),
            min_version: None,
            hooks: Default::default(),
            permissions: Default::default(),
            dependencies: vec![],
        }
    }

    /// Build a manager wired with a single mock plugin subscribed to `on_startup`,
    /// whose async hook sleeps for `delay`.
    fn manager_with_sleeping_plugin(timeout_ms: u64, delay: Duration) -> PluginManager {
        let config = PluginConfig {
            default_timeout_ms: timeout_ms,
            ..Default::default()
        };
        let mut pm = PluginManager::new(config);
        pm.register_runtime(Box::new(SleepRuntime { delay }))
            .unwrap();
        let handle = pm.alloc_handle();
        pm.plugins.insert(
            handle,
            LoadedPlugin {
                handle,
                runtime_handle: PluginHandle(0),
                id: "mock".into(),
                metadata: mock_metadata(),
                path: std::path::PathBuf::from("/tmp/mock"),
                hooks: vec!["on_startup".into()],
            },
        );
        pm
    }

    #[tokio::test]
    async fn dispatch_returns_result_within_budget() {
        // Hook completes well inside the budget → its result is returned.
        let pm = manager_with_sleeping_plugin(50, Duration::from_millis(0));
        let ctx = HookContext::new();
        let results = pm.dispatch_hook(&Hook::OnStartup, &ctx).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].handled);
    }

    #[test]
    fn any_prevent_default_reflects_veto() {
        let allow = HookResult {
            handled: true,
            ..Default::default()
        };
        let veto = HookResult {
            handled: true,
            prevent_default: true,
            ..Default::default()
        };
        assert!(!PluginManager::any_prevent_default(&[]));
        assert!(!PluginManager::any_prevent_default(std::slice::from_ref(
            &allow
        )));
        assert!(PluginManager::any_prevent_default(&[allow, veto]));
    }

    #[tokio::test]
    async fn dispatch_discards_results_past_time_budget() {
        // budget = default_timeout_ms * active(1) + 500ms ≈ 510ms.
        // A 2s hook blows the budget, so dispatch sheds and returns nothing.
        let pm = manager_with_sleeping_plugin(10, Duration::from_secs(2));
        let ctx = HookContext::new();
        let results = pm.dispatch_hook(&Hook::OnStartup, &ctx).await;
        assert!(
            results.is_empty(),
            "expected empty results when dispatch exceeds its time budget"
        );
    }
}
