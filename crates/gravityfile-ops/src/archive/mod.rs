//! Archive extraction and creation.
//!
//! Extraction decodes into a private staging directory beside the
//! destination and is published only after the whole archive decoded and
//! passed validation, so a malformed or malicious archive never leaves
//! partial output behind.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::fs::{RenameError, rename_noreplace, resolves_within, staging_dir};

mod create;
mod extract;
mod parser_budget;
#[cfg(test)]
mod tests;

pub use create::{create_archive, create_archive_cancellable};

/// Cumulative reads during ZIP metadata parsing, including retries and seeks.
const MAX_ZIP_METADATA_READ_BYTES: u64 = 64 * 1024 * 1024;
/// TAR headers, extension bodies, sparse maps and skipped padding.
const MAX_TAR_METADATA_READ_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TAR_ENTRY_METADATA_READ_BYTES: u64 = 1024 * 1024;
/// Memory the XZ decoder may allocate for its dictionary.
const MAX_XZ_DECODER_MEMORY: u64 = 256 * 1024 * 1024;
/// ZIP symlink targets are stored as entry content; cap what is read.
const MAX_SYMLINK_TARGET_BYTES: u64 = 8 * 1024;

/// Resource limits that reject decompression bombs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtractLimits {
    /// Total bytes written across all entries.
    pub max_total_bytes: u64,
    /// Bytes written for any single entry.
    pub max_entry_bytes: u64,
    /// Number of entries.
    pub max_entries: u64,
    /// Declared uncompressed-to-compressed ratio of a ZIP entry.
    pub max_ratio: f64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 64 * 1024 * 1024 * 1024,
            max_entry_bytes: 64 * 1024 * 1024 * 1024,
            max_entries: 1_000_000,
            max_ratio: 10_000.0,
        }
    }
}

/// Error from an archive operation.
#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("Archive operation cancelled before publication")]
    Cancelled,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Unsupported archive format: {0}")]
    UnsupportedFormat(String),

    #[error("Archive not found: {0}")]
    NotFound(PathBuf),

    #[error("Destination already exists: {0}")]
    DestinationExists(PathBuf),

    /// Publication failed and some already-published entries could not be
    /// moved back out of the destination.
    #[error("Archive extraction failed: {source}; {count} published entries could not be withdrawn from {destination}", count = .left_behind.len())]
    ExtractionRollback {
        destination: PathBuf,
        #[source]
        source: Box<ArchiveError>,
        left_behind: Vec<PathBuf>,
    },

    #[error("Potential decompression bomb detected: {0}")]
    DecompressionBomb(String),
}

/// Result of an archive operation.
pub type ArchiveResult<T> = Result<T, ArchiveError>;

fn invalid_data(message: impl Into<String>) -> ArchiveError {
    ArchiveError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

/// Supported archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    TarZst,
}

impl ArchiveFormat {
    /// Every supported format.
    pub const ALL: [Self; 6] = [
        Self::Zip,
        Self::Tar,
        Self::TarGz,
        Self::TarBz2,
        Self::TarXz,
        Self::TarZst,
    ];

    /// Detect the format from a file name.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_lowercase();
        Self::ALL.into_iter().find(|format| {
            format
                .extensions()
                .iter()
                .any(|extension| name.ends_with(extension))
        })
    }

    /// Canonical extension, including the leading dot.
    pub fn extension(&self) -> &'static str {
        self.extensions()[0]
    }

    /// Recognized extensions; the first is canonical.
    fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Zip => &[".zip"],
            Self::Tar => &[".tar"],
            Self::TarGz => &[".tar.gz", ".tgz"],
            Self::TarBz2 => &[".tar.bz2", ".tbz2", ".tbz"],
            Self::TarXz => &[".tar.xz", ".txz"],
            Self::TarZst => &[".tar.zst", ".tzst"],
        }
    }
}

/// Extract an archive into `destination`, creating it if needed.
///
/// An existing destination directory is merged into: entries that already
/// exist (other than directories on both sides) are reported as conflicts
/// before anything is changed. Returns every extracted path at its final
/// location.
///
/// # Security
///
/// - Absolute paths and `..` components are rejected.
/// - Symlinks may not resolve outside the destination, checked by resolving
///   every link the way the kernel would after decoding.
/// - Setuid/setgid bits and extended attributes are not restored.
/// - Sizes, entry counts and ZIP compression ratios are bounded by
///   [`ExtractLimits`], measured on bytes actually written.
pub fn extract_archive(archive_path: &Path, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    extract_archive_with(
        archive_path,
        destination,
        &ExtractLimits::default(),
        &CancellationToken::new(),
    )
}

/// [`extract_archive`] with cooperative cancellation up to publication.
pub fn extract_archive_cancellable(
    archive_path: &Path,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    extract_archive_with(archive_path, destination, &ExtractLimits::default(), cancel)
}

/// [`extract_archive`] with explicit limits and cancellation.
pub fn extract_archive_with(
    archive_path: &Path,
    destination: &Path,
    limits: &ExtractLimits,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    check_cancel(cancel)?;
    let input = open_archive_input(archive_path)?;
    let format = ArchiveFormat::from_path(archive_path).ok_or_else(|| {
        ArchiveError::UnsupportedFormat(
            archive_path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("unknown")
                .to_string(),
        )
    })?;
    if let Ok(metadata) = fs::symlink_metadata(destination)
        && !metadata.is_dir()
    {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "extraction destination is not a directory",
        )));
    }
    let parent = crate::fs::parent_or_current(destination);
    fs::create_dir_all(parent)?;
    let destination = parent
        .canonicalize()?
        .join(destination.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "extraction destination has no file name",
            )
        })?);

    let staging = staging_dir(&destination, ".gravityfile-extract-")?;
    let payload = staging.path().join("payload");
    fs::create_dir(&payload)?;
    let payload = payload.canonicalize()?;

    let decoded = extract::decode(format, input, &payload, limits, cancel)
        .and_then(|entries| reject_escaping_links(&payload, cancel).map(|()| entries));
    let entries = match decoded {
        Ok(entries) => entries,
        Err(_) if cancel.is_cancelled() => return Err(ArchiveError::Cancelled),
        Err(error) => return Err(error),
    };
    check_cancel(cancel)?;
    publish(&payload, &destination)?;
    if let Err(error) = staging.close() {
        tracing::warn!(%error, "extraction published; staging cleanup incomplete");
    }
    Ok(entries
        .into_iter()
        .filter_map(|path| Some(destination.join(path.strip_prefix(&payload).ok()?)))
        .collect())
}

/// Kernel-accurate check that no decoded symlink resolves outside `root`.
fn reject_escaping_links(root: &Path, cancel: &CancellationToken) -> ArchiveResult<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            check_cancel(cancel)?;
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_symlink() && !resolves_within(root, &entry.path())? {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(&entry.path())
                    .to_path_buf();
                return Err(invalid_data(format!(
                    "Symlink escapes destination: {}",
                    relative.display()
                )));
            }
        }
    }
    Ok(())
}

/// Move the decoded tree into place without replacing anything.
///
/// A new destination is a single rename. Merging into an existing directory
/// first checks every entry for conflicts, then publishes each non-existing
/// subtree with one rename; on failure the published subtrees are moved back.
fn publish(payload: &Path, destination: &Path) -> ArchiveResult<()> {
    match rename_noreplace(payload, destination) {
        Ok(()) => return Ok(()),
        Err(RenameError::DestinationExists) => {}
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    if !fs::symlink_metadata(destination)?.is_dir() {
        return Err(ArchiveError::DestinationExists(destination.to_path_buf()));
    }

    // Plan: (staged, target) for every subtree absent from the destination.
    let mut moves = Vec::new();
    let mut pending = vec![(payload.to_path_buf(), destination.to_path_buf())];
    while let Some((staged_dir, target_dir)) = pending.pop() {
        for entry in fs::read_dir(&staged_dir)? {
            let entry = entry?;
            let staged = entry.path();
            let target = target_dir.join(entry.file_name());
            match fs::symlink_metadata(&target) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    moves.push((staged, target));
                }
                Ok(existing) if existing.is_dir() && entry.file_type()?.is_dir() => {
                    pending.push((staged, target));
                }
                Ok(_) => {
                    return Err(ArchiveError::Io(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("archive entry conflicts with '{}'", target.display()),
                    )));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    let mut published: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(moves.len());
    for (staged, target) in moves {
        if let Err(error) = rename_noreplace(&staged, &target) {
            let left_behind: Vec<PathBuf> = published
                .iter()
                .rev()
                .filter(|(staged, target)| rename_noreplace(target, staged).is_err())
                .map(|(_, target)| target.clone())
                .collect();
            let error = ArchiveError::Io(error.into());
            return Err(if left_behind.is_empty() {
                error
            } else {
                ArchiveError::ExtractionRollback {
                    destination: destination.to_path_buf(),
                    source: Box::new(error),
                    left_behind,
                }
            });
        }
        published.push((staged, target));
    }
    Ok(())
}

/// Reader wrapper that turns cancellation into an I/O error at the next read.
struct ArchiveInput<R> {
    reader: R,
    cancel: CancellationToken,
}

impl<R> ArchiveInput<R> {
    fn new(reader: R, cancel: &CancellationToken) -> Self {
        Self {
            reader,
            cancel: cancel.clone(),
        }
    }

    fn check(&self) -> std::io::Result<()> {
        if self.cancel.is_cancelled() {
            Err(std::io::Error::other("archive extraction cancelled"))
        } else {
            Ok(())
        }
    }
}

impl<R: Read> Read for ArchiveInput<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        self.reader.read(bytes)
    }
}

impl<R: std::io::Seek> std::io::Seek for ArchiveInput<R> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.check()?;
        self.reader.seek(position)
    }
}

/// Open once so path replacement cannot switch inputs between validation and
/// decoding. Symlinks to regular archives are followed; FIFOs never block.
fn open_archive_input(path: &Path) -> ArchiveResult<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::NotFound(path.to_path_buf())
        } else {
            ArchiveError::Io(error)
        }
    })?;
    if !file.metadata()?.is_file() {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Archive input must be a regular file",
        )));
    }
    Ok(file)
}

fn check_cancel(cancel: &CancellationToken) -> ArchiveResult<()> {
    if cancel.is_cancelled() {
        Err(ArchiveError::Cancelled)
    } else {
        Ok(())
    }
}
