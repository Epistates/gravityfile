//! File preview system for the TUI.
//!
//! Provides syntax-highlighted previews for text files,
//! hex views for binary files, and directory listings.

mod content;
mod image;
mod syntax;

#[cfg(test)]
pub use content::ArchiveEntry;
pub use content::{
    ImageContext, PreviewContent, PreviewError, PreviewLoader, PreviewMode, PreviewState,
};
