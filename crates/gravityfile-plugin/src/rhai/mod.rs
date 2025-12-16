//! Rhai plugin runtime implementation.
//!
//! This module provides a Rhai scripting runtime as an alternative to Lua.
//! Rhai has a Rust-like syntax that may be more familiar to Rust developers.

mod runtime;

pub use runtime::RhaiRuntime;
