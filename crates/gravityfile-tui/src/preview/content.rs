//! Preview content types and loading.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ratatui::layout::Size;
use ratatui::text::Line;

use super::image::ImageProtocol;
use super::syntax::SyntaxHighlighter;

/// Maximum file size to attempt preview (10 MB).
const MAX_PREVIEW_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum lines to read for preview.
const MAX_PREVIEW_LINES: usize = 500;

/// Maximum archive entries to scan for preview.
/// Prevents hanging on archives with millions of entries.
const MAX_ARCHIVE_ENTRIES: usize = 10_000;

/// Maximum archive file size to attempt preview (100 MB).
/// Larger archives should be extracted to view contents.
const MAX_ARCHIVE_PREVIEW_SIZE: u64 = 100 * 1024 * 1024;
const MAX_DECODED_PREVIEW_SIZE: u64 = 128 * 1024 * 1024;
const MAX_XZ_PREVIEW_MEMORY: u64 = 256 * 1024 * 1024;
const MAX_ZIP_PREVIEW_READ_BYTES: u64 = 64 * 1024 * 1024;

/// Return an error, never a false EOF, when a preview read budget is exceeded.
struct PreviewReadBudget<R> {
    reader: R,
    remaining: u64,
    exceeded: bool,
}

impl<R> PreviewReadBudget<R> {
    fn new(reader: R, remaining: u64) -> Self {
        Self {
            reader,
            remaining,
            exceeded: false,
        }
    }
}

impl<R: Read> Read for PreviewReadBudget<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.exceeded {
            return Err(std::io::Error::other(
                "Archive preview read budget exceeded",
            ));
        }
        if self.remaining == 0 {
            let count = self.reader.read(&mut [0u8; 1])?;
            if count == 0 {
                return Ok(0);
            }
            self.exceeded = true;
            return Err(std::io::Error::other(
                "Archive preview read budget exceeded",
            ));
        }
        let length = (buffer.len() as u64).min(self.remaining) as usize;
        let count = self.reader.read(&mut buffer[..length])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

impl<R: Seek> Seek for PreviewReadBudget<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        // Parser retries count toward the same cumulative read budget.
        self.reader.seek(position)
    }
}

/// Number of bytes to inspect for binary detection.
const BINARY_CHECK_BYTES: usize = 1024;

/// Tab size for display.
const TAB_SIZE: u8 = 4;

/// Error that can occur during preview loading.
#[derive(Debug, Clone)]
pub enum PreviewError {
    /// File is too large to preview.
    TooLarge(u64),
    /// File could not be read.
    IoError(String),
    /// No preview available.
    NoPreview,
}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(size) => write!(f, "File too large: {} bytes", size),
            Self::IoError(e) => write!(f, "I/O error: {}", e),
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
    /// Render the file as an image using the terminal's graphics protocol.
    Image,
}

impl PreviewMode {
    /// Cycle to the next preview mode.
    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Text,
            Self::Text => Self::Hex,
            Self::Hex => Self::Metadata,
            Self::Metadata => Self::Image,
            Self::Image => Self::Auto,
        }
    }

    /// Get display name for the mode.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Hex => "hex",
            Self::Metadata => "meta",
            Self::Image => "image",
        }
    }
}

/// An entry in an archive listing.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Path within the archive.
    pub path: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Whether this is a symbolic link.
    pub is_symlink: bool,
    /// Symlink target path, if this is a symlink.
    pub link_target: Option<String>,
    /// Compression ratio (compressed/uncompressed), if available.
    pub compression_ratio: Option<f64>,
}

/// Content that can be displayed in the preview pane.
#[derive(Debug, Clone, Default)]
pub enum PreviewContent {
    /// Syntax-highlighted text lines.
    Text {
        lines: Vec<Line<'static>>,
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
        /// First 500 entries in directories-first, name order.
        entries: Vec<(String, bool)>, // (name, is_dir)
        /// Total entries observed during successful enumeration.
        total_entries: usize,
    },
    /// Archive contents listing.
    Archive {
        /// Archive format (zip, tar, tar.gz, etc.).
        format: String,
        /// Total number of entries in the archive.
        entry_count: usize,
        /// Total uncompressed size.
        total_size: u64,
        /// Entries to display (limited for preview).
        entries: Vec<ArchiveEntry>,
    },
    /// A decoded image, already encoded for the terminal's graphics protocol.
    Image {
        /// Encoded protocol payload, ready to blit.
        protocol: Box<ImageProtocol>,
        /// Source pixel width.
        width: u32,
        /// Source pixel height.
        height: u32,
        /// Detected image format (e.g. "PNG").
        format: String,
    },
    /// Error message.
    Error(String),
    /// Preview content is being loaded off the UI thread.
    Loading,
    /// Empty preview.
    #[default]
    Empty,
}

/// Everything the loader needs to render an image for this terminal.
///
/// The picker carries the detected graphics protocol and font size, which are
/// terminal properties; the area is the preview pane's current size in cells.
#[derive(Clone)]
pub struct ImageContext {
    /// Detected terminal graphics capabilities.
    pub picker: ratatui_image::picker::Picker,
    /// Preview pane size, in character cells.
    pub area: Size,
}

/// Loads preview content for files.
pub struct PreviewLoader;

impl PreviewLoader {
    /// Load preview content with a specific mode.
    ///
    /// `image_context` supplies the terminal's graphics capabilities and the
    /// size of the preview pane. It is `None` when the terminal was never
    /// queried (non-interactive use), in which case image files fall back to
    /// the hex/metadata view rather than failing.
    pub fn load_with_mode(
        path: &Path,
        mode: PreviewMode,
        image_context: Option<&ImageContext>,
    ) -> Result<PreviewContent, PreviewError> {
        if path.is_dir() {
            return Self::load_directory(path);
        }

        if !path.is_file() {
            return Err(PreviewError::NoPreview);
        }

        match mode {
            PreviewMode::Auto => {
                if let Some(context) = image_context
                    && super::image::is_image_path(path)
                {
                    return Self::load_image(path, context);
                }
                Self::load_file(path)
            }
            PreviewMode::Text => Self::load_text(path),
            PreviewMode::Hex => Self::load_hex_preview(path),
            PreviewMode::Metadata => Self::load_metadata(path),
            PreviewMode::Image => match image_context {
                Some(context) => Self::load_image(path, context),
                None => Err(PreviewError::NoPreview),
            },
        }
    }

    /// Decode and encode an image for the terminal's graphics protocol.
    fn load_image(path: &Path, context: &ImageContext) -> Result<PreviewContent, PreviewError> {
        let (protocol, width, height, format) =
            super::image::load(path, &context.picker, context.area)?;
        Ok(PreviewContent::Image {
            protocol: Box::new(protocol),
            width,
            height,
            format,
        })
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

    pub(super) fn open_preview_file(path: &Path) -> Result<File, PreviewError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|error| PreviewError::IoError(error.to_string()))?;
        if !file
            .metadata()
            .map_err(|error| PreviewError::IoError(error.to_string()))?
            .is_file()
        {
            return Err(PreviewError::NoPreview);
        }
        Ok(file)
    }

    /// Force load as text (even if binary).
    fn load_text(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = Self::open_preview_file(path)?;
        let metadata = file
            .metadata()
            .map_err(|e| PreviewError::IoError(e.to_string()))?;

        if metadata.len() > MAX_PREVIEW_SIZE {
            return Err(PreviewError::TooLarge(metadata.len()));
        }

        Self::read_text(file, path)
    }

    /// Enforce the byte budget on reads, including files that grow after metadata.
    fn read_text(reader: impl Read, path: &Path) -> Result<PreviewContent, PreviewError> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_PREVIEW_SIZE + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| PreviewError::IoError(error.to_string()))?;
        if bytes.len() as u64 > MAX_PREVIEW_SIZE {
            return Err(PreviewError::TooLarge(bytes.len() as u64));
        }
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            PreviewError::IoError(format!("Text preview is not valid UTF-8: {error}"))
        })?;
        let line_count = text.lines().count();
        let lines: Vec<String> = text
            .lines()
            .take(MAX_PREVIEW_LINES)
            .map(str::to_owned)
            .collect();
        // Budget before syntax detection too: first-line matching can otherwise
        // run regexes over an arbitrarily long minified line.
        let syntax = SyntaxHighlighter::within_budget(&lines)
            .then(|| SyntaxHighlighter::find_syntax(path, lines.first().map(String::as_str)))
            .flatten();
        let (lines, highlighted) = match syntax {
            Some(syntax) => SyntaxHighlighter::highlight_with_status(&lines, syntax, TAB_SIZE),
            None => (SyntaxHighlighter::plain_lines(&lines, TAB_SIZE), false),
        };
        Ok(PreviewContent::Text {
            lines,
            total_lines: line_count,
            highlighted,
        })
    }

    /// Force load as hex dump.
    fn load_hex_preview(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = Self::open_preview_file(path)?;
        let total_bytes = file
            .metadata()
            .map_err(|error| PreviewError::IoError(error.to_string()))?
            .len();
        Self::load_hex(file, total_bytes)
    }

    /// Detect archive format from file extension.
    fn detect_archive_format(path: &Path) -> Option<&'static str> {
        let name = path.file_name()?.to_str()?.to_lowercase();

        if name.ends_with(".zip") {
            Some("zip")
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some("tar.gz")
        } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
            Some("tar.bz2")
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some("tar.xz")
        } else if name.ends_with(".tar") {
            Some("tar")
        } else if name.ends_with(".7z") {
            Some("7z")
        } else if name.ends_with(".rar") {
            Some("rar")
        } else {
            None
        }
    }

    /// Load archive contents as preview.
    fn load_archive(path: &Path, format: &str) -> Result<PreviewContent, PreviewError> {
        match format {
            "zip" => Self::load_zip_archive(path),
            "tar" => Self::load_tar_archive(path),
            "tar.gz" => Self::load_tar_gz_archive(path),
            "tar.bz2" => Self::load_tar_bz2_archive(path),
            "tar.xz" => Self::load_tar_xz_archive(path),
            "7z" | "rar" => {
                // These require external tools - show info message
                Ok(PreviewContent::Error(format!(
                    "{} archives require external tools for preview. Use :extract to extract.",
                    format.to_uppercase()
                )))
            }
            _ => Err(PreviewError::NoPreview),
        }
    }

    /// Load ZIP archive contents.
    fn load_zip_archive(path: &Path) -> Result<PreviewContent, PreviewError> {
        Self::load_zip_with_budget(path, MAX_ZIP_PREVIEW_READ_BYTES)
    }

    fn load_zip_with_budget(path: &Path, limit: u64) -> Result<PreviewContent, PreviewError> {
        // Check file size before attempting to parse
        let file = Self::open_preview_file(path)?;
        let file_size = file
            .metadata()
            .map_err(|e| PreviewError::IoError(e.to_string()))?
            .len();

        if file_size > MAX_ARCHIVE_PREVIEW_SIZE {
            return Err(PreviewError::TooLarge(file_size));
        }

        let mut reader = PreviewReadBudget::new(file, limit);
        let result = Self::inspect_zip(&mut reader);
        // ZIP can retry records and replace the original read error. Preserve
        // exhaustion even if a fallback parser result hides it.
        if reader.exceeded {
            return Err(PreviewError::IoError(format!(
                "ZIP preview exceeds cumulative read budget of {limit} bytes"
            )));
        }
        result
    }

    fn inspect_zip(reader: impl Read + Seek) -> Result<PreviewContent, PreviewError> {
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|e| PreviewError::IoError(format!("Invalid ZIP: {}", e)))?;

        let entry_count = archive.len();

        if entry_count > MAX_ARCHIVE_ENTRIES {
            return Err(PreviewError::IoError(format!(
                "Archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry preview limit; inventory not inspected"
            )));
        }
        let mut entries = Vec::new();
        let mut total_size = 0u64;
        for index in 0..entry_count {
            let entry = archive.by_index(index).map_err(|error| {
                PreviewError::IoError(format!("Cannot inspect ZIP entry {index}: {error}"))
            })?;
            let size = entry.size();
            total_size = total_size
                .checked_add(size)
                .ok_or_else(|| PreviewError::IoError("Archive size total overflows u64".into()))?;
            if entries.len() < MAX_PREVIEW_LINES {
                entries.push(ArchiveEntry {
                    path: entry.name().to_string(),
                    size,
                    is_dir: entry.is_dir(),
                    is_symlink: entry
                        .unix_mode()
                        .is_some_and(|mode| (mode & 0o170000) == 0o120000),
                    link_target: None,
                    compression_ratio: (size > 0)
                        .then(|| entry.compressed_size() as f64 / size as f64),
                });
            }
        }
        Ok(PreviewContent::Archive {
            format: "ZIP".into(),
            entry_count,
            total_size,
            entries,
        })
    }

    /// Load plain TAR archive contents.
    fn load_tar_archive(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = Self::open_archive_reader(path)?;
        Self::load_tar_from_reader(file, "TAR")
    }

    /// Load TAR.GZ archive contents.
    fn load_tar_gz_archive(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = Self::open_archive_reader(path)?;
        let decoder = flate2::read::MultiGzDecoder::new(file);
        Self::load_tar_from_reader(decoder, "TAR.GZ")
    }

    /// Load TAR.BZ2 archive contents.
    fn load_tar_bz2_archive(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = Self::open_archive_reader(path)?;
        let decoder = bzip2::read::MultiBzDecoder::new(file);
        Self::load_tar_from_reader(decoder, "TAR.BZ2")
    }

    /// Load TAR.XZ archive contents.
    fn load_tar_xz_archive(path: &Path) -> Result<PreviewContent, PreviewError> {
        let file = Self::open_archive_reader(path)?;
        let stream = xz2::stream::Stream::new_stream_decoder(
            MAX_XZ_PREVIEW_MEMORY,
            xz2::stream::CONCATENATED,
        )
        .map_err(|error| PreviewError::IoError(error.to_string()))?;
        let decoder = xz2::read::XzDecoder::new_stream(file, stream);
        Self::load_tar_from_reader(decoder, "TAR.XZ")
    }

    /// Check the opened file and enforce its input budget during decoding.
    fn open_archive_reader(path: &Path) -> Result<PreviewReadBudget<File>, PreviewError> {
        let file = Self::open_preview_file(path)?;
        let size = file
            .metadata()
            .map_err(|error| PreviewError::IoError(error.to_string()))?
            .len();
        if size > MAX_ARCHIVE_PREVIEW_SIZE {
            return Err(PreviewError::TooLarge(size));
        }
        Ok(PreviewReadBudget::new(file, MAX_ARCHIVE_PREVIEW_SIZE))
    }

    /// Load TAR archive from a reader.
    fn load_tar_from_reader<R: Read>(
        reader: R,
        format: &str,
    ) -> Result<PreviewContent, PreviewError> {
        let mut archive =
            tar::Archive::new(PreviewReadBudget::new(reader, MAX_DECODED_PREVIEW_SIZE));
        let entries_iter = archive
            .entries()
            .map_err(|e| PreviewError::IoError(e.to_string()))?;

        let mut entries = Vec::new();
        let mut entry_count = 0;
        let mut total_size: u64 = 0;

        for entry_result in entries_iter {
            // Stop if we've scanned too many entries (prevent hanging)
            if entry_count >= MAX_ARCHIVE_ENTRIES {
                return Err(PreviewError::IoError(format!(
                    "Archive exceeds the {MAX_ARCHIVE_ENTRIES}-entry preview limit; inventory incomplete"
                )));
            }

            let entry = entry_result.map_err(|e| PreviewError::IoError(e.to_string()))?;
            let size = entry.size();
            total_size = total_size
                .checked_add(size)
                .ok_or_else(|| PreviewError::IoError("Archive size total overflows u64".into()))?;
            entry_count += 1;

            if entries.len() < MAX_PREVIEW_LINES {
                let path = entry
                    .path()
                    .map_err(|error| PreviewError::IoError(error.to_string()))?
                    .to_string_lossy()
                    .into_owned();
                let entry_type = entry.header().entry_type();
                let is_dir = entry_type.is_dir();
                let is_symlink = entry_type.is_symlink() || entry_type.is_hard_link();

                // Get symlink target if available
                let link_target = if is_symlink {
                    entry
                        .link_name()
                        .map_err(|error| PreviewError::IoError(error.to_string()))?
                        .map(|p| p.to_string_lossy().to_string())
                } else {
                    None
                };

                entries.push(ArchiveEntry {
                    path,
                    size,
                    is_dir,
                    is_symlink,
                    link_target,
                    compression_ratio: None, // TAR doesn't have per-file compression info
                });
            }
        }

        // TAR iteration can stop before the codec has checked its trailer.
        // Drain through the same decoded-byte budget, accepting only bounded
        // zero padding. A later compression member must not hide another TAR
        // or a malformed trailer behind an apparently complete inventory.
        let mut reader = archive.into_inner();
        let mut padding = 0usize;
        let mut buffer = [0u8; 8192];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| PreviewError::IoError(error.to_string()))?;
            if count == 0 {
                break;
            }
            padding += count;
            if padding > 1024 * 1024 {
                return Err(PreviewError::IoError(
                    "TAR trailing padding exceeds 1 MiB".into(),
                ));
            }
            if buffer[..count].iter().any(|byte| *byte != 0) {
                return Err(PreviewError::IoError(
                    "Nonzero data follows the TAR end marker".into(),
                ));
            }
        }

        Ok(PreviewContent::Archive {
            format: format.to_string(),
            entry_count,
            total_size,
            entries,
        })
    }

    /// Load preview for a regular file.
    fn load_file(path: &Path) -> Result<PreviewContent, PreviewError> {
        // Check if it's an archive first
        if let Some(format) = Self::detect_archive_format(path) {
            return Self::load_archive(path, format);
        }

        let file = Self::open_preview_file(path)?;
        let metadata = file
            .metadata()
            .map_err(|e| PreviewError::IoError(e.to_string()))?;

        // Check file size
        if metadata.len() > MAX_PREVIEW_SIZE {
            return Err(PreviewError::TooLarge(metadata.len()));
        }

        // Check if binary
        let mut reader = BufReader::new(file);
        let binary = Self::is_binary(&mut reader)?;

        // Reuse the inspected descriptor, even if the pathname is replaced.
        // Seek back to start
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|e| PreviewError::IoError(e.to_string()))?;

        if binary {
            Self::load_hex(reader, metadata.len())
        } else {
            Self::read_text(reader, path)
        }
    }

    /// Check if file content appears to be binary.
    fn is_binary(reader: impl Read) -> Result<bool, PreviewError> {
        let mut bytes = Vec::new();
        reader
            .take(BINARY_CHECK_BYTES as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| PreviewError::IoError(error.to_string()))?;
        Ok(bytes.contains(&0))
    }

    /// Load a bounded hex preview from the already inspected file.
    fn load_hex(reader: impl Read, total_bytes: u64) -> Result<PreviewContent, PreviewError> {
        let mut buf = Vec::new();
        reader
            .take((MAX_PREVIEW_LINES as u64 * 16).min(total_bytes))
            .read_to_end(&mut buf)
            .map_err(|error| PreviewError::IoError(error.to_string()))?;

        let lines: Vec<Line<'static>> = buf
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
        let entries = std::fs::read_dir(path)
            .map_err(|error| PreviewError::IoError(error.to_string()))?
            .map(|entry| {
                let entry = entry?;
                let is_dir = entry.file_type()?.is_dir();
                Ok((entry.file_name().to_string_lossy().into_owned(), is_dir))
            });
        Self::select_directory_entries(entries)
    }

    fn select_directory_entries(
        entries: impl IntoIterator<Item = std::io::Result<(String, bool)>>,
    ) -> Result<PreviewContent, PreviewError> {
        // A max heap drops the last entry in display order. At most 501 names
        // are live here, regardless of directory width or enumeration order.
        let mut selected = std::collections::BinaryHeap::with_capacity(MAX_PREVIEW_LINES + 1);
        let mut total_entries = 0;
        for entry in entries {
            let (name, is_dir) = entry.map_err(|error| PreviewError::IoError(error.to_string()))?;
            total_entries += 1;
            selected.push((!is_dir, name));
            if selected.len() > MAX_PREVIEW_LINES {
                selected.pop();
            }
        }
        let entries = selected
            .into_sorted_vec()
            .into_iter()
            .map(|(is_file, name)| (name, !is_file))
            .collect();
        Ok(PreviewContent::Directory {
            entries,
            total_entries,
        })
    }
}

/// State for managing preview in the app.
#[derive(Default)]
pub struct PreviewState {
    /// Currently loaded preview.
    pub content: PreviewContent,
    /// Path being previewed.
    pub path: Option<PathBuf>,
    /// Whether preview is loading.
    pub loading: bool,
    /// Current preview mode.
    pub mode: PreviewMode,
    /// Rows scrolled past at the top of a text, hex or listing preview.
    pub scroll: usize,
}

impl PreviewState {
    /// Create new preview state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a path as loading. Returns `false` when the requested path is
    /// already current and no forced reload is needed.
    pub fn begin_update(&mut self, path: &Path, force: bool) -> bool {
        if !force && self.path.as_deref() == Some(path) {
            return false;
        }
        self.path = Some(path.to_path_buf());
        self.scroll = 0;
        self.loading = true;
        self.content = PreviewContent::Loading;
        true
    }

    /// Install a completed preview if it still matches the current request.
    pub fn complete_update(
        &mut self,
        path: &Path,
        mode: PreviewMode,
        result: Result<PreviewContent, PreviewError>,
    ) {
        if self.path.as_deref() != Some(path) || self.mode != mode {
            return;
        }
        self.content = result.unwrap_or_else(|error| PreviewContent::Error(error.to_string()));
        self.loading = false;
    }

    /// Clear the preview and forget its path so returning to Miller mode
    /// reloads the selected file.
    pub fn clear(&mut self) {
        self.path = None;
        self.content = PreviewContent::Empty;
        self.scroll = 0;
        self.loading = false;
    }

    /// Scroll by `delta` rows, keeping the last screenful of `visible` rows
    /// in view at the bottom.
    pub fn scroll_by(&mut self, delta: isize, visible: usize) {
        let rows = match &self.content {
            PreviewContent::Text { lines, .. } | PreviewContent::Hex { lines, .. } => lines.len(),
            PreviewContent::Directory { entries, .. } => entries.len(),
            PreviewContent::Archive { entries, .. } => entries.len(),
            _ => 0,
        };
        let max = rows.saturating_sub(visible);
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    /// Cycle to the next preview mode. The application schedules the reload.
    pub fn cycle_mode(&mut self) {
        self.mode = self.mode.cycle();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_highlighting_input_is_preserved_and_labelled_plain() {
        use super::*;
        let text = format!("{}\nlast", "x".repeat(17 * 1024));
        let PreviewContent::Text {
            lines,
            total_lines,
            highlighted,
        } = PreviewLoader::read_text(text.as_bytes(), Path::new("large.rs")).unwrap()
        else {
            panic!("expected text")
        };
        assert!(!highlighted);
        assert_eq!(total_lines, 2);
        assert_eq!(lines[0].to_string(), "x".repeat(17 * 1024));
        assert_eq!(lines[1].to_string(), "last");
    }
    #[test]
    fn preview_seeks_do_not_replenish_read_budget() {
        use super::*;
        let mut reader = PreviewReadBudget::new(std::io::Cursor::new(b"abcdef"), 5);
        let mut bytes = [0; 4];
        reader.read_exact(&mut bytes).unwrap();
        reader.rewind().unwrap();
        assert_eq!(reader.read(&mut bytes).unwrap(), 1);
        assert!(reader.read(&mut bytes).is_err());
        assert!(reader.exceeded);
        reader.rewind().unwrap();
        assert!(reader.read(&mut bytes).is_err());
    }

    #[test]
    fn zip_preview_enforces_budget_before_inventory_success() {
        use super::*;
        use std::io::Write;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("valid.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer
            .start_file("item", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"contents").unwrap();
        writer.finish().unwrap();
        assert!(PreviewLoader::load_zip_archive(&path).is_ok());
        assert!(
            matches!(PreviewLoader::load_zip_with_budget(&path, 8), Err(PreviewError::IoError(message)) if message.contains("cumulative read budget"))
        );
        assert!(PreviewLoader::load_zip_archive(&path).is_ok());
    }
    #[test]
    fn bounded_archive_readers_keep_all_supported_tar_previews_working() {
        use super::*;
        use std::io::Write;
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "item", &b"data"[..]).unwrap();
        let bytes = tar.into_inner().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&bytes).unwrap();
        let mut bzip = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        bzip.write_all(&bytes).unwrap();
        let mut xz = xz2::write::XzEncoder::new(Vec::new(), 1);
        xz.write_all(&bytes).unwrap();
        let fixture = tempfile::tempdir().unwrap();
        for (format, data) in [
            ("tar", bytes),
            ("tar.gz", gzip.finish().unwrap()),
            ("tar.bz2", bzip.finish().unwrap()),
            ("tar.xz", xz.finish().unwrap()),
        ] {
            let path = fixture.path().join(format!("archive.{format}"));
            std::fs::write(&path, data).unwrap();
            let PreviewContent::Archive {
                entry_count,
                total_size,
                entries,
                ..
            } = PreviewLoader::load_archive(&path, format).unwrap()
            else {
                panic!("expected archive")
            };
            assert_eq!(entry_count, 1);
            assert_eq!(total_size, 4);
            assert_eq!(entries[0].path, "item");
        }
    }
    #[test]
    fn archive_read_budget_distinguishes_exact_eof_from_overflow() {
        use super::*;
        let mut exact = PreviewReadBudget::new(&b"abcd"[..], 4);
        let mut output = Vec::new();
        exact.read_to_end(&mut output).unwrap();
        assert_eq!(output, b"abcd");
        let mut growing = PreviewReadBudget::new(std::io::repeat(0), 4);
        output.clear();
        assert!(growing.read_to_end(&mut output).is_err());
        assert_eq!(output.len(), 4);
        assert!(growing.read(&mut [0]).is_err());
    }

    #[test]
    fn compressed_tar_previews_validate_all_members_and_trailers() {
        use super::*;
        use std::io::Write;
        fn encode(format: &str, bytes: &[u8]) -> Vec<u8> {
            match format {
                "tar.gz" => {
                    let mut encoder =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                    encoder.write_all(bytes).unwrap();
                    encoder.finish().unwrap()
                }
                "tar.bz2" => {
                    let mut encoder =
                        bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
                    encoder.write_all(bytes).unwrap();
                    encoder.finish().unwrap()
                }
                "tar.xz" => {
                    let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
                    encoder.write_all(bytes).unwrap();
                    encoder.finish().unwrap()
                }
                _ => unreachable!(),
            }
        }
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1024);
        header.set_mode(0o600);
        builder
            .append_data(&mut header, "payload", &[42; 1024][..])
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        let directory = tempfile::tempdir().unwrap();
        for format in ["tar.gz", "tar.bz2", "tar.xz"] {
            let path = directory.path().join(format!("fixture.{format}"));
            // Member boundaries may divide a TAR header, body, or end padding.
            for split in [13, 600, bytes.len() - 100] {
                let mut encoded = encode(format, &bytes[..split]);
                encoded.extend(encode(format, &bytes[split..]));
                std::fs::write(&path, &encoded).unwrap();
                let PreviewContent::Archive {
                    entry_count,
                    total_size,
                    ..
                } = PreviewLoader::load_archive(&path, format).unwrap()
                else {
                    panic!("expected archive")
                };
                assert_eq!(entry_count, 1);
                assert_eq!(total_size, 1024);
                encoded.pop();
                std::fs::write(&path, encoded).unwrap();
                assert!(
                    PreviewLoader::load_archive(&path, format).is_err(),
                    "{format} truncated final member"
                );
            }
            let mut hidden = encode(format, &bytes);
            hidden.extend(encode(format, b"hidden later data"));
            std::fs::write(&path, hidden).unwrap();
            assert!(
                matches!(PreviewLoader::load_archive(&path, format), Err(PreviewError::IoError(message)) if message.contains("Nonzero data"))
            );

            let mut corrupt = encode(format, &bytes);
            let index = corrupt.len() - 8;
            corrupt[index] ^= 0xff;
            std::fs::write(&path, corrupt).unwrap();
            assert!(
                PreviewLoader::load_archive(&path, format).is_err(),
                "{format} corrupt trailer"
            );
        }
    }

    #[test]
    fn tar_preview_rejects_excessive_trailing_padding() {
        use super::*;
        let reader = std::io::repeat(0).take(2 * 1024 * 1024);
        assert!(
            matches!(PreviewLoader::load_tar_from_reader(reader, "TAR"), Err(PreviewError::IoError(message)) if message.contains("padding exceeds"))
        );
    }

    #[test]
    fn tar_preview_limits_decoded_bytes_even_with_few_entries() {
        use super::*;
        let mut header = tar::Header::new_gnu();
        header.set_path("large").unwrap();
        header.set_size(MAX_DECODED_PREVIEW_SIZE + 1024);
        header.set_mode(0o600);
        header.set_cksum();
        let reader = header.as_bytes().as_slice().chain(std::io::repeat(0));
        assert!(
            matches!(PreviewLoader::load_tar_from_reader(reader, "TAR"), Err(PreviewError::IoError(message)) if message.contains("budget exceeded"))
        );
    }

    #[test]
    fn xz_preview_refuses_an_excessive_dictionary_request() {
        use super::*;
        // Valid stream/block CRCs, with an LZMA2 property requesting 4 GiB.
        let header = [
            253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 2, 0, 33, 1, 40, 0, 0, 0, 230, 160,
            17, 179,
        ];
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("large.tar.xz");
        std::fs::write(&path, header).unwrap();
        assert!(
            matches!(PreviewLoader::load_tar_xz_archive(&path), Err(PreviewError::IoError(message)) if message.to_lowercase().contains("memory"))
        );
    }
    #[test]
    fn zip_inventory_counts_unshown_entries_and_rejects_unreadable_ones() {
        use super::*;
        use std::io::Write;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("many.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        for index in 0..MAX_PREVIEW_LINES + 1 {
            writer
                .start_file(
                    format!("entry-{index}"),
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(b"x").unwrap();
        }
        writer.finish().unwrap();
        let PreviewContent::Archive {
            entry_count,
            total_size,
            entries,
            ..
        } = PreviewLoader::load_zip_archive(&path).unwrap()
        else {
            panic!("expected archive")
        };
        assert_eq!(entry_count, MAX_PREVIEW_LINES + 1);
        assert_eq!(total_size, (MAX_PREVIEW_LINES + 1) as u64);
        assert_eq!(entries.len(), MAX_PREVIEW_LINES);
        let mut bytes = std::fs::read(&path).unwrap();
        let last_header = bytes
            .windows(4)
            .rposition(|chunk| chunk == b"PK\x03\x04")
            .unwrap();
        bytes[last_header] = 0;
        std::fs::write(&path, bytes).unwrap();
        assert!(
            matches!(PreviewLoader::load_zip_archive(&path), Err(PreviewError::IoError(message)) if message.contains("entry 500"))
        );
    }

    #[test]
    fn archive_entry_limits_fail_instead_of_returning_partial_totals() {
        use super::*;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("too-many.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let mut tar = tar::Builder::new(Vec::new());
        for index in 0..=MAX_ARCHIVE_ENTRIES {
            let name = format!("entry-{index}");
            zip.start_file(&name, zip::write::SimpleFileOptions::default())
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o600);
            header.set_cksum();
            tar.append_data(&mut header, &name, std::io::empty())
                .unwrap();
        }
        zip.finish().unwrap();
        let bytes = tar.into_inner().unwrap();
        for result in [
            PreviewLoader::load_zip_archive(&path),
            PreviewLoader::load_tar_from_reader(bytes.as_slice(), "TAR"),
        ] {
            assert!(
                matches!(result, Err(PreviewError::IoError(message)) if message.contains("preview limit"))
            );
        }
    }
    #[test]
    fn directory_selection_keeps_global_sorted_prefix_and_full_count() {
        use super::*;
        for reverse in [false, true] {
            let entries = (0..100_000).map(|index| {
                let index = if reverse { 99_999 - index } else { index };
                Ok((format!("entry-{index:06}"), index % 2 == 0))
            });
            let PreviewContent::Directory {
                entries,
                total_entries,
            } = PreviewLoader::select_directory_entries(entries).unwrap()
            else {
                panic!("expected directory")
            };
            assert_eq!(total_entries, 100_000);
            assert_eq!(entries.len(), MAX_PREVIEW_LINES);
            for (index, (name, is_dir)) in entries.iter().enumerate() {
                assert!(*is_dir);
                assert_eq!(name, &format!("entry-{:06}", index * 2));
            }
        }
    }

    #[test]
    fn directory_enumeration_errors_do_not_become_successful_partial_lists() {
        use super::*;
        let entries = [
            Ok(("first".into(), false)),
            Err(std::io::Error::other("entry unavailable")),
        ];
        assert!(
            matches!(PreviewLoader::select_directory_entries(entries), Err(PreviewError::IoError(message)) if message.contains("entry unavailable"))
        );
        let PreviewContent::Directory {
            entries,
            total_entries,
        } = PreviewLoader::select_directory_entries(std::iter::empty()).unwrap()
        else {
            panic!("expected directory")
        };
        assert!(entries.is_empty());
        assert_eq!(total_entries, 0);
    }
    #[test]
    fn binary_probe_and_hex_preview_handle_short_reads_and_enforce_caps() {
        use super::*;
        struct OneByte(std::io::Cursor<Vec<u8>>);
        impl Read for OneByte {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let limit = buffer.len().min(1);
                self.0.read(&mut buffer[..limit])
            }
        }
        assert!(
            PreviewLoader::is_binary(OneByte(std::io::Cursor::new(vec![b'a', b'b', 0]))).unwrap()
        );
        let preview =
            PreviewLoader::load_hex(OneByte(std::io::Cursor::new(vec![0, 1, 2, 3])), 4).unwrap();
        let PreviewContent::Hex { lines, .. } = preview else {
            panic!("expected hex")
        };
        assert!(lines[0].to_string().contains("00 01 02 03"));
        let PreviewContent::Hex { lines, total_bytes } =
            PreviewLoader::load_hex(std::io::repeat(0xff), u64::MAX).unwrap()
        else {
            panic!("expected hex")
        };
        assert_eq!(lines.len(), MAX_PREVIEW_LINES);
        assert_eq!(total_bytes, u64::MAX);
    }

    #[cfg(unix)]
    #[test]
    fn binary_preview_keeps_the_opened_file_when_its_path_is_replaced() {
        use super::*;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("binary");
        std::fs::write(&path, [0, 1, 2, 3]).unwrap();
        let file = PreviewLoader::open_preview_file(&path).unwrap();
        let mut reader = BufReader::new(file);
        assert!(PreviewLoader::is_binary(&mut reader).unwrap());
        std::fs::rename(&path, fixture.path().join("original")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        reader.seek(SeekFrom::Start(0)).unwrap();
        let PreviewContent::Hex { lines, .. } = PreviewLoader::load_hex(reader, 4).unwrap() else {
            panic!("expected hex")
        };
        assert!(lines[0].to_string().contains("00 01 02 03"));
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    }
    #[test]
    fn text_reader_enforces_runtime_budget_and_keeps_line_count() {
        use super::*;
        assert!(matches!(
            PreviewLoader::read_text(std::io::repeat(b'a'), Path::new("data")),
            Err(PreviewError::TooLarge(size)) if size == MAX_PREVIEW_SIZE + 1
        ));
        let result = PreviewLoader::read_text(
            std::io::repeat(b'\n').take(MAX_PREVIEW_SIZE),
            Path::new("data"),
        )
        .unwrap();
        let PreviewContent::Text {
            lines, total_lines, ..
        } = result
        else {
            panic!("expected text")
        };
        assert_eq!(total_lines, MAX_PREVIEW_SIZE as usize);
        assert_eq!(lines.len(), MAX_PREVIEW_LINES);
    }

    #[test]
    fn text_reader_reports_decoding_and_io_errors_instead_of_partial_success() {
        use super::*;
        assert!(PreviewLoader::read_text(&b"valid\ninvalid\xff"[..], Path::new("data")).is_err());
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected read failure"))
            }
        }
        assert!(
            matches!(PreviewLoader::read_text(Broken, Path::new("data")), Err(PreviewError::IoError(message)) if message.contains("injected"))
        );
        let result =
            PreviewLoader::read_text(&b"first\r\nsecond\r\r\nlast"[..], Path::new("data")).unwrap();
        let PreviewContent::Text {
            lines, total_lines, ..
        } = result
        else {
            panic!("expected text")
        };
        assert_eq!(total_lines, 3);
        assert_eq!(lines[1].to_string(), "second\\r");
    }

    #[cfg(unix)]
    #[test]
    fn text_open_rejects_fifo_without_waiting_for_a_writer() {
        use super::*;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("fifo");
        let native = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(PreviewLoader::open_preview_file(&path).is_err());
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
        );
    }
    #[test]
    fn auto_mode_renders_an_image_file_as_an_image() {
        use ratatui::layout::Size;
        use ratatui_image::picker::Picker;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pixel.png");
        let mut buffer = image::RgbaImage::new(8, 8);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([10, 200, 90, 255]);
        }
        buffer.save(&path).unwrap();

        let context = ImageContext {
            picker: Picker::halfblocks(),
            area: Size::new(12, 6),
        };

        let content = PreviewLoader::load_with_mode(&path, PreviewMode::Auto, Some(&context))
            .expect("image preview should load");
        assert!(
            matches!(content, PreviewContent::Image { format, .. } if format == "PNG"),
            "auto mode should pick the image renderer for a .png"
        );
    }

    #[test]
    fn auto_mode_falls_back_when_the_terminal_has_no_graphics_support() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pixel.png");
        let mut buffer = image::RgbaImage::new(8, 8);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([10, 200, 90, 255]);
        }
        buffer.save(&path).unwrap();

        // No image context: the file must still preview, just not as an image.
        let content = PreviewLoader::load_with_mode(&path, PreviewMode::Auto, None)
            .expect("preview should still load");
        assert!(
            !matches!(content, PreviewContent::Image { .. }),
            "without a picker there is no way to render an image"
        );
    }

    #[test]
    fn preview_mode_cycles_through_every_variant_and_back() {
        let mut mode = PreviewMode::Auto;
        let mut seen = vec![mode];
        for _ in 0..8 {
            mode = mode.cycle();
            if mode == PreviewMode::Auto {
                break;
            }
            seen.push(mode);
        }
        assert_eq!(mode, PreviewMode::Auto, "cycling must return to Auto");
        assert!(seen.contains(&PreviewMode::Image), "Image mode unreachable");
        assert_eq!(seen.len(), 5, "unexpected mode count: {seen:?}");
    }

    use super::*;

    #[test]
    fn preview_state_ignores_completion_for_a_stale_path() {
        let mut state = PreviewState::new();
        let first = Path::new("first.txt");
        let second = Path::new("second.txt");

        assert!(state.begin_update(first, false));
        assert!(state.begin_update(second, false));
        state.complete_update(
            first,
            PreviewMode::Auto,
            Ok(PreviewContent::Error("stale".into())),
        );

        assert!(state.loading);
        assert!(matches!(state.content, PreviewContent::Loading));

        state.complete_update(
            second,
            PreviewMode::Auto,
            Ok(PreviewContent::Error("fresh".into())),
        );
        assert!(!state.loading);
        assert!(matches!(
            state.content,
            PreviewContent::Error(ref message) if message == "fresh"
        ));
    }

    #[test]
    fn preview_state_ignores_completion_for_a_stale_mode() {
        let mut state = PreviewState::new();
        let path = Path::new("preview.txt");

        assert!(state.begin_update(path, false));
        state.cycle_mode();
        state.complete_update(
            path,
            PreviewMode::Auto,
            Ok(PreviewContent::Error("stale".into())),
        );

        assert!(state.loading);
        assert!(matches!(state.content, PreviewContent::Loading));
    }

    #[test]
    fn clearing_preview_forgets_path_and_allows_reload() {
        let mut state = PreviewState::new();
        let path = Path::new("preview.txt");

        assert!(state.begin_update(path, false));
        state.clear();

        assert!(state.path.is_none());
        assert!(!state.loading);
        assert!(matches!(state.content, PreviewContent::Empty));
        assert!(state.begin_update(path, false));
    }
}
