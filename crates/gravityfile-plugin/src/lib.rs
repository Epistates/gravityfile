//! Lua plugins for gravityfile.
//!
//! Plugins are Lua scripts that subscribe to [`Hook`]s (startup, navigation,
//! scan completion, deletion, ...). The [`PluginManager`] dispatches hooks to
//! a [`PluginRuntime`]; in the application that runtime is a
//! [`ProcessRuntime`], which runs each plugin in its own supervised worker
//! process with deadlines and bounded IPC, so a misbehaving plugin cannot
//! block or crash the host. [`lua::LuaRuntime`] is the in-process engine the
//! worker uses, with a memory limit and an instruction-count deadline.
//!
//! A hook result can veto the default action (currently deletion) through
//! `prevent_default`.

mod config;
mod hooks;
pub mod lua;
mod process;
mod runtime;
mod sandbox;
mod types;

pub use config::{PluginConfig, PluginMetadata};
pub use hooks::{Hook, HookContext, HookResult};
pub use process::{ProcessRuntime, run_worker};
pub use runtime::{IsolatedContext, PluginHandle, PluginManager, PluginRuntime};
pub use sandbox::{Permission, SandboxConfig};
pub use types::{PluginError, PluginKind, PluginResult, Value};
