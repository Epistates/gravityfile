//! File preview system for the TUI.
//!
//! Provides syntax-highlighted previews for text files,
//! hex views for binary files, and directory listings.

mod content;
mod image;
mod syntax;

pub use content::{ImageContext, PreviewContent, PreviewMode, PreviewState};
#[allow(unused_imports)]
pub use image::{ImageProtocol, is_image_path};

// Re-export for potential future use
#[allow(unused_imports)]
pub use content::{ArchiveEntry, PreviewError, PreviewLoader};
pub use syntax::SyntaxHighlighter;
