//! Isolated Lua context for async plugin execution.
//!
//! This provides a lightweight, isolated Lua environment for running
//! async plugins (previewers, analyzers) safely in background tasks.

use mlua::{Lua, Value as LuaValue};
use tokio_util::sync::CancellationToken;

use crate::runtime::{BoxFuture, IsolatedContext};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult, Value};

use super::bindings;

/// An isolated Lua context with limited API access.
pub struct LuaIsolatedContext {
    lua: Lua,
    sandbox: SandboxConfig,
}

impl LuaIsolatedContext {
    /// Create a new isolated Lua context.
    pub fn new(sandbox: SandboxConfig) -> PluginResult<Self> {
        let lua = Lua::new();

        // Set memory limit if configured
        if sandbox.max_memory > 0 {
            let _ = lua.set_memory_limit(sandbox.max_memory);
        }

        // Initialize minimal API
        let globals = lua.globals();

        // Remove potentially dangerous functions
        globals.set("loadfile", LuaValue::Nil).ok();
        globals.set("dofile", LuaValue::Nil).ok();
        globals.set("load", LuaValue::Nil).ok();
        globals.set("os", LuaValue::Nil).ok();
        globals.set("io", LuaValue::Nil).ok();
        globals.set("debug", LuaValue::Nil).ok();

        // Add basic 'gf' namespace
        let gf = lua.create_table().map_err(|e| PluginError::LoadError {
            name: "isolate".into(),
            message: e.to_string(),
        })?;

        gf.set("version", env!("CARGO_PKG_VERSION")).ok();

        // Add logging (safe)
        let log_info = lua
            .create_function(|_, msg: String| {
                tracing::info!(target: "plugin_isolate", "{}", msg);
                Ok(())
            })
            .map_err(|e| PluginError::LoadError {
                name: "isolate".into(),
                message: e.to_string(),
            })?;
        gf.set("log_info", log_info).ok();

        globals.set("gf", gf).ok();

        // Add 'fs' if read permission is granted
        if sandbox.has_permission(crate::sandbox::Permission::Read) {
            let fs = bindings::create_fs_api(&lua)?;
            globals.set("fs", fs).ok();
        }

        // Add 'ui' for rendering (safe, no side effects)
        let ui = bindings::create_ui_api(&lua)?;
        globals.set("ui", ui).ok();

        Ok(Self { lua, sandbox })
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
                let mut obj = std::collections::HashMap::new();
                for pair in t.pairs::<String, LuaValue>() {
                    if let Ok((k, v)) = pair {
                        obj.insert(k, Self::lua_to_value(v));
                    }
                }
                Value::Object(obj)
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
}

impl IsolatedContext for LuaIsolatedContext {
    fn execute<'a>(
        &'a self,
        code: &'a [u8],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            // Check for cancellation
            if cancel.is_cancelled() {
                return Err(PluginError::Cancelled {
                    name: "isolate".into(),
                });
            }

            let code_str =
                std::str::from_utf8(code).map_err(|e| PluginError::ExecutionError {
                    name: "isolate".into(),
                    message: format!("Invalid UTF-8 in code: {}", e),
                })?;

            // Set up instruction hook for cancellation and timeout
            let cancel_clone = cancel.clone();
            let instruction_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let max_instructions = self.sandbox.timeout_ms * 10000; // Rough estimate

            let ic = instruction_count.clone();
            self.lua.set_hook(
                mlua::HookTriggers::new().every_nth_instruction(1000),
                move |_lua, _debug| {
                    let count = ic.fetch_add(1000, std::sync::atomic::Ordering::Relaxed);
                    if cancel_clone.is_cancelled() {
                        return Err(mlua::Error::external("Cancelled"));
                    }
                    if count > max_instructions {
                        return Err(mlua::Error::external("Timeout"));
                    }
                    Ok(mlua::VmState::Continue)
                },
            );

            // Execute the code
            let result = self
                .lua
                .load(code_str)
                .eval::<LuaValue>()
                .map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("Cancelled") {
                        PluginError::Cancelled {
                            name: "isolate".into(),
                        }
                    } else if msg.contains("Timeout") {
                        PluginError::Timeout {
                            name: "isolate".into(),
                            timeout_ms: self.sandbox.timeout_ms,
                        }
                    } else {
                        PluginError::ExecutionError {
                            name: "isolate".into(),
                            message: msg,
                        }
                    }
                })?;

            // Remove hook
            self.lua.remove_hook();

            Ok(Self::lua_to_value(result))
        })
    }

    fn call_function<'a>(
        &'a self,
        name: &'a str,
        args: Vec<Value>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(PluginError::Cancelled {
                    name: "isolate".into(),
                });
            }

            let globals = self.lua.globals();
            let func: mlua::Function = globals.get(name).map_err(|e| PluginError::ExecutionError {
                name: "isolate".into(),
                message: format!("Function '{}' not found: {}", name, e),
            })?;

            // Convert args
            let lua_args: Vec<LuaValue> = args
                .iter()
                .map(|v| self.value_to_lua(&self.lua, v))
                .collect::<Result<_, _>>()
                .map_err(|e| PluginError::ExecutionError {
                    name: "isolate".into(),
                    message: e.to_string(),
                })?;

            let result: LuaValue = func
                .call(mlua::MultiValue::from_vec(lua_args))
                .map_err(|e| PluginError::ExecutionError {
                    name: "isolate".into(),
                    message: e.to_string(),
                })?;

            Ok(Self::lua_to_value(result))
        })
    }

    fn set_global(&mut self, name: &str, value: Value) -> PluginResult<()> {
        let lua_val = self.value_to_lua(&self.lua, &value).map_err(|e| {
            PluginError::ExecutionError {
                name: "isolate".into(),
                message: e.to_string(),
            }
        })?;

        self.lua
            .globals()
            .set(name, lua_val)
            .map_err(|e| PluginError::ExecutionError {
                name: "isolate".into(),
                message: e.to_string(),
            })?;

        Ok(())
    }

    fn get_global(&self, name: &str) -> PluginResult<Value> {
        let val: LuaValue = self.lua.globals().get(name).map_err(|e| {
            PluginError::ExecutionError {
                name: "isolate".into(),
                message: e.to_string(),
            }
        })?;

        Ok(Self::lua_to_value(val))
    }
}
