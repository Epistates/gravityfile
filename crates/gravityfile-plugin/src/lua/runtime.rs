//! Lua runtime implementation.

use std::collections::HashMap;
use std::path::Path;

use mlua::{Function, Lua, MultiValue, Table, Value as LuaValue};

use crate::config::{PluginConfig, PluginMetadata};
use crate::hooks::{Hook, HookContext, HookResult};
use crate::runtime::{BoxFuture, IsolatedContext, PluginHandle, PluginRuntime};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult, Value};

use super::bindings;
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

    /// Whether the runtime has been initialized.
    initialized: bool,
}

impl LuaRuntime {
    /// Create a new Lua runtime.
    pub fn new() -> PluginResult<Self> {
        let lua = Lua::new();

        // Disable potentially dangerous standard library functions
        lua.globals()
            .set("loadfile", LuaValue::Nil)
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;

        lua.globals()
            .set("dofile", LuaValue::Nil)
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;

        Ok(Self {
            lua,
            plugins: HashMap::new(),
            next_handle: 0,
            config: None,
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

        globals
            .set("gf", gf)
            .map_err(|e| PluginError::LoadError {
                name: "lua".into(),
                message: e.to_string(),
            })?;

        // Create the 'fs' namespace (filesystem API)
        let fs = bindings::create_fs_api(&self.lua)?;
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

    /// Convert a Lua value to our Value type.
    fn lua_to_value(lua_val: LuaValue) -> Value {
        match lua_val {
            LuaValue::Nil => Value::Null,
            LuaValue::Boolean(b) => Value::Bool(b),
            LuaValue::Integer(i) => Value::Integer(i),
            LuaValue::Number(n) => Value::Float(n),
            LuaValue::String(s) => Value::String(s.to_string_lossy()),
            LuaValue::Table(t) => {
                // Check if it's an array or object
                let mut is_array = true;
                let mut max_index = 0i64;

                for pair in t.clone().pairs::<i64, LuaValue>() {
                    if let Ok((k, _)) = pair {
                        if k > 0 {
                            max_index = max_index.max(k);
                        } else {
                            is_array = false;
                            break;
                        }
                    } else {
                        is_array = false;
                        break;
                    }
                }

                if is_array && max_index > 0 {
                    let mut arr = Vec::new();
                    for i in 1..=max_index {
                        if let Ok(v) = t.get::<LuaValue>(i) {
                            arr.push(Self::lua_to_value(v));
                        }
                    }
                    Value::Array(arr)
                } else {
                    let mut obj = std::collections::HashMap::new();
                    for pair in t.pairs::<String, LuaValue>() {
                        if let Ok((k, v)) = pair {
                            obj.insert(k, Self::lua_to_value(v));
                        }
                    }
                    Value::Object(obj)
                }
            }
            _ => Value::Null,
        }
    }

    /// Convert our Value type to a Lua value.
    fn value_to_lua(&self, lua: &Lua, val: &Value) -> mlua::Result<LuaValue> {
        match val {
            Value::Null => Ok(LuaValue::Nil),
            Value::Bool(b) => Ok(LuaValue::Boolean(*b)),
            Value::Integer(i) => Ok(LuaValue::Integer(*i)),
            Value::Float(f) => Ok(LuaValue::Number(*f)),
            Value::String(s) => Ok(LuaValue::String(lua.create_string(s)?)),
            Value::Array(arr) => {
                let table = lua.create_table()?;
                for (i, v) in arr.iter().enumerate() {
                    table.set(i + 1, self.value_to_lua(lua, v)?)?;
                }
                Ok(LuaValue::Table(table))
            }
            Value::Object(obj) => {
                let table = lua.create_table()?;
                for (k, v) in obj {
                    table.set(k.as_str(), self.value_to_lua(lua, v)?)?;
                }
                Ok(LuaValue::Table(table))
            }
            Value::Bytes(b) => Ok(LuaValue::String(lua.create_string(b)?)),
        }
    }

    /// Convert a Hook to a Lua table.
    fn hook_to_lua(&self, lua: &Lua, hook: &Hook) -> mlua::Result<Table> {
        let table = lua.create_table()?;

        // Serialize hook to JSON then to Lua table
        let json = serde_json::to_string(hook).map_err(|e| mlua::Error::external(e))?;
        let json_val: serde_json::Value =
            serde_json::from_str(&json).map_err(|e| mlua::Error::external(e))?;

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
}

impl Default for LuaRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create Lua runtime")
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

        self.config = Some(config.clone());
        self.init_api()?;
        self.initialized = true;

        Ok(())
    }

    fn load_plugin(&mut self, id: &str, source: &Path) -> PluginResult<PluginHandle> {
        // Read the plugin source
        let code = std::fs::read_to_string(source)?;

        // Load and execute the plugin
        let chunk = self.lua.load(&code).set_name(id);

        let module: Table = chunk.eval().map_err(|e| PluginError::LoadError {
            name: id.to_string(),
            message: e.to_string(),
        })?;

        // Detect which hooks are implemented
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
            if module.contains_key(hook_name).unwrap_or(false) {
                hooks.push(hook_name.to_string());
            }
        }

        // Store in registry
        let key = self.lua.create_registry_value(module).map_err(|e| {
            PluginError::LoadError {
                name: id.to_string(),
                message: e.to_string(),
            }
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
        let plugin = self.plugins.get(&handle).ok_or_else(|| PluginError::NotFound {
            path: std::path::PathBuf::new(),
        })?;

        let module: Table = self.lua.registry_value(&plugin.module).map_err(|e| {
            PluginError::ExecutionError {
                name: plugin.name.clone(),
                message: e.to_string(),
            }
        })?;

        let hook_name = hook.name();
        let func: Function = match module.get(hook_name) {
            Ok(f) => f,
            Err(_) => return Ok(HookResult::default()),
        };

        // Convert hook and context to Lua
        let hook_table = self.hook_to_lua(&self.lua, hook).map_err(|e| {
            PluginError::ExecutionError {
                name: plugin.name.clone(),
                message: e.to_string(),
            }
        })?;

        // Call the function
        let result: LuaValue = func.call((module.clone(), hook_table)).map_err(|e| {
            PluginError::ExecutionError {
                name: plugin.name.clone(),
                message: e.to_string(),
            }
        })?;

        // Convert result
        let mut hook_result = HookResult::ok();
        if let LuaValue::Table(t) = result {
            if let Ok(prevent) = t.get::<bool>("prevent_default") {
                if prevent {
                    hook_result = hook_result.prevent_default();
                }
            }
            if let Ok(stop) = t.get::<bool>("stop_propagation") {
                if stop {
                    hook_result = hook_result.stop_propagation();
                }
            }
            if let Ok(val) = t.get::<LuaValue>("value") {
                hook_result.value = Some(Self::lua_to_value(val));
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
        // For now, just call sync version
        // TODO: Implement true async with spawn_blocking
        Box::pin(async move { self.call_hook_sync(handle, hook, ctx) })
    }

    fn call_method<'a>(
        &'a self,
        handle: PluginHandle,
        method: &'a str,
        args: Vec<Value>,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            let plugin = self.plugins.get(&handle).ok_or_else(|| PluginError::NotFound {
                path: std::path::PathBuf::new(),
            })?;

            let module: Table = self.lua.registry_value(&plugin.module).map_err(|e| {
                PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                }
            })?;

            let func: Function = module.get(method).map_err(|e| PluginError::ExecutionError {
                name: plugin.name.clone(),
                message: format!("Method '{}' not found: {}", method, e),
            })?;

            // Convert args to Lua
            let lua_args: Vec<LuaValue> = args
                .iter()
                .map(|v| self.value_to_lua(&self.lua, v))
                .collect::<Result<_, _>>()
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })?;

            let result: LuaValue = func
                .call(MultiValue::from_vec(
                    std::iter::once(LuaValue::Table(module))
                        .chain(lua_args)
                        .collect(),
                ))
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })?;

            Ok(Self::lua_to_value(result))
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
