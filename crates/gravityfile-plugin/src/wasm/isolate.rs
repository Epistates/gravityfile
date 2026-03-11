//! Isolated WASM context for async plugin execution.

use std::time::Duration;

use extism::{Manifest, Plugin, Wasm};
use tokio_util::sync::CancellationToken;

use crate::runtime::{BoxFuture, IsolatedContext};
use crate::sandbox::{Permission, SandboxConfig};
use crate::types::{PluginError, PluginResult, Value};

/// An isolated WASM context with limited API access.
pub struct WasmIsolatedContext {
    sandbox: SandboxConfig,
}

impl WasmIsolatedContext {
    /// Create a new isolated WASM context.
    pub fn new(sandbox: SandboxConfig) -> PluginResult<Self> {
        Ok(Self { sandbox })
    }

    /// Build a sandboxed Extism plugin from raw WASM bytes.
    fn build_plugin(&self, code: &[u8]) -> Result<Plugin, String> {
        let wasm = Wasm::data(code);
        let sandbox = &self.sandbox;

        // Only enable WASI when the sandbox explicitly permits syscall-level access.
        let allow_wasi = sandbox.has_permission(Permission::Execute)
            || sandbox.has_permission(Permission::Network);

        let mut manifest = Manifest::new([wasm]);

        if sandbox.max_memory > 0 {
            let pages = (sandbox.max_memory / (64 * 1024)).max(1) as u32;
            manifest = manifest.with_memory_max(pages);
        }

        if sandbox.timeout_ms > 0 {
            manifest = manifest.with_timeout(Duration::from_millis(sandbox.timeout_ms));
        }

        for path in &sandbox.allowed_read_paths {
            if let Some(s) = path.to_str() {
                manifest = manifest.with_allowed_path(s.to_string(), path);
            }
        }

        for path in &sandbox.allowed_write_paths {
            if let Some(s) = path.to_str()
                && !sandbox.allowed_read_paths.contains(path)
            {
                manifest = manifest.with_allowed_path(s.to_string(), path);
            }
        }

        Plugin::new(&manifest, [], allow_wasi).map_err(|e| e.to_string())
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

            // Build a sandboxed Extism plugin from the provided WASM binary.
            let mut plugin = self
                .build_plugin(code)
                .map_err(|e| PluginError::ExecutionError {
                    name: "wasm_isolate".into(),
                    message: e,
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
