//! Isolated WASM context for async plugin execution.

use extism::{Manifest, Plugin};
use tokio_util::sync::CancellationToken;

use crate::runtime::{BoxFuture, IsolatedContext};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult, Value};

/// An isolated WASM context with limited API access.
pub struct WasmIsolatedContext {
    #[allow(dead_code)]
    sandbox: SandboxConfig,
}

impl WasmIsolatedContext {
    /// Create a new isolated WASM context.
    pub fn new(sandbox: SandboxConfig) -> PluginResult<Self> {
        Ok(Self { sandbox })
    }
}

impl IsolatedContext for WasmIsolatedContext {
    fn execute<'a>(
        &'a self,
        code: &'a [u8],
        cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            // Check for cancellation
            if cancel.is_cancelled() {
                return Err(PluginError::Cancelled {
                    name: "wasm_isolate".into(),
                });
            }

            // In Extism, "code" would be the WASM binary.
            let manifest = Manifest::new([extism::Wasm::data(code)]);
            let mut plugin =
                Plugin::new(&manifest, [], true).map_err(|e| PluginError::ExecutionError {
                    name: "wasm_isolate".into(),
                    message: e.to_string(),
                })?;

            // Extism allows running a "main" or default function
            // We assume it's exported as "run"
            let res = plugin.call::<&[u8], &[u8]>("run", &[]).map_err(|e| {
                PluginError::ExecutionError {
                    name: "wasm_isolate".into(),
                    message: e.to_string(),
                }
            })?;

            if res.is_empty() {
                return Ok(Value::Null);
            }

            let val: Value =
                serde_json::from_slice(res).map_err(|e| PluginError::ExecutionError {
                    name: "wasm_isolate".into(),
                    message: format!("Failed to parse WASM output: {}", e),
                })?;

            Ok(val)
        })
    }

    fn call_function<'a>(
        &'a self,
        _name: &'a str,
        _args: Vec<Value>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            Err(PluginError::ExecutionError {
                name: "wasm_isolate".into(),
                message: "call_function not supported directly on uninitialized WASM context"
                    .to_string(),
            })
        })
    }

    fn set_global(&mut self, _name: &str, _value: Value) -> PluginResult<()> {
        Ok(()) // Not applicable for Extism WASM without memory sharing
    }

    fn get_global(&self, _name: &str) -> PluginResult<Value> {
        Ok(Value::Null)
    }
}
