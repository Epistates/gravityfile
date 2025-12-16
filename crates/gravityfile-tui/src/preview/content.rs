//! Preview content types and loading.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ratatui::text::Line;

use super::SyntaxHighlighter;

/// Maximum file size to attempt preview (10 MB).
const MAX_PREVIEW_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum lines to read for preview.
const MAX_PREVIEW_LINES: usize = 500;

/// Number of bytes to inspect for binary detection.
const BINARY_CHECK_BYTES: usize = 1024;

/// Tab size for display.
const TAB_SIZE: u8 = 4;

/// Error that can occur during preview loading.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum PreviewError {
    /// File is too large to preview.
    TooLarge(u64),
    /// File is binary.
    Binary,
    /// File could not be read.
    IoError(String),
    /// File is a directory.
    IsDirectory,
    /// No preview available.
    NoPreview,
}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(size) => write!(f, "File too large: {} bytes", size),
            Self::Binary => write!(f, "Binary file"),
            Self::IoError(e) => write!(f, "I/O error: {}", e),
            Self::IsDirectory => write!(f, "Is a directory"),
            Self::NoPreview => write!(f, "No preview available"),
        }
    }
}

impl std::error::Error for PreviewError {}

/// Preview display mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewMode {
    /// Automatic mode - chooses best preview based on file type.
    #[default]
    Auto,
    /// Force text preview with syntax highlighting.
    Text,
    /// Force hex dump view.
    Hex,
    /// Show file metadata only.
    Metadata,
}

impl PreviewMode {
    /// Cycle to the next preview mode.
    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Text,
            Self::Text => Self::Hex,
            Self::Hex => Self::Metadata,
            Self::Metadata => Self::Auto,
        }
    }

    /// Get display name for the mode.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Hex => "hex",
            Self::Metadata => "meta",
        }
    }
}

/// Content that can be displayed in the preview pane.
#[derive(Debug, Clone)]
pub enum PreviewContent {
    /// Syntax-highlighted text lines.
    Text {
        lines: Vec<Line<'static>>,
        #[allow(dead_code)]
        total_lines: usize,
        highlighted: bool,
    },
    /// Hex dump of binary file.
    Hex {
        lines: Vec<Line<'static>>,
        total_bytes: u64,
    },
    /// File metadata.
    Metadata {
        size: u64,
        modified: Option<std::time::SystemTime>,
        created: Option<std::time::SystemTime>,
        accessed: Option<std::time::SystemTime>,
        file_type: String,
        permissions: Option<String>,
    },
    /// Directory listing.
    Directory {
        entries: Vec<(String, bool)>, // (name, is_dir)
    },
    /// Error message.
    Error(String),
    /// Empty preview.
    Empty,
}

impl Default for PreviewContent {
    fn default() -> Self {
        Self::Empty
    }
}

/// Loads preview content for files.
pub struct PreviewLoader;

impl PreviewLoader {
    /// Load preview content for a path (auto mode).
    pub fn load(path: &Path) -> Result<PreviewContent, PreviewError> {
        Self::load_with_mode(path, PreviewMode::Auto)
    }

    /// Load preview content with a specific mode.
    pub fn load_with_mode(path: &Path, mode: PreviewMode) -> Result<PreviewContent, PreviewError> {
        if path.is_dir() {
            return Self::load_directory(path);
        }

        if !path.is_file() {
            return Err(PreviewError::NoPreview);
        }

        match mode {
            PreviewMode::Auto => Self::load_file(path),
            PreviewMode::Text => Self::load_text(path),
            PreviewMode::Hex => Self::load_hex_preview(path),
            PreviewMode::Metadata => Self::load_metadata(path),
        }
    }

    /// Load file metadata.
    pub fn load_metadata(path: &Path) -> Result<PreviewContent, PreviewError> {
        let metadata = std::fs::metadata(path).map_err(|e| PreviewError::IoError(e.to_string()))?;

        let file_type = if metadata.is_file() {
            // Try to determine file type from extension
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("File (.{})", e))
                .unwrap_or_else(|| "File".to_string())
        } else if metadata.is_dir() {
            "Directory".to_string()
        } else if metadata.is_symlink() {
            "Symbolic Link".to_string()
        } else {
            "Other".to_string()
        };

        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(format!("{:o}", metadata.permissions().mode() & 0o777))
        };

        #[cfg(not(unix))]
        let permissions = None;

        Ok(PreviewContent::Metadata {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            accessed: metadata.accessed().ok(),
            file_type,
            permissions,
        })
    }

    /// Force load as text (even if binary).
    fn load_text(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = File::open(path).map_err(|e| PreviewError::IoError(e.to_string()))?;
        let metadata = file.metadata().map_err(|e| PreviewError::IoError(e.to_string()))?;

        if metadata.len() > MAX_PREVIEW_SIZE {
            return Err(PreviewError::TooLarge(metadata.len()));
        }

        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut line_count = 0;

        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    if lines.len() < MAX_PREVIEW_LINES {
                        lines.push(line);
                    }
                    line_count += 1;
                }
                Err(_) => break, // Stop on encoding errors
            }
        }

        // Try to syntax highlight
        let first_line = lines.first().map(|s| s.as_str());
        let highlighted = if let Some(syntax) = SyntaxHighlighter::find_syntax(path, first_line) {
            let highlighted_lines = SyntaxHighlighter::highlight_lines(&lines, syntax, TAB_SIZE);
            Some(highlighted_lines)
        } else {
            None
        };

        match highlighted {
            Some(hl_lines) => Ok(PreviewContent::Text {
                lines: hl_lines,
                total_lines: line_count,
                highlighted: true,
            }),
            None => Ok(PreviewContent::Text {
                lines: SyntaxHighlighter::plain_lines(&lines, TAB_SIZE),
                total_lines: line_count,
                highlighted: false,
            }),
        }
    }

    /// Force load as hex dump.
    fn load_hex_preview(path: &Path) -> Result<PreviewContent, PreviewError> {
        let metadata = std::fs::metadata(path).map_err(|e| PreviewError::IoError(e.to_string()))?;
        Self::load_hex(path, metadata.len())
    }

    /// Load preview for a regular file.
    fn load_file(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = File::open(path).map_err(|e| PreviewError::IoError(e.to_string()))?;
        let metadata = file.metadata().map_err(|e| PreviewError::IoError(e.to_string()))?;

        // Check file size
        if metadata.len() > MAX_PREVIEW_SIZE {
            return Err(PreviewError::TooLarge(metadata.len()));
        }

        // Check if binary
        let mut reader = BufReader::new(file);
        if Self::is_binary(&mut reader)? {
            return Self::load_hex(path, metadata.len());
        }

        // Seek back to start
        reader.seek(SeekFrom::Start(0)).map_err(|e| PreviewError::IoError(e.to_string()))?;

        // Read lines
        let mut lines = Vec::new();
        let mut line_buf = String::new();
        let mut line_count = 0;

        while reader.read_line(&mut line_buf).map_err(|e| PreviewError::IoError(e.to_string()))? > 0 {
            // Normalize line endings
            let line = line_buf.trim_end_matches(['\r', '\n']).to_string();

            if lines.len() < MAX_PREVIEW_LINES {
                lines.push(line);
            }
            line_count += 1;
            line_buf.clear();
        }

        // Try to syntax highlight
        let first_line = lines.first().map(|s| s.as_str());
        let highlighted = if let Some(syntax) = SyntaxHighlighter::find_syntax(path, first_line) {
            let highlighted_lines = SyntaxHighlighter::highlight_lines(&lines, syntax, TAB_SIZE);
            Some(highlighted_lines)
        } else {
            None
        };

        match highlighted {
            Some(hl_lines) => Ok(PreviewContent::Text {
                lines: hl_lines,
                total_lines: line_count,
                highlighted: true,
            }),
            None => Ok(PreviewContent::Text {
                lines: SyntaxHighlighter::plain_lines(&lines, TAB_SIZE),
                total_lines: line_count,
                highlighted: false,
            }),
        }
    }

    /// Check if file content appears to be binary.
    fn is_binary(reader: &mut BufReader<File>) -> Result<bool, PreviewError> {
        let mut buf = [0u8; BINARY_CHECK_BYTES];
        let bytes_read = reader.read(&mut buf).map_err(|e| PreviewError::IoError(e.to_string()))?;

        // Check for null bytes (common binary indicator)
        Ok(buf[..bytes_read].iter().any(|&b| b == 0))
    }

    /// Load hex preview for binary file.
    fn load_hex(path: &Path, total_bytes: u64) -> Result<PreviewContent, PreviewError> {
        let mut file = File::open(path).map_err(|e| PreviewError::IoError(e.to_string()))?;
        let mut buf = vec![0u8; (MAX_PREVIEW_LINES * 16).min(total_bytes as usize)];
        let bytes_read = file.read(&mut buf).map_err(|e| PreviewError::IoError(e.to_string()))?;

        let lines: Vec<Line<'static>> = buf[..bytes_read]
            .chunks(16)
            .enumerate()
            .map(|(i, chunk)| {
                let offset = format!("{:08x}  ", i * 16);
                let hex: String = chunk
                    .iter()
                    .map(|b| format!("{:02x} ", b))
                    .collect::<String>();
                let hex_padded = format!("{:48}", hex); // Pad to 48 chars (16 * 3)
                let ascii: String = chunk
                    .iter()
                    .map(|&b| {
                        if b.is_ascii_graphic() || b == b' ' {
                            b as char
                        } else {
                            '.'
                        }
                    })
                    .collect();
                Line::from(format!("{}{} |{}|", offset, hex_padded, ascii))
            })
            .collect();

        Ok(PreviewContent::Hex { lines, total_bytes })
    }

    /// Load directory listing preview.
    fn load_directory(path: &Path) -> Result<PreviewContent, PreviewError> {
        let mut entries: Vec<(String, bool)> = std::fs::read_dir(path)
            .map_err(|e| PreviewError::IoError(e.to_string()))?
            .filter_map(|entry| {
                entry.ok().map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    (name, is_dir)
                })
            })
            .collect();

        // Sort: directories first, then by name
        entries.sort_by(|(a_name, a_dir), (b_name, b_dir)| {
            match (a_dir, b_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a_name.cmp(b_name),
            }
        });

        // Limit entries
        entries.truncate(MAX_PREVIEW_LINES);

        Ok(PreviewContent::Directory { entries })
    }

    /// Load preview content asynchronously (for use in TUI event loop).
    #[allow(dead_code)]
    pub fn load_async(path: PathBuf) -> std::thread::JoinHandle<Result<PreviewContent, PreviewError>> {
        std::thread::spawn(move || Self::load(&path))
    }
}

/// State for managing preview in the app.
#[derive(Default)]
pub struct PreviewState {
    /// Currently loaded preview.
    pub content: PreviewContent,
    /// Path being previewed.
    pub path: Option<PathBuf>,
    /// Scroll offset.
    pub scroll: usize,
    /// Whether preview is loading.
    pub loading: bool,
    /// Current preview mode.
    pub mode: PreviewMode,
}

impl PreviewState {
    /// Create new preview state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the preview for a new path.
    pub fn update(&mut self, path: Option<&Path>) {
        match path {
            Some(p) if self.path.as_deref() != Some(p) => {
                self.path = Some(p.to_path_buf());
                self.scroll = 0;
                self.loading = true;
                // Load with current mode
                self.content = match PreviewLoader::load_with_mode(p, self.mode) {
                    Ok(content) => content,
                    Err(e) => PreviewContent::Error(e.to_string()),
                };
                self.loading = false;
            }
            None => {
                self.path = None;
                self.content = PreviewContent::Empty;
                self.scroll = 0;
            }
            _ => {} // Same path, no update needed
        }
    }

    /// Cycle to the next preview mode and reload.
    pub fn cycle_mode(&mut self) {
        self.mode = self.mode.cycle();
        // Reload with new mode if we have a path
        if let Some(path) = self.path.clone() {
            self.loading = true;
            self.content = match PreviewLoader::load_with_mode(&path, self.mode) {
                Ok(content) => content,
                Err(e) => PreviewContent::Error(e.to_string()),
            };
            self.loading = false;
            self.scroll = 0;
        }
    }

    /// Scroll preview up.
    #[allow(dead_code)]
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    /// Scroll preview down.
    #[allow(dead_code)]
    pub fn scroll_down(&mut self, amount: usize, max_lines: usize) {
        let content_lines = self.content_line_count();
        if content_lines > max_lines {
            self.scroll = (self.scroll + amount).min(content_lines - max_lines);
        }
    }

    /// Get the number of lines in the current content.
    #[allow(dead_code)]
    fn content_line_count(&self) -> usize {
        match &self.content {
            PreviewContent::Text { lines, .. } => lines.len(),
            PreviewContent::Hex { lines, .. } => lines.len(),
            PreviewContent::Metadata { .. } => 10, // Fixed number of metadata lines
            PreviewContent::Directory { entries } => entries.len(),
            PreviewContent::Error(_) => 1,
            PreviewContent::Empty => 0,
        }
    }
}
