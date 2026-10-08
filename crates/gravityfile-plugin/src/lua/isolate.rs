//! Isolated Lua context for async plugin execution.
//!
//! This provides a lightweight, isolated Lua environment for running
//! async plugins (previewers, analyzers) safely in background tasks.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mlua::{Lua, LuaOptions, StdLib, Value as LuaValue};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::runtime::{BoxFuture, IsolatedContext};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult, Value};

use super::bindings;
use super::convert::{lua_to_value, value_to_lua};

fn execution_error(error: mlua::Error, timeout_ms: u64) -> PluginError {
    let message = error.to_string();
    if message.contains("plugin execution cancelled") {
        PluginError::Cancelled {
            name: "isolate".into(),
        }
    } else if message.contains("plugin execution timed out") {
        PluginError::Timeout {
            name: "isolate".into(),
            timeout_ms,
        }
    } else {
        PluginError::ExecutionError {
            name: "isolate".into(),
            message,
        }
    }
}

/// Standard libraries a sandboxed plugin may use.
///
/// This is a whitelist, not a blocklist: `io`, `os`, `package` and `debug` are
/// never loaded into the VM at all. Nil-ing those globals after the fact left
/// them reachable through any reference already captured (notably
/// `package.loaded`).
fn sandboxed_stdlib() -> StdLib {
    StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE
}

/// Base-library functions that load or execute new code. The base library is
/// always present, so these are removed explicitly.
const DENIED_BASE_GLOBALS: &[&str] = &[
    "load",
    "loadstring",
    "loadfile",
    "dofile",
    "require",
    "collectgarbage",
];

/// An isolated Lua context with limited API access.
pub struct LuaIsolatedContext {
    lua: Arc<Lua>,
    sandbox: SandboxConfig,
    /// Serializes VM access. The debug hook carrying the deadline is global to
    /// the Lua state, so concurrent executions would clobber each other.
    vm_lock: Arc<Mutex<()>>,
}

impl LuaIsolatedContext {
    /// Create a new isolated Lua context.
    pub fn new(sandbox: SandboxConfig) -> PluginResult<Self> {
        let lua = Lua::new_with(sandboxed_stdlib(), LuaOptions::default()).map_err(|e| {
            PluginError::LoadError {
                name: "isolate".into(),
                message: format!("Failed to create sandboxed Lua state: {e}"),
            }
        })?;

        // Set memory limit if configured
        if sandbox.max_memory > 0 {
            let _ = lua.set_memory_limit(sandbox.max_memory);
        }

        let globals = lua.globals();

        for name in DENIED_BASE_GLOBALS {
            globals.set(*name, LuaValue::Nil).ok();
        }

        // `print` writes straight to the process stdout, which during TUI
        // operation is the alternate screen. Route it to the log so a plugin
        // cannot corrupt the display.
        let print = lua
            .create_function(|_, args: mlua::Variadic<LuaValue>| {
                let line = args
                    .iter()
                    .map(|value| match value {
                        LuaValue::String(text) => text.to_string_lossy().to_string(),
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("\t");
                tracing::info!(target: "plugin_isolate", "{}", line);
                Ok(())
            })
            .map_err(|e| PluginError::LoadError {
                name: "isolate".into(),
                message: e.to_string(),
            })?;
        globals.set("print", print).ok();

        // Disable string.dump to prevent bytecode extraction
        if let Ok(string_table) = globals.get::<mlua::Table>("string") {
            string_table.set("dump", LuaValue::Nil).ok();
        }

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
            let fs = bindings::create_fs_api(&lua, Some(sandbox.clone()))?;
            globals.set("fs", fs).ok();
        }

        // Add 'ui' for rendering (safe, no side effects)
        let ui = bindings::create_ui_api(&lua)?;
        globals.set("ui", ui).ok();

        drop(globals);

        Ok(Self {
            lua: Arc::new(lua),
            sandbox,
            vm_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Install the deadline/cancellation hook for one VM call.
    ///
    /// The hook is the only thing that can interrupt running Lua bytecode. The
    /// wall-clock timeout around the blocking task bounds host calls (file
    /// reads, say) that the hook cannot reach into.
    fn install_guard_hook(lua: &Lua, cancel: &CancellationToken, timeout_ms: u64) {
        let cancel = cancel.clone();
        let deadline = (timeout_ms > 0)
            .then(|| Instant::now().checked_add(Duration::from_millis(timeout_ms)))
            .flatten();
        let _ = lua.set_hook(
            mlua::HookTriggers::new().every_nth_instruction(1000),
            move |_lua, _debug| {
                if cancel.is_cancelled() {
                    return Err(mlua::Error::external("plugin execution cancelled"));
                }
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(mlua::Error::external("plugin execution timed out"));
                }
                Ok(mlua::VmState::Continue)
            },
        );
    }

    /// Run `job` on a blocking thread under the context's VM lock, bounded by
    /// the sandbox's wall-clock timeout.
    ///
    /// Running on a blocking thread is what makes the timeout real: the future
    /// it wraps now has a genuine await point, so `tokio::time::timeout` can
    /// actually fire. Previously the whole VM call ran inline inside the
    /// timeout's own future, which meant it could never be preempted.
    async fn run_guarded<T, F>(&self, cancel: CancellationToken, job: F) -> PluginResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Lua) -> PluginResult<T> + Send + 'static,
    {
        if cancel.is_cancelled() {
            return Err(PluginError::Cancelled {
                name: "isolate".into(),
            });
        }

        let timeout_ms = self.sandbox.timeout_ms;
        let lua = Arc::clone(&self.lua);
        let guard = Arc::clone(&self.vm_lock).lock_owned().await;

        let handle = tokio::task::spawn_blocking(move || {
            // Held for the whole VM call so no other execution can install a
            // competing hook on this state.
            let _guard = guard;
            Self::install_guard_hook(&lua, &cancel, timeout_ms);
            let result = job(&lua);
            lua.remove_hook();
            result
        });

        let joined = if timeout_ms == 0 {
            handle.await
        } else {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), handle).await {
                Ok(joined) => joined,
                Err(_elapsed) => {
                    return Err(PluginError::Timeout {
                        name: "isolate".into(),
                        timeout_ms,
                    });
                }
            }
        };

        joined.map_err(|error| PluginError::ExecutionError {
            name: "isolate".into(),
            message: format!("plugin task failed: {error}"),
        })?
    }
}

impl IsolatedContext for LuaIsolatedContext {
    fn execute<'a>(
        &'a self,
        code: &'a [u8],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            let timeout_ms = self.sandbox.timeout_ms;
            let code = std::str::from_utf8(code)
                .map_err(|e| PluginError::ExecutionError {
                    name: "isolate".into(),
                    message: format!("Invalid UTF-8 in code: {e}"),
                })?
                .to_owned();

            self.run_guarded(cancel, move |lua| {
                lua.load(&code)
                    .eval::<LuaValue>()
                    .map(lua_to_value)
                    .map_err(|error| execution_error(error, timeout_ms))
            })
            .await
        })
    }

    fn call_function<'a>(
        &'a self,
        name: &'a str,
        args: Vec<Value>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            let timeout_ms = self.sandbox.timeout_ms;
            let name = name.to_owned();

            self.run_guarded(cancel, move |lua| {
                let func: mlua::Function =
                    lua.globals()
                        .get(&*name)
                        .map_err(|e| PluginError::ExecutionError {
                            name: "isolate".into(),
                            message: format!("Function '{name}' not found: {e}"),
                        })?;

                let lua_args: Vec<LuaValue> = args
                    .iter()
                    .map(|value| value_to_lua(lua, value))
                    .collect::<Result<_, _>>()
                    .map_err(|e| PluginError::ExecutionError {
                        name: "isolate".into(),
                        message: e.to_string(),
                    })?;

                func.call(mlua::MultiValue::from_vec(lua_args))
                    .map(lua_to_value)
                    .map_err(|error| execution_error(error, timeout_ms))
            })
            .await
        })
    }

    fn set_global(&mut self, name: &str, value: Value) -> PluginResult<()> {
        let lua_val = value_to_lua(&self.lua, &value).map_err(|e| PluginError::ExecutionError {
            name: "isolate".into(),
            message: e.to_string(),
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
        let val: LuaValue =
            self.lua
                .globals()
                .get(name)
                .map_err(|e| PluginError::ExecutionError {
                    name: "isolate".into(),
                    message: e.to_string(),
                })?;

        Ok(lua_to_value(val))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(timeout_ms: u64) -> LuaIsolatedContext {
        let mut sandbox = SandboxConfig::minimal();
        sandbox.timeout_ms = timeout_ms;
        LuaIsolatedContext::new(sandbox).unwrap()
    }

    #[tokio::test]
    async fn execute_returns_converted_values() {
        let context = context(1_000);
        let value = context
            .execute(b"return {1, 2, 3}", CancellationToken::new())
            .await
            .unwrap();

        assert!(matches!(value, Value::Array(values) if values.len() == 3));
    }

    #[tokio::test]
    async fn execute_honors_precancellation() {
        let context = context(1_000);
        let token = CancellationToken::new();
        token.cancel();

        let error = context.execute(b"return 1", token).await.unwrap_err();
        assert!(matches!(error, PluginError::Cancelled { .. }));
    }

    #[tokio::test]
    async fn call_function_aborts_runaway_code() {
        let context = context(50);
        context
            .execute(
                b"function runaway() while true do end end",
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let error = context
            .call_function("runaway", vec![], CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, PluginError::Timeout { .. }));
    }

    #[tokio::test]
    async fn zero_timeout_allows_finite_execution() {
        let context = context(0);
        let value = context
            .execute(b"return 40 + 2", CancellationToken::new())
            .await
            .unwrap();

        assert!(matches!(value, Value::Integer(42)));
    }
}
