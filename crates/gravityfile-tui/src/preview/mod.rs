//! File preview system for the TUI.
//!
//! Provides syntax-highlighted previews for text files,
//! hex views for binary files, and directory listings.

mod content;
mod syntax;

pub use content::{PreviewContent, PreviewMode, PreviewState};

// Re-export for potential future use
#[allow(unused_imports)]
pub use content::{PreviewError, PreviewLoader};
pub use syntax::SyntaxHighlighter;
