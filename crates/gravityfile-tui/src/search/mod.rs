//! Fuzzy search implementation for file navigation.
//!
//! This module provides fast fuzzy matching using the nucleo library,
//! enabling quick navigation to any file or directory in the tree.

mod filter;
mod state;

pub use filter::{EntryKind, SearchEntry};
pub use state::SearchState;
