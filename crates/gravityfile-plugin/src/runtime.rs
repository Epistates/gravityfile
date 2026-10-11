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
    "on_scan_start",
    "on_scan_complete",
    "on_scan_error",
    "on_delete_start",
    "on_delete_complete",
    "on_copy_complete",
    "on_move_complete",
    "on_rename_complete",
    "on_duplicates_found",
    "on_age_analysis_complete",
    "on_startup",
    "on_shutdown",
];

use tokio_util::sync::CancellationToken;

use crate::config::{PluginConfig, PluginMetadata};
use crate::hooks::{Hook, HookContext, HookResult};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginKind, PluginResult, Value};

/// Bound source loading independently of VM limits: source strings are allocated
/// by Rust before either scripting engine can enforce its own memory budget.
pub(crate) fn read_plugin_source(path: &Path) -> PluginResult<String> {
    read_regular_utf8(path, 1024 * 1024, "plugin source")
}

fn read_plugin_manifest(path: &Path) -> PluginResult<PluginMetadata> {
    let content = read_regular_utf8(path, 64 * 1024, "plugin manifest")?;
    toml::from_str(&content).map_err(|error| PluginError::ConfigError {
        message: error.to_string(),
    })
}

fn read_regular_utf8(path: &Path, limit: u64, kind: &str) -> PluginResult<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(PluginError::LoadError {
            name: path.display().to_string(),
            message: format!("{kind} must be a regular UTF-8 file of at most {limit} bytes"),
        });
    }
    let mut source = String::new();
    file.take(limit + 1).read_to_string(&mut source)?;
    if source.len() as u64 > limit {
        return Err(PluginError::LoadError {
            name: path.display().to_string(),
            message: format!("{kind} grew beyond {limit} bytes"),
        });
    }
    Ok(source)
}

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
/// Implemented by the in-process Lua engine and by the worker-process
/// runtime that hosts it.
pub trait PluginRuntime: Send + Sync {
    /// Get the name of this runtime (e.g. "lua").
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
    /// Isolated contexts have limited API access and their own Lua state,
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

    fn validate_load_policy(&self, name: &str) -> PluginResult<()> {
        if self.config.is_disabled(name) {
            return Err(PluginError::ConfigError {
                message: format!("Plugin '{name}' is disabled"),
            });
        }
        if self.plugins.values().any(|plugin| plugin.id == name) {
            return Err(PluginError::ConfigError {
                message: format!("Plugin '{name}' is already loaded"),
            });
        }
        Ok(())
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
        if self.runtimes.contains_key(&name) {
            return Err(PluginError::ConfigError {
                message: format!("Runtime '{name}' is already registered"),
            });
        }
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
        match std::fs::metadata(&plugin_dir) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        }

        let mut loaded = vec![];

        // Read plugin directories
        let mut paths = std::fs::read_dir(&plugin_dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();

        for path in paths {
            if !path.is_dir() {
                continue;
            }

            // Look for plugin.toml
            let toml_path = path.join("plugin.toml");
            if !toml_path.exists() {
                continue;
            }

            // Explicit loading is the single policy and publication path.
            // Discovery differs only in continuing after an individual failure.
            match self.load_plugin(&path) {
                Ok(plugin) => loaded.push(plugin),
                Err(error) => tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "Skipping plugin that could not be loaded"
                ),
            }
        }

        Ok(loaded)
    }

    /// Load a single plugin from a path.
    pub fn load_plugin(&mut self, path: &Path) -> PluginResult<LoadedPlugin> {
        if !self.config.enabled {
            return Err(PluginError::ConfigError {
                message: "Plugin system is disabled".into(),
            });
        }
        // Parse metadata
        let toml_path = path.join("plugin.toml");
        let metadata = read_plugin_manifest(&toml_path)?;

        // Validate plugin name
        Self::validate_plugin_name(&metadata.name)?;
        self.validate_load_policy(&metadata.name)?;

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
        let Some(plugin) = self.plugins.get(&handle) else {
            return Ok(());
        };
        let runtime = self
            .runtimes
            .get_mut(&plugin.metadata.runtime)
            .ok_or_else(|| PluginError::RuntimeNotAvailable {
                runtime: plugin.metadata.runtime.clone(),
            })?;
        runtime.unload_plugin(plugin.runtime_handle)?;
        self.plugins.shift_remove(&handle);
        Ok(())
    }

    /// Dispatch a hook to all plugins that implement it.
    ///
    /// The overall timeout bounds cooperatively yielding hook futures; worker
    /// deadlines and the runtimes' instruction hooks bound script execution.
    pub async fn dispatch_hook(&self, hook: &Hook, ctx: &HookContext) -> Vec<HookResult> {
        use std::time::Duration;

        // Overall budget scales with the plugin count so each plugin still gets
        // roughly its per-call timeout even when several are registered.
        let plugin_budget = self.config.default_timeout_ms.max(1);
        let active = self.plugins.len().max(1) as u64;
        let budget =
            Duration::from_millis(plugin_budget.saturating_mul(active).saturating_add(500));

        match tokio::time::timeout(budget, self.run_hook_dispatch(hook, ctx)).await {
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

    /// The per-plugin dispatch loop.
    async fn run_hook_dispatch(&self, hook: &Hook, ctx: &HookContext) -> Vec<HookResult> {
        let hook_name = hook.name();
        let mut results = vec![];

        for (_handle, plugin) in &self.plugins {
            if !plugin.hooks.iter().any(|h| h == hook_name) {
                continue;
            }

            if let Some(runtime) = self.runtimes.get(&plugin.metadata.runtime) {
                let rh = plugin.runtime_handle;
                match runtime.call_hook_async(rh, hook, ctx).await {
                    Ok(mut r) => {
                        r.plugin = Some(plugin.id.clone());
                        let stop = r.stop_propagation;
                        results.push(r);
                        if stop {
                            break;
                        }
                    }
                    Err(e) => {
                        // Report it to the host, and continue with other plugins.
                        tracing::warn!(
                            plugin_id = %plugin.id,
                            error = %e,
                            "Plugin hook dispatch error"
                        );
                        let mut failed = HookResult::error(e.to_string());
                        failed.plugin = Some(plugin.id.clone());
                        results.push(failed);
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
        let mut first_error = None;
        for (name, runtime) in self.runtimes.iter_mut() {
            match runtime.shutdown() {
                Ok(()) => self
                    .plugins
                    .retain(|_, plugin| plugin.metadata.runtime != *name),
                Err(error) => {
                    tracing::warn!(runtime = %name, error = %error, "Plugin runtime shutdown failed");
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
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

    #[tokio::test]
    async fn discovery_order_defines_stable_identity_selection_and_hook_order() {
        let root = tempfile::tempdir().unwrap();
        // Deliberately create in a different order than the documented path order.
        for (directory, name, value) in [
            ("zeta", "second", 2),
            ("mirror", "first", 99),
            ("alpha", "first", 1),
        ] {
            let path = root.path().join(directory);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("plugin.toml"), format!("name = '{name}'\n")).unwrap();
            std::fs::write(path.join("main.lua"), format!("return {{ on_startup = function(self, hook) return {{ handled = true, value = {value} }} end }}")).unwrap();
        }
        let mut manager =
            PluginManager::new(PluginConfig::default().with_plugin_dir(root.path().to_path_buf()));
        manager
            .register_runtime(Box::new(crate::lua::LuaRuntime::new().unwrap()))
            .unwrap();
        manager.init_runtimes().unwrap();
        let loaded = manager.discover_plugins().await.unwrap();
        assert_eq!(
            loaded
                .iter()
                .map(|plugin| plugin.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(loaded[0].path, root.path().join("alpha"));
        let results = manager
            .dispatch_hook(&Hook::OnStartup, &HookContext::new())
            .await;
        assert_eq!(
            results
                .iter()
                .map(|result| result.value.as_ref().and_then(Value::as_i64))
                .collect::<Vec<_>>(),
            [Some(1), Some(2)]
        );
    }

    #[tokio::test]
    async fn oversized_manifest_is_rejected_without_blocking_healthy_discovery() {
        let root = tempfile::tempdir().unwrap();
        let bad = root.path().join("oversized");
        let good = root.path().join("healthy");
        std::fs::create_dir(&bad).unwrap();
        std::fs::create_dir(&good).unwrap();
        std::fs::File::create(bad.join("plugin.toml"))
            .unwrap()
            .set_len(64 * 1024 + 1)
            .unwrap();
        std::fs::write(good.join("plugin.toml"), "name = 'healthy'\n").unwrap();
        std::fs::write(good.join("main.lua"), "return {}").unwrap();
        let mut manager =
            PluginManager::new(PluginConfig::default().with_plugin_dir(root.path().to_path_buf()));
        manager
            .register_runtime(Box::new(crate::lua::LuaRuntime::new().unwrap()))
            .unwrap();
        manager.init_runtimes().unwrap();
        assert!(manager.load_plugin(&bad).is_err());
        let loaded = manager.discover_plugins().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "healthy");
    }

    #[tokio::test]
    async fn explicit_loading_obeys_disable_policy_before_runtime_execution() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::new(PluginConfig {
            enabled: false,
            ..Default::default()
        });
        assert!(matches!(
            manager.load_plugin(&root.path().join("absent")),
            Err(PluginError::ConfigError { .. })
        ));
        assert!(manager.discover_plugins().await.unwrap().is_empty());
        std::fs::write(root.path().join("plugin.toml"), "name = 'blocked'\n").unwrap();
        let mut manager = PluginManager::new(PluginConfig::default().disable_plugin("blocked"));
        // No runtime or entry file: policy must reject before either is consulted.
        let error = manager.load_plugin(root.path()).unwrap_err();
        assert!(
            matches!(error, PluginError::ConfigError { message } if message.contains("disabled"))
        );
    }

    #[tokio::test]
    async fn rediscovery_and_explicit_load_do_not_duplicate_plugin_identity() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("one");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.toml"), "name = 'one'\n").unwrap();
        std::fs::write(plugin.join("main.lua"), "return {}").unwrap();
        let mut manager =
            PluginManager::new(PluginConfig::default().with_plugin_dir(root.path().to_path_buf()));
        manager
            .register_runtime(Box::new(crate::lua::LuaRuntime::new().unwrap()))
            .unwrap();
        manager.init_runtimes().unwrap();
        let first = manager.discover_plugins().await.unwrap();
        assert_eq!(first.len(), 1);
        assert!(manager.discover_plugins().await.unwrap().is_empty());
        assert!(manager.load_plugin(&plugin).is_err());
        assert_eq!(manager.plugins.len(), 1);
        manager.unload_plugin(first[0].handle).unwrap();
        assert!(manager.load_plugin(&plugin).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn manifest_reader_rejects_symlinks_and_fifos() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("actual.toml");
        std::fs::write(&target, "name = 'healthy'\n").unwrap();
        let link = root.path().join("link.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_plugin_manifest(&link).is_err());
        let fifo = root.path().join("pipe.toml");
        let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the NUL-terminated path is valid for this call, in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        assert!(read_plugin_manifest(&fifo).is_err());
    }

    #[test]
    fn source_loading_rejects_oversize_and_nonregular_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("large.lua");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        assert!(read_plugin_source(&path).is_err());
        assert!(read_plugin_source(root.path()).is_err());
        std::fs::write(&path, "return {}").unwrap();
        assert_eq!(read_plugin_source(&path).unwrap(), "return {}");
        #[cfg(unix)]
        {
            let link = root.path().join("link.lua");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_plugin_source(&link).is_err());
        }
    }

    /// A minimal runtime whose async hook sleeps for a configurable delay,
    /// letting us exercise the Tower dispatch timeout budget deterministically.
    #[derive(Default)]
    struct SleepRuntime {
        delay: Duration,
        runtime_name: Option<&'static str>,
        fail_cleanup: std::sync::Arc<std::sync::atomic::AtomicBool>,
        shutdown_calls: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl PluginRuntime for SleepRuntime {
        fn name(&self) -> &'static str {
            self.runtime_name.unwrap_or("mock")
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
            if self.fail_cleanup.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(PluginError::ConfigError {
                    message: "injected cleanup failure".into(),
                });
            }
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
            self.shutdown_calls.lock().unwrap().push(self.name());
            self.unload_plugin(PluginHandle(0))
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
        pm.register_runtime(Box::new(SleepRuntime {
            delay,
            ..Default::default()
        }))
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

    #[test]
    fn runtime_registration_cannot_replace_loaded_handle_ownership() {
        let mut pm = manager_with_sleeping_plugin(100, Duration::ZERO);
        assert!(pm.register_runtime(Box::<SleepRuntime>::default()).is_err());
        assert_eq!(pm.plugins.len(), 1);
        assert_eq!(pm.runtimes.len(), 1);
        pm.unload_plugin(PluginHandle(0)).unwrap();
        assert!(pm.plugins.is_empty());
    }

    #[test]
    fn failed_unload_preserves_handle_for_retry() {
        let mut pm = manager_with_sleeping_plugin(100, Duration::ZERO);
        let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        pm.runtimes.clear();
        pm.register_runtime(Box::new(SleepRuntime {
            fail_cleanup: fail.clone(),
            ..Default::default()
        }))
        .unwrap();
        assert!(pm.unload_plugin(PluginHandle(0)).is_err());
        assert!(pm.plugins.contains_key(&PluginHandle(0)));
        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        pm.unload_plugin(PluginHandle(0)).unwrap();
        assert!(pm.plugins.is_empty());
    }

    #[test]
    fn shutdown_attempts_every_runtime_and_retains_only_failed_ownership() {
        let mut pm = manager_with_sleeping_plugin(100, Duration::ZERO);
        let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        pm.runtimes.clear();
        pm.register_runtime(Box::new(SleepRuntime {
            fail_cleanup: fail.clone(),
            shutdown_calls: calls.clone(),
            ..Default::default()
        }))
        .unwrap();
        pm.register_runtime(Box::new(SleepRuntime {
            runtime_name: Some("healthy"),
            shutdown_calls: calls.clone(),
            ..Default::default()
        }))
        .unwrap();
        let mut healthy = pm.plugins[&PluginHandle(0)].clone();
        healthy.handle = pm.alloc_handle();
        healthy.metadata.runtime = "healthy".into();
        pm.plugins.insert(healthy.handle, healthy);
        assert!(pm.shutdown().is_err());
        assert_eq!(*calls.lock().unwrap(), ["mock", "healthy"]);
        assert_eq!(pm.plugins.len(), 1);
        assert!(pm.plugins.contains_key(&PluginHandle(0)));
        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        pm.shutdown().unwrap();
        assert!(pm.plugins.is_empty());
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
