#![doc = include_str!("../README.md")]

mod config;
mod hooks;
pub mod lua;
mod process;
mod runtime;
mod sandbox;
mod types;

pub use config::{PluginConfig, PluginMetadata};
pub use hooks::{Hook, HookContext, HookResult, Notice, NoticeLevel};
pub use process::{ProcessRuntime, run_worker};
pub use runtime::{IsolatedContext, PluginHandle, PluginManager, PluginRuntime};
pub use sandbox::{Permission, SandboxConfig};
pub use types::{PluginError, PluginKind, PluginResult, Value};
