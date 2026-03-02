//! WASM runtime implementation using Extism.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use extism::{Manifest, Plugin, Wasm};

use crate::config::{PluginConfig, PluginMetadata};
use crate::hooks::{Hook, HookContext, HookResult};
use crate::runtime::{BoxFuture, IsolatedContext, PluginHandle, PluginRuntime};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult, Value};

use super::isolate::WasmIsolatedContext;

/// A loaded WASM plugin.
struct LoadedWasmPlugin {
    /// Plugin name/id.
    name: String,

    /// Plugin metadata.
    metadata: PluginMetadata,

    /// Hooks implemented by this plugin.
    hooks: Vec<String>,

    /// The Extism plugin instance.
    plugin: Arc<Mutex<Plugin>>,
}

/// WASM plugin runtime.
pub struct WasmRuntime {
    /// Loaded plugins by handle.
    plugins: HashMap<PluginHandle, LoadedWasmPlugin>,

    /// Next plugin handle ID.
    next_handle: usize,

    /// Runtime configuration.
    config: Option<PluginConfig>,

    /// Whether the runtime has been initialized.
    initialized: bool,
}

impl WasmRuntime {
    /// Create a new WASM runtime.
    pub fn new() -> PluginResult<Self> {
        Ok(Self {
            plugins: HashMap::new(),
            next_handle: 0,
            config: None,
            initialized: false,
        })
    }
}

impl Default for WasmRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create WASM runtime")
    }
}

impl PluginRuntime for WasmRuntime {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &[".wasm"]
    }

    fn init(&mut self, config: &PluginConfig) -> PluginResult<()> {
        if self.initialized {
            return Ok(());
        }

        self.config = Some(config.clone());
        self.initialized = true;

        Ok(())
    }

    fn load_plugin(&mut self, id: &str, source: &Path) -> PluginResult<PluginHandle> {
        let wasm = Wasm::file(source);
        let manifest = Manifest::new([wasm]);

        let plugin = Plugin::new(&manifest, [], true).map_err(|e| PluginError::LoadError {
            name: id.to_string(),
            message: e.to_string(),
        })?;

        // Determine which hooks are available by checking exports
        let mut hooks = vec![];
        for hook_name in [
            "on_navigate",
            "on_drill_down",
            "on_back",
            "on_scan_start",
            "on_scan_progress",
            "on_scan_complete",
            "on_delete_start",
            "on_delete_complete",
            "on_copy_start",
            "on_copy_complete",
            "on_move_start",
            "on_move_complete",
            "on_render",
            "on_action",
            "on_mode_change",
            "on_startup",
            "on_shutdown",
        ] {
            if plugin.function_exists(hook_name) {
                hooks.push(hook_name.to_string());
            }
        }

        let handle = PluginHandle::new(self.next_handle);
        self.next_handle += 1;

        let metadata = PluginMetadata {
            name: id.to_string(),
            runtime: "wasm".to_string(),
            ..Default::default()
        };

        self.plugins.insert(
            handle,
            LoadedWasmPlugin {
                name: id.to_string(),
                metadata,
                hooks,
                plugin: Arc::new(Mutex::new(plugin)),
            },
        );

        Ok(handle)
    }

    fn unload_plugin(&mut self, handle: PluginHandle) -> PluginResult<()> {
        self.plugins.remove(&handle);
        Ok(())
    }

    fn get_metadata(&self, handle: PluginHandle) -> Option<&PluginMetadata> {
        self.plugins.get(&handle).map(|p| &p.metadata)
    }

    fn has_hook(&self, handle: PluginHandle, hook_name: &str) -> bool {
        self.plugins
            .get(&handle)
            .map(|p| p.hooks.contains(&hook_name.to_string()))
            .unwrap_or(false)
    }

    fn call_hook_sync(
        &self,
        handle: PluginHandle,
        hook: &Hook,
        _ctx: &HookContext,
    ) -> PluginResult<HookResult> {
        let plugin = self
            .plugins
            .get(&handle)
            .ok_or_else(|| PluginError::NotFound {
                path: std::path::PathBuf::new(),
            })?;

        let mut extism_plugin = plugin
            .plugin
            .lock()
            .map_err(|_| PluginError::ExecutionError {
                name: plugin.name.clone(),
                message: "Mutex lock failed".to_string(),
            })?;

        let hook_name = hook.name();

        let payload = serde_json::to_vec(hook).map_err(|e| PluginError::ExecutionError {
            name: plugin.name.clone(),
            message: e.to_string(),
        })?;

        let res = extism_plugin.call::<&[u8], &[u8]>(hook_name, &payload);

        match res {
            Ok(output) => {
                if output.is_empty() {
                    return Ok(HookResult::ok());
                }

                let result: HookResult =
                    serde_json::from_slice(output).map_err(|e| PluginError::ExecutionError {
                        name: plugin.name.clone(),
                        message: format!("Failed to parse WASM output: {}", e),
                    })?;

                Ok(result)
            }
            Err(e) => {
                eprintln!(
                    "WASM plugin '{}' hook '{}' failed: {}",
                    plugin.name, hook_name, e
                );
                Ok(HookResult::ok())
            }
        }
    }

    fn call_hook_async<'a>(
        &'a self,
        handle: PluginHandle,
        hook: &'a Hook,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, PluginResult<HookResult>> {
        // Run synchronous in a background task
        Box::pin(async move { self.call_hook_sync(handle, hook, ctx) })
    }

    fn call_method<'a>(
        &'a self,
        handle: PluginHandle,
        method: &'a str,
        args: Vec<Value>,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            let plugin = self
                .plugins
                .get(&handle)
                .ok_or_else(|| PluginError::NotFound {
                    path: std::path::PathBuf::new(),
                })?;

            let mut extism_plugin =
                plugin
                    .plugin
                    .lock()
                    .map_err(|_| PluginError::ExecutionError {
                        name: plugin.name.clone(),
                        message: "Mutex lock failed".to_string(),
                    })?;

            let payload = serde_json::to_vec(&args).map_err(|e| PluginError::ExecutionError {
                name: plugin.name.clone(),
                message: e.to_string(),
            })?;

            let res = extism_plugin
                .call::<&[u8], &[u8]>(method, payload.as_slice())
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })?;

            if res.is_empty() {
                return Ok(Value::Null);
            }

            let val: Value =
                serde_json::from_slice(res).map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: format!("Failed to parse output: {}", e),
                })?;

            Ok(val)
        })
    }

    fn create_isolated_context(
        &self,
        sandbox: &SandboxConfig,
    ) -> PluginResult<Box<dyn IsolatedContext>> {
        Ok(Box::new(WasmIsolatedContext::new(sandbox.clone())?))
    }

    fn loaded_plugins(&self) -> Vec<PluginHandle> {
        self.plugins.keys().copied().collect()
    }

    fn shutdown(&mut self) -> PluginResult<()> {
        self.plugins.clear();
        Ok(())
    }
}
