//! Lua plugin runtime implementation.
//!
//! This module provides a Lua scripting runtime using mlua (Lua 5.4).
//! It follows patterns from Yazi's plugin system with two-stage initialization.

mod bindings;
mod isolate;
mod runtime;

pub use runtime::LuaRuntime;
