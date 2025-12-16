//! Rhai runtime implementation.

use std::collections::HashMap;
use std::path::Path;

use rhai::{Dynamic, Engine, Scope, AST};

use crate::config::{PluginConfig, PluginMetadata};
use crate::hooks::{Hook, HookContext, HookResult};
use crate::runtime::{BoxFuture, IsolatedContext, PluginHandle, PluginRuntime};
use crate::sandbox::SandboxConfig;
use crate::types::{PluginError, PluginResult, Value};

/// A loaded Rhai plugin.
struct LoadedRhaiPlugin {
    /// Plugin name/id.
    name: String,

    /// Compiled AST.
    ast: AST,

    /// Plugin metadata.
    metadata: PluginMetadata,

    /// Hooks implemented by this plugin.
    hooks: Vec<String>,
}

/// Rhai plugin runtime.
pub struct RhaiRuntime {
    /// The Rhai engine.
    engine: Engine,

    /// Loaded plugins by handle.
    plugins: HashMap<PluginHandle, LoadedRhaiPlugin>,

    /// Next plugin handle ID.
    next_handle: usize,

    /// Runtime configuration.
    config: Option<PluginConfig>,

    /// Whether the runtime has been initialized.
    initialized: bool,
}

impl RhaiRuntime {
    /// Create a new Rhai runtime.
    pub fn new() -> PluginResult<Self> {
        let mut engine = Engine::new();

        // Configure safety limits
        engine.set_max_expr_depths(64, 64);
        engine.set_max_call_levels(64);
        engine.set_max_operations(1_000_000);
        engine.set_max_modules(100);
        engine.set_max_string_size(1024 * 1024); // 1MB strings
        engine.set_max_array_size(10_000);
        engine.set_max_map_size(10_000);

        Ok(Self {
            engine,
            plugins: HashMap::new(),
            next_handle: 0,
            config: None,
            initialized: false,
        })
    }

    /// Initialize the Rhai engine with the gravityfile API.
    fn init_api(&mut self) -> PluginResult<()> {
        // Register logging functions
        self.engine.register_fn("log_info", |msg: &str| {
            tracing::info!(target: "plugin", "{}", msg);
        });

        self.engine.register_fn("log_warn", |msg: &str| {
            tracing::warn!(target: "plugin", "{}", msg);
        });

        self.engine.register_fn("log_error", |msg: &str| {
            tracing::error!(target: "plugin", "{}", msg);
        });

        self.engine.register_fn("notify", |msg: &str| {
            tracing::info!(target: "plugin_notify", "{}", msg);
        });

        // Register filesystem functions
        self.engine
            .register_fn("fs_exists", |path: &str| -> bool {
                std::path::Path::new(path).exists()
            });

        self.engine
            .register_fn("fs_is_dir", |path: &str| -> bool {
                std::path::Path::new(path).is_dir()
            });

        self.engine
            .register_fn("fs_is_file", |path: &str| -> bool {
                std::path::Path::new(path).is_file()
            });

        self.engine
            .register_fn("fs_read", |path: &str| -> Dynamic {
                match std::fs::read_to_string(path) {
                    Ok(content) => Dynamic::from(content),
                    Err(_) => Dynamic::UNIT,
                }
            });

        self.engine
            .register_fn("fs_extension", |path: &str| -> Dynamic {
                let p = std::path::Path::new(path);
                match p.extension().and_then(|e| e.to_str()) {
                    Some(ext) => Dynamic::from(ext.to_string()),
                    None => Dynamic::UNIT,
                }
            });

        self.engine
            .register_fn("fs_filename", |path: &str| -> Dynamic {
                let p = std::path::Path::new(path);
                match p.file_name().and_then(|n| n.to_str()) {
                    Some(name) => Dynamic::from(name.to_string()),
                    None => Dynamic::UNIT,
                }
            });

        self.engine
            .register_fn("fs_parent", |path: &str| -> Dynamic {
                let p = std::path::Path::new(path);
                match p.parent().and_then(|p| p.to_str()) {
                    Some(parent) => Dynamic::from(parent.to_string()),
                    None => Dynamic::UNIT,
                }
            });

        self.engine
            .register_fn("fs_size", |path: &str| -> Dynamic {
                match std::fs::metadata(path) {
                    Ok(meta) => Dynamic::from(meta.len() as i64),
                    Err(_) => Dynamic::from(-1_i64),
                }
            });

        // Register UI helper functions
        self.engine.register_fn(
            "ui_span",
            |text: &str, fg: &str| -> rhai::Map {
                let mut map = rhai::Map::new();
                map.insert("type".into(), Dynamic::from("span"));
                map.insert("text".into(), Dynamic::from(text.to_string()));
                map.insert("fg".into(), Dynamic::from(fg.to_string()));
                map
            },
        );

        self.engine.register_fn("ui_line", |spans: rhai::Array| -> rhai::Map {
            let mut map = rhai::Map::new();
            map.insert("type".into(), Dynamic::from("line"));
            map.insert("spans".into(), Dynamic::from(spans));
            map
        });

        Ok(())
    }

    /// Convert a Rhai Dynamic to our Value type.
    fn dynamic_to_value(val: &Dynamic) -> Value {
        if val.is_unit() {
            Value::Null
        } else if val.is_bool() {
            Value::Bool(val.as_bool().unwrap_or(false))
        } else if val.is_int() {
            Value::Integer(val.as_int().unwrap_or(0))
        } else if val.is_float() {
            Value::Float(val.as_float().unwrap_or(0.0))
        } else if val.is_string() {
            Value::String(val.clone().into_string().unwrap_or_default())
        } else if val.is_array() {
            let arr = val.clone().into_array().unwrap_or_default();
            Value::Array(arr.iter().map(Self::dynamic_to_value).collect())
        } else if val.is_map() {
            let map = val.clone().cast::<rhai::Map>();
            let obj: std::collections::HashMap<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k.to_string(), Self::dynamic_to_value(&v)))
                .collect();
            Value::Object(obj)
        } else {
            Value::Null
        }
    }

    /// Convert our Value type to a Rhai Dynamic.
    fn value_to_dynamic(val: &Value) -> Dynamic {
        match val {
            Value::Null => Dynamic::UNIT,
            Value::Bool(b) => Dynamic::from(*b),
            Value::Integer(i) => Dynamic::from(*i),
            Value::Float(f) => Dynamic::from(*f),
            Value::String(s) => Dynamic::from(s.clone()),
            Value::Array(arr) => {
                let rhai_arr: rhai::Array = arr.iter().map(Self::value_to_dynamic).collect();
                Dynamic::from(rhai_arr)
            }
            Value::Object(obj) => {
                let mut map = rhai::Map::new();
                for (k, v) in obj {
                    map.insert(k.clone().into(), Self::value_to_dynamic(v));
                }
                Dynamic::from(map)
            }
            Value::Bytes(b) => Dynamic::from(b.clone()),
        }
    }

    /// Convert a Hook to a Rhai map.
    fn hook_to_dynamic(&self, hook: &Hook) -> Dynamic {
        // Serialize to JSON, then to Rhai map
        let json = serde_json::to_value(hook).unwrap_or(serde_json::Value::Null);

        fn json_to_dynamic(val: &serde_json::Value) -> Dynamic {
            match val {
                serde_json::Value::Null => Dynamic::UNIT,
                serde_json::Value::Bool(b) => Dynamic::from(*b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Dynamic::from(i)
                    } else {
                        Dynamic::from(n.as_f64().unwrap_or(0.0))
                    }
                }
                serde_json::Value::String(s) => Dynamic::from(s.clone()),
                serde_json::Value::Array(arr) => {
                    let rhai_arr: rhai::Array = arr.iter().map(json_to_dynamic).collect();
                    Dynamic::from(rhai_arr)
                }
                serde_json::Value::Object(obj) => {
                    let mut map = rhai::Map::new();
                    for (k, v) in obj {
                        map.insert(k.clone().into(), json_to_dynamic(v));
                    }
                    Dynamic::from(map)
                }
            }
        }

        json_to_dynamic(&json)
    }
}

impl Default for RhaiRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create Rhai runtime")
    }
}

impl PluginRuntime for RhaiRuntime {
    fn name(&self) -> &'static str {
        "rhai"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &[".rhai"]
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
        // Read and compile the plugin
        let code = std::fs::read_to_string(source)?;

        let ast = self.engine.compile(&code).map_err(|e| PluginError::LoadError {
            name: id.to_string(),
            message: e.to_string(),
        })?;

        // Detect hooks by looking for function definitions
        let mut hooks = vec![];
        for func in ast.iter_functions() {
            let name = func.name.to_string();
            if name.starts_with("on_") {
                hooks.push(name);
            }
        }

        let handle = PluginHandle::new(self.next_handle);
        self.next_handle += 1;

        let metadata = PluginMetadata {
            name: id.to_string(),
            runtime: "rhai".to_string(),
            ..Default::default()
        };

        self.plugins.insert(
            handle,
            LoadedRhaiPlugin {
                name: id.to_string(),
                ast,
                metadata,
                hooks,
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
        let plugin = self.plugins.get(&handle).ok_or_else(|| PluginError::NotFound {
            path: std::path::PathBuf::new(),
        })?;

        let hook_name = hook.name();
        if !plugin.hooks.contains(&hook_name.to_string()) {
            return Ok(HookResult::default());
        }

        // Create a scope with the hook data
        let mut scope = Scope::new();
        scope.push("hook", self.hook_to_dynamic(hook));

        // Call the hook function
        let result = self
            .engine
            .call_fn::<Dynamic>(&mut scope, &plugin.ast, hook_name, ())
            .map_err(|e| PluginError::ExecutionError {
                name: plugin.name.clone(),
                message: e.to_string(),
            })?;

        // Convert result
        let mut hook_result = HookResult::ok();
        if result.is_map() {
            if let Some(map) = result.try_cast::<rhai::Map>() {
                if let Some(prevent) = map.get("prevent_default") {
                    if prevent.as_bool().unwrap_or(false) {
                        hook_result = hook_result.prevent_default();
                    }
                }
                if let Some(stop) = map.get("stop_propagation") {
                    if stop.as_bool().unwrap_or(false) {
                        hook_result = hook_result.stop_propagation();
                    }
                }
                if let Some(val) = map.get("value") {
                    hook_result.value = Some(Self::dynamic_to_value(val));
                }
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

            let mut scope = Scope::new();

            // Convert args to Rhai dynamics
            let rhai_args: Vec<Dynamic> = args.iter().map(Self::value_to_dynamic).collect();

            // Rhai doesn't support variable-length args directly, so we pass as array
            scope.push("args", rhai_args);

            let result = self
                .engine
                .call_fn::<Dynamic>(&mut scope, &plugin.ast, method, ())
                .map_err(|e| PluginError::ExecutionError {
                    name: plugin.name.clone(),
                    message: e.to_string(),
                })?;

            Ok(Self::dynamic_to_value(&result))
        })
    }

    fn create_isolated_context(
        &self,
        sandbox: &SandboxConfig,
    ) -> PluginResult<Box<dyn IsolatedContext>> {
        Ok(Box::new(RhaiIsolatedContext::new(sandbox.clone())?))
    }

    fn loaded_plugins(&self) -> Vec<PluginHandle> {
        self.plugins.keys().copied().collect()
    }

    fn shutdown(&mut self) -> PluginResult<()> {
        self.plugins.clear();
        Ok(())
    }
}

/// An isolated Rhai context for async execution.
struct RhaiIsolatedContext {
    engine: Engine,
    #[allow(dead_code)]
    sandbox: SandboxConfig,
}

impl RhaiIsolatedContext {
    fn new(sandbox: SandboxConfig) -> PluginResult<Self> {
        let mut engine = Engine::new();

        // Strict limits for isolated contexts
        engine.set_max_expr_depths(32, 32);
        engine.set_max_call_levels(32);
        engine.set_max_operations(100_000);
        engine.set_max_modules(10);
        engine.set_max_string_size(100 * 1024); // 100KB
        engine.set_max_array_size(1000);
        engine.set_max_map_size(1000);

        // Disable potentially dangerous operations
        engine.disable_symbol("eval");

        Ok(Self { engine, sandbox })
    }
}

impl IsolatedContext for RhaiIsolatedContext {
    fn execute<'a>(
        &'a self,
        code: &'a [u8],
        cancel: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(PluginError::Cancelled {
                    name: "isolate".into(),
                });
            }

            let code_str = std::str::from_utf8(code).map_err(|e| PluginError::ExecutionError {
                name: "isolate".into(),
                message: format!("Invalid UTF-8: {}", e),
            })?;

            let result = self.engine.eval::<Dynamic>(code_str).map_err(|e| {
                PluginError::ExecutionError {
                    name: "isolate".into(),
                    message: e.to_string(),
                }
            })?;

            Ok(RhaiRuntime::dynamic_to_value(&result))
        })
    }

    fn call_function<'a>(
        &'a self,
        name: &'a str,
        args: Vec<Value>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'a, PluginResult<Value>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(PluginError::Cancelled {
                    name: "isolate".into(),
                });
            }

            // Rhai requires AST to call functions, so we construct a call expression
            let args_str: Vec<String> = args
                .iter()
                .map(|v| match v {
                    Value::String(s) => format!("\"{}\"", s.replace("\"", "\\\"")),
                    Value::Integer(i) => i.to_string(),
                    Value::Float(f) => f.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => "()".to_string(),
                })
                .collect();

            let code = format!("{}({})", name, args_str.join(", "));

            let result = self.engine.eval::<Dynamic>(&code).map_err(|e| {
                PluginError::ExecutionError {
                    name: "isolate".into(),
                    message: e.to_string(),
                }
            })?;

            Ok(RhaiRuntime::dynamic_to_value(&result))
        })
    }

    fn set_global(&mut self, _name: &str, _value: Value) -> PluginResult<()> {
        // Rhai doesn't support persistent globals without scope
        // This would need to be handled differently
        Ok(())
    }

    fn get_global(&self, _name: &str) -> PluginResult<Value> {
        Ok(Value::Null)
    }
}
