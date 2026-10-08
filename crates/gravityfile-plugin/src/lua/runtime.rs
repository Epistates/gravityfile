//! Lua runtime implementation.

use std::collections::HashMap;
use std::path::Path;

use mlua::{Function, Lua, MultiValue, Table, Value as LuaValue};

use crate::config::{PluginConfig, PluginMetadata};
use crate::hooks::{Hook, HookContext, HookResult};
use crate::runtime::{BoxFuture, HOOK_NAMES, IsolatedContext, PluginHandle, PluginRuntime};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult, Value};

use super::bindings;
use super::convert::{lua_to_value, value_to_lua};
use super::isolate::LuaIsolatedContext;

/// A loaded Lua plugin.
struct LoadedLuaPlugin {
    /// Plugin name/id.
    name: String,

    /// The plugin's module table.
    module: mlua::RegistryKey,

    /// Plugin metadata.
    metadata: PluginMetadata,

    /// Hooks implemented by this plugin.
    hooks: Vec<String>,
}

/// Lua plugin runtime.
pub struct LuaRuntime {
    /// The main Lua state.
    lua: Lua,

    /// Loaded plugins by handle.
    plugins: HashMap<PluginHandle, LoadedLuaPlugin>,

    /// Next plugin handle ID.
    next_handle: usize,

    /// Runtime configuration.
    config: Option<PluginConfig>,

    /// Sandbox configuration used to gate filesystem API access.
    sandbox: SandboxConfig,

    /// Whether the runtime has been initialized.
    initialized: bool,
}

impl LuaRuntime {
    /// Create a new Lua runtime.
    pub fn new() -> PluginResult<Self> {
        let lua = Lua::new();

        // Disable potentially dangerous standard library functions
        {
            let globals = lua.globals();
            let map_err = |e: mlua::Error| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            };
            globals.set("loadfile", LuaValue::Nil).map_err(map_err)?;
            globals
                .set("dofile", LuaValue::Nil)
                .map_err(|e| PluginError::LoadError {
                    name: "lua".into(),
                    message: e.to_string(),
                })?;
            globals
                .set("load", LuaValue::Nil)
                .map_err(|e| PluginError::LoadError {
                    name: "lua".into(),
                    message: e.to_string(),
                })?;
            globals
                .set("os", LuaValue::Nil)
                .map_err(|e| PluginError::LoadError {
                    name: "lua".into(),
                    message: e.to_string(),
                })?;
            globals
                .set("io", LuaValue::Nil)
                .map_err(|e| PluginError::LoadError {
                    name: "lua".into(),
                    message: e.to_string(),
                })?;
            globals
                .set("debug", LuaValue::Nil)
                .map_err(|e| PluginError::LoadError {
                    name: "lua".into(),
                    message: e.to_string(),
                })?;
            globals
                .set("require", LuaValue::Nil)
                .map_err(|e| PluginError::LoadError {
                    name: "lua".into(),
                    message: e.to_string(),
                })?;
            globals
                .set("package", LuaValue::Nil)
                .map_err(|e| PluginError::LoadError {
                    name: "lua".into(),
                    message: e.to_string(),
                })?;
            // Disable string.dump to prevent bytecode extraction
            if let Ok(string_table) = globals.get::<mlua::Table>("string") {
                string_table.set("dump", LuaValue::Nil).ok();
            }
        }

        Ok(Self {
            lua,
            plugins: HashMap::new(),
            next_handle: 0,
            config: None,
            sandbox: SandboxConfig::default(),
            initialized: false,
        })
    }

    /// Initialize the Lua runtime with the gravityfile API.
    fn init_api(&self) -> PluginResult<()> {
        let globals = self.lua.globals();

        // Create the 'gf' namespace (gravityfile API)
        let gf = self
            .lua
            .create_table()
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: format!("Failed to create gf table: {}", e),
            })?;

        // Add version info
        gf.set("version", env!("CARGO_PKG_VERSION"))
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;

        // Add logging functions
        let log_info = self
            .lua
            .create_function(|_, msg: String| {
                tracing::info!(target: "plugin", "{}", msg);
                Ok(())
            })
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;
        gf.set("log_info", log_info).ok();

        let log_warn = self
            .lua
            .create_function(|_, msg: String| {
                tracing::warn!(target: "plugin", "{}", msg);
                Ok(())
            })
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;
        gf.set("log_warn", log_warn).ok();

        let log_error = self
            .lua
            .create_function(|_, msg: String| {
                tracing::error!(target: "plugin", "{}", msg);
                Ok(())
            })
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;
        gf.set("log_error", log_error).ok();

        // Add notify function
        let notify = self
            .lua
            .create_function(|_, (msg, level): (String, Option<String>)| {
                let level = level.unwrap_or_else(|| "info".to_string());
                tracing::info!(target: "plugin_notify", level = level, "{}", msg);
                Ok(())
            })
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;
        gf.set("notify", notify).ok();

        globals.set("gf", gf).map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;

        // Create the 'fs' namespace (filesystem API), gated by the runtime sandbox.
        let fs = bindings::create_fs_api(&self.lua, Some(self.sandbox.clone()))?;
        globals.set("fs", fs).map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;

        // Create the 'ui' namespace (UI elements)
        let ui = bindings::create_ui_api(&self.lua)?;
        globals.set("ui", ui).map_err(|e| PluginError::LoadError {
            name: "lua".into(),
            message: e.to_string(),
        })?;

        Ok(())
    }

    /// Convert a Hook to a Lua table.
    fn hook_to_lua(&self, lua: &Lua, hook: &Hook) -> mlua::Result<Table> {
        let table = lua.create_table()?;

        // Serialize hook to JSON then to Lua table
        let json = serde_json::to_string(hook).map_err(mlua::Error::external)?;
        let json_val: serde_json::Value =
            serde_json::from_str(&json).map_err(mlua::Error::external)?;

        fn json_to_lua(lua: &Lua, val: &serde_json::Value) -> mlua::Result<LuaValue> {
            match val {
                serde_json::Value::Null => Ok(LuaValue::Nil),
                serde_json::Value::Bool(b) => Ok(LuaValue::Boolean(*b)),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Ok(LuaValue::Integer(i))
                    } else {
                        Ok(LuaValue::Number(n.as_f64().unwrap_or(0.0)))
                    }
                }
                serde_json::Value::String(s) => Ok(LuaValue::String(lua.create_string(s)?)),
                serde_json::Value::Array(arr) => {
                    let t = lua.create_table()?;
                    for (i, v) in arr.iter().enumerate() {
                        t.set(i + 1, json_to_lua(lua, v)?)?;
                    }
                    Ok(LuaValue::Table(t))
                }
                serde_json::Value::Object(obj) => {
                    let t = lua.create_table()?;
                    for (k, v) in obj {
                        t.set(k.as_str(), json_to_lua(lua, v)?)?;
                    }
                    Ok(LuaValue::Table(t))
                }
            }
        }

        if let serde_json::Value::Object(obj) = json_val {
            for (k, v) in obj {
                table.set(k.as_str(), json_to_lua(lua, &v)?)?;
            }
        }

        Ok(table)
    }

    /// Run `f` under a CPU watchdog: an instruction-count hook aborts execution
    /// once the wall-clock deadline (from the sandbox timeout) passes. This
    /// bounds runaway plugins on the shared Lua state without requiring the state
    /// to be `Send` (so we avoid `spawn_blocking`). A timeout surfaces as an
    /// execution error from the wrapped Lua call.
    fn with_deadline<T>(&self, f: impl FnOnce() -> PluginResult<T>) -> PluginResult<T> {
        use std::time::{Duration, Instant};

        let timeout_ms = self.sandbox.timeout_ms;
        if timeout_ms == 0 {
            return f();
        }

        let Some(deadline) = Instant::now().checked_add(Duration::from_millis(timeout_ms)) else {
            return f();
        };
        let _ = self.lua.set_hook(
            mlua::HookTriggers::new().every_nth_instruction(100_000),
            move |_lua, _debug| {
                if Instant::now() >= deadline {
                    Err(mlua::Error::external("plugin execution timed out"))
                } else {
                    Ok(mlua::VmState::Continue)
                }
            },
        );
        let result = f();
        self.lua.remove_hook();
        result
    }
}

impl PluginRuntime for LuaRuntime {
    fn name(&self) -> &'static str {
        "lua"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &[".lua"]
    }

    fn init(&mut self, config: &PluginConfig) -> PluginResult<()> {
        if self.initialized {
            return Ok(());
        }

        // Build a sandbox from plugin config settings. The default grants the
        // Read permission but no paths; wiring the configured roots is what makes
        // the filesystem API actually usable (and scoped).
        self.sandbox = SandboxConfig {
            timeout_ms: config.default_timeout_ms,
            max_memory: config.max_memory_mb.saturating_mul(1024 * 1024),
            allow_network: config.allow_network,
            allowed_read_paths: config.fs_read_roots.clone(),
            ..SandboxConfig::default()
        };

        // Bound memory on the shared Lua state so a runaway plugin can't OOM the
        // whole TUI. 0 means "unlimited" in our config; only set a real cap.
        if self.sandbox.max_memory > 0 {
            let _ = self.lua.set_memory_limit(self.sandbox.max_memory);
        }

        self.config = Some(config.clone());
        self.init_api()?;
        self.initialized = true;

        Ok(())
    }

    fn load_plugin(&mut self, id: &str, source: &Path) -> PluginResult<PluginHandle> {
        // Read the plugin source
        let code = crate::runtime::read_plugin_source(source)?;

        // Load and execute the plugin
        let chunk = self.lua.load(&code).set_name(id);

        let module: Table = chunk.eval().map_err(|e| PluginError::LoadError {
            name: id.to_string(),
            message: e.to_string(),
        })?;

        // Detect which hooks are implemented, using the shared canonical list
        // so Lua plugins can register every hook the host dispatches.
        let mut hooks = vec![];
        for hook_name in HOOK_NAMES {
            if module.contains_key(*hook_name).unwrap_or(false) {
                hooks.push(hook_name.to_string());
            }
        }

        // Store in registry
        let key = self
            .lua
            .create_registry_value(module)
            .map_err(|e| PluginError::LoadError {
                name: id.to_string(),
                message: e.to_string(),
            })?;

        let handle = PluginHandle::new(self.next_handle);
        self.next_handle += 1;

        // Create default metadata (would normally come from plugin.toml)
        let metadata = PluginMetadata {
            name: id.to_string(),
            runtime: "lua".to_string(),
            ..Default::default()
        };

        self.plugins.insert(
            handle,
            LoadedLuaPlugin {
                name: id.to_string(),
                module: key,
                metadata,
                hooks,
            },
        );

        Ok(handle)
    }

    fn unload_plugin(&mut self, handle: PluginHandle) -> PluginResult<()> {
        if let Some(plugin) = self.plugins.remove(&handle) {
            self.lua.remove_registry_value(plugin.module).ok();
        }
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

        let module: Table =
            self.lua
                .registry_value(&plugin.module)
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })?;

        let hook_name = hook.name();
        let func: Function = match module.get(hook_name) {
            Ok(f) => f,
            Err(_) => return Ok(HookResult::default()),
        };

        // Convert hook and context to Lua
        let hook_table =
            self.hook_to_lua(&self.lua, hook)
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })?;

        // Call the function under the CPU/time watchdog.
        let result: LuaValue = self.with_deadline(|| {
            func.call((module.clone(), hook_table))
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })
        })?;

        // Convert result
        let mut hook_result = HookResult::ok();
        if let LuaValue::Table(t) = result {
            if let Ok(prevent) = t.get::<bool>("prevent_default")
                && prevent
            {
                hook_result = hook_result.prevent_default();
            }
            if let Ok(stop) = t.get::<bool>("stop_propagation")
                && stop
            {
                hook_result = hook_result.stop_propagation();
            }
            if let Ok(val) = t.get::<LuaValue>("value") {
                hook_result.value = Some(lua_to_value(val));
            }
        }

        Ok(hook_result)
    }

    fn call_hook_async<'a>(
        &'a self,
        handle: PluginHandle,
        hook: &'a Hook,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, PluginResult<HookResult>> {
        // The main Lua state is shared with synchronous render hooks, so calls
        // remain serialized. `with_deadline` enforces the VM instruction/time
        // budget even though this future resolves synchronously.
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

            let module: Table = self.lua.registry_value(&plugin.module).map_err(|e| {
                PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                }
            })?;

            let func: Function = module
                .get(method)
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: format!("Method '{}' not found: {}", method, e),
                })?;

            // Convert args to Lua
            let lua_args: Vec<LuaValue> = args
                .iter()
                .map(|v| value_to_lua(&self.lua, v))
                .collect::<Result<_, _>>()
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })?;

            let result: LuaValue = self.with_deadline(|| {
                func.call(MultiValue::from_vec(
                    std::iter::once(LuaValue::Table(module))
                        .chain(lua_args)
                        .collect(),
                ))
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })
            })?;

            Ok(lua_to_value(result))
        })
    }

    fn create_isolated_context(
        &self,
        sandbox: &SandboxConfig,
    ) -> PluginResult<Box<dyn IsolatedContext>> {
        Ok(Box::new(LuaIsolatedContext::new(sandbox.clone())?))
    }

    fn loaded_plugins(&self) -> Vec<PluginHandle> {
        self.plugins.keys().copied().collect()
    }

    fn shutdown(&mut self) -> PluginResult<()> {
        self.plugins.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{Hook, HookContext};
    use std::io::Write as _;

    fn lua_runtime(timeout_ms: u64) -> LuaRuntime {
        let mut rt = LuaRuntime::new().unwrap();
        let cfg = PluginConfig {
            default_timeout_ms: timeout_ms,
            ..PluginConfig::default()
        };
        rt.init(&cfg).unwrap();
        rt
    }

    #[test]
    fn deadline_aborts_infinite_loop_hook() {
        let mut rt = lua_runtime(50);

        let mut file = tempfile::NamedTempFile::new().unwrap();
        // on_action is a sync hook; the body loops forever and must be aborted
        // by the instruction-count deadline watchdog rather than hanging.
        write!(
            file,
            "local M = {{}}\nfunction M.on_action(self, hook) while true do end end\nreturn M\n"
        )
        .unwrap();

        let handle = rt.load_plugin("loopy", file.path()).unwrap();
        assert!(rt.has_hook(handle, "on_action"));

        let start = std::time::Instant::now();
        let res = rt.call_hook_sync(
            handle,
            &Hook::OnAction { action: "x".into() },
            &HookContext::new(),
        );

        assert!(res.is_err(), "runaway hook should abort, got {res:?}");
        assert!(
            start.elapsed().as_secs() < 5,
            "watchdog should abort promptly, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn normal_hook_runs_under_deadline() {
        let mut rt = lua_runtime(1000);

        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            "local M = {{}}\nfunction M.on_action(self, hook) return {{ handled = true }} end\nreturn M\n"
        )
        .unwrap();

        let handle = rt.load_plugin("calm", file.path()).unwrap();
        let res = rt
            .call_hook_sync(
                handle,
                &Hook::OnAction { action: "x".into() },
                &HookContext::new(),
            )
            .unwrap();
        assert!(res.is_ok());
    }
}
