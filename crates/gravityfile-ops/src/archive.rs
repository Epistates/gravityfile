//! Archive operations (extract and compress).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

mod parser_budget;

/// Cumulative reads during ZIP metadata parsing, including retries and seeks.
/// Does not bound allocations made from advertised counts before reader I/O.
const MAX_ZIP_METADATA_READ_BYTES: u64 = 64 * 1024 * 1024;

// Includes headers, extension bodies, sparse maps and skipped entry padding.
const MAX_TAR_METADATA_READ_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TAR_ENTRY_METADATA_READ_BYTES: u64 = 1024 * 1024;

/// Maximum decompression ratio allowed (1000:1).
/// Archives with higher ratios are considered potential zip bombs.
const MAX_DECOMPRESSION_RATIO: f64 = 1000.0;

/// Maximum total size for extracted content (10 GB).
const MAX_TOTAL_EXTRACTED_SIZE: u64 = 10 * 1024 * 1024 * 1024;

/// Maximum number of entries allowed in a TAR archive.
const MAX_ENTRY_COUNT: u64 = 100_000;

/// Maximum size for a single TAR entry (10 GB).
const MAX_SINGLE_ENTRY_SIZE: u64 = 10 * 1024 * 1024 * 1024;

/// Maximum memory the XZ decoder may allocate for its filter chain/dictionary.
const MAX_XZ_DECODER_MEMORY: u64 = 256 * 1024 * 1024;

/// Maximum length of a symlink target read out of an archive.
///
/// A ZIP symlink entry stores its target as the entry's *content*, so a crafted
/// archive can declare a symlink whose body is gigabytes long. That body is
/// read into memory to be interpreted as a path, so it is capped well above any
/// real `PATH_MAX` but far below what would exhaust memory.
const MAX_SYMLINK_TARGET_BYTES: u64 = 8 * 1024;

/// Error that can occur during archive operations.
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

    /// The complete archive is visible, but its directory entry may not be durable.
    #[error(
        "Archive published at {destination}, but directory synchronization failed: {source}; durability is uncertain; inspect the archive before retrying"
    )]
    PublishedDurability {
        destination: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Publication failed and some installed paths could not be removed.
    #[error("Archive extraction failed: {source}; rollback failed for {remaining_count} paths; inspect {destination} before retrying", remaining_count = .cleanup_errors.len())]
    ExtractionRollback {
        destination: PathBuf,
        #[source]
        source: Box<ArchiveError>,
        cleanup_errors: Vec<(PathBuf, std::io::Error)>,
    },
    /// Interrupted or uncertain extraction retains a durable publication journal.
    #[error(
        "Extraction outcome requires inspection; retained journal {transaction}: {source}; inspect before retrying"
    )]
    ExtractionRecovery {
        transaction: PathBuf,
        #[source]
        source: Box<ArchiveError>,
    },

    #[error("Potential decompression bomb detected: {0}")]
    DecompressionBomb(String),
}

/// Result of an archive operation.
pub type ArchiveResult<T> = Result<T, ArchiveError>;

/// Supported archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
}

impl ArchiveFormat {
    /// Detect format from file extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_lowercase();

        if name.ends_with(".zip") {
            Some(Self::Zip)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
            Some(Self::TarBz2)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some(Self::TarXz)
        } else if name.ends_with(".tar") {
            Some(Self::Tar)
        } else {
            None
        }
    }

    /// Get default extension for format.
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Zip => ".zip",
            Self::Tar => ".tar",
            Self::TarGz => ".tar.gz",
            Self::TarBz2 => ".tar.bz2",
            Self::TarXz => ".tar.xz",
        }
    }
}

/// Extract an archive to a destination directory.
///
/// Automatically detects the archive format from the file extension and extracts
/// all contents to the specified destination directory.
///
/// # Arguments
///
/// * `archive_path` - Path to the archive file to extract
/// * `destination` - Directory where contents will be extracted
///
/// # Returns
///
/// A vector of paths to all extracted files and directories.
///
/// # Errors
///
/// Returns an error if:
/// - The archive file doesn't exist ([`ArchiveError::NotFound`])
/// - The format is not supported ([`ArchiveError::UnsupportedFormat`])
/// - The archive is corrupted or malformed
/// - A path traversal attack is detected ([`ArchiveError::Io`])
/// - A decompression bomb is detected ([`ArchiveError::DecompressionBomb`])
/// - Publication fails and cleanup is incomplete ([`ArchiveError::ExtractionRollback`]);
///   inspect the destination before retrying. The error retains failed cleanup paths.
///
/// # Security
///
/// This function includes multiple protections against malicious archives:
/// - Path traversal prevention (rejects `..` and absolute paths)
/// - Symlink attack mitigation (validates canonical paths)
/// - Decompression bomb detection (ratio and size limits)
/// - Permission sanitization (strips setuid/setgid bits on Unix)
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use gravityfile_ops::extract_archive;
///
/// let extracted = extract_archive(
///     Path::new("archive.zip"),
///     Path::new("/tmp/extracted"),
/// )?;
/// println!("Extracted {} files", extracted.len());
/// # Ok::<(), gravityfile_ops::ArchiveError>(())
/// ```
pub fn extract_archive(archive_path: &Path, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    extract_archive_cancellable(archive_path, destination, &CancellationToken::new())
}

/// Extract with cooperative cancellation during decoding and merge preflight.
/// Once publication starts, its existing completion/rollback path runs to the end.
pub fn extract_archive_cancellable(
    archive_path: &Path,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    extract_archive_impl(archive_path, destination, cancel, || {})
}

fn extract_archive_impl(
    archive_path: &Path,
    destination: &Path,
    cancel: &CancellationToken,
    after_decode: impl FnOnce(),
) -> ArchiveResult<Vec<PathBuf>> {
    check_archive_cancel(cancel)?;
    let archive_file = open_archive_input(archive_path)?;

    let format = ArchiveFormat::from_path(archive_path).ok_or_else(|| {
        ArchiveError::UnsupportedFormat(
            archive_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_string(),
        )
    })?;

    if let Ok(metadata) = std::fs::symlink_metadata(destination)
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "extraction destination is not a directory",
        )));
    }

    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let destination = if std::fs::symlink_metadata(destination).is_ok() {
        destination.canonicalize()?
    } else {
        parent.canonicalize()?.join(
            destination
                .file_name()
                .ok_or_else(|| std::io::Error::other("extraction destination has no filename"))?,
        )
    };
    let publication_parent = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extraction into a filesystem root is unsupported",
        )
    })?;
    let staging_directory =
        crate::staging::private_directory(publication_parent, ".gravityfile-extract-")?;
    let staging = staging_directory.path().to_path_buf();

    let extraction = match format {
        ArchiveFormat::Zip => extract_zip(archive_file, &staging, cancel),
        ArchiveFormat::Tar => extract_tar(archive_file, &staging, cancel),
        ArchiveFormat::TarGz => extract_tar_gz(archive_file, &staging, cancel),
        ArchiveFormat::TarBz2 => extract_tar_bz2(archive_file, &staging, cancel),
        ArchiveFormat::TarXz => extract_tar_xz(archive_file, &staging, cancel),
    };
    if let Err(error) = extraction {
        let _ = std::fs::remove_dir_all(&staging);
        return if cancel.is_cancelled() {
            Err(ArchiveError::Cancelled)
        } else {
            Err(error)
        };
    }

    after_decode();
    // Publication owns staging from here: a journal/error must survive Drop.
    let _ = staging_directory.keep();
    let result = publish_extraction(&staging, &destination, cancel);
    if matches!(result, Err(ArchiveError::ExtractionRecovery { .. })) {
        return result;
    }
    if let Err(cleanup_error) = std::fs::remove_dir_all(&staging)
        && result.is_ok()
    {
        tracing::warn!(
            path = %staging.display(),
            error = %cleanup_error,
            "Archive extraction succeeded but its staging directory could not be removed"
        );
    }
    result
}

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
/// decoding. Symlinks to regular archives are supported; Unix FIFOs never block
/// waiting for a writer. Concurrent changes to file contents are not isolated.
fn open_archive_input(path: &Path) -> ArchiveResult<File> {
    let mut options = std::fs::OpenOptions::new();
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

/// Publish an extraction staged on the destination filesystem without ever
/// replacing an existing path. If a later step fails, attempt to remove all newly
/// installed entries and report any incomplete rollback.
fn publish_extraction(
    staging: &Path,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    publish_extraction_observed(
        staging,
        destination,
        cancel,
        install_extracted_leaf,
        remove_extracted_path,
        true,
        |_, _, _| {},
    )
}

// Rollback must never recursively delete a path that was installed as a leaf:
// another writer may have replaced it with a directory containing unrelated data.
fn remove_extracted_path(path: &Path, directory: bool) -> std::io::Result<()> {
    let result = if directory {
        std::fs::remove_dir(path)
    } else {
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTypeExt;
            if std::fs::symlink_metadata(path)?
                .file_type()
                .is_symlink_dir()
            {
                return std::fs::remove_dir(path);
            }
        }
        std::fs::remove_file(path)
    };
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn install_extracted_leaf(source: &Path, target: &Path) -> ArchiveResult<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        crate::copy::rename_noreplace(source, target)
            .map_err(|error| ArchiveError::Io(std::io::Error::other(error)))?;
    } else {
        // Sibling staging permits atomic no-clobber publication with metadata retained.
        std::fs::hard_link(source, target)?;
    }
    Ok(())
}

/// Identity evidence for best-effort rollback. This detects substitutions before
/// the check, but cannot make the following path-based unlink race-free.
struct ExtractedEntry {
    path: PathBuf,
    identity: Option<crate::ObjectIdentity>,
}

impl ExtractedEntry {
    fn capture(path: &Path, identity_path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            identity: crate::ObjectIdentity::capture(identity_path).ok(),
        }
    }

    fn remove_if_unchanged(
        &self,
        directory: bool,
        remove: &mut impl FnMut(&Path, bool) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        {
            let current = match crate::ObjectIdentity::capture(&self.path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            if self.identity.as_ref() != Some(&current) {
                return Err(std::io::Error::other(
                    "extracted path identity changed or could not be recorded; refusing rollback removal",
                ));
            }
        }
        remove(&self.path, directory)
    }
}

#[cfg(test)]
fn publish_extraction_with_io(
    staging: &Path,
    destination: &Path,
    cancel: &CancellationToken,
    install: impl FnMut(&Path, &Path) -> ArchiveResult<()>,
    remove: impl FnMut(&Path, bool) -> std::io::Result<()>,
) -> ArchiveResult<Vec<PathBuf>> {
    publish_extraction_observed(
        staging,
        destination,
        cancel,
        install,
        remove,
        false,
        |_, _, _| {},
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_extraction_observed(
    staging: &Path,
    destination: &Path,
    cancel: &CancellationToken,
    mut install: impl FnMut(&Path, &Path) -> ArchiveResult<()>,
    mut remove: impl FnMut(&Path, bool) -> std::io::Result<()>,
    recorded: bool,
    observer: fn(&Path, usize, &str),
) -> ArchiveResult<Vec<PathBuf>> {
    let mut directories = Vec::<(PathBuf, PathBuf)>::new();
    let mut leaves = Vec::<(PathBuf, PathBuf)>::new();
    let mut pending = vec![staging.to_path_buf()];

    while let Some(directory) = pending.pop() {
        check_archive_cancel(cancel)?;
        for entry in std::fs::read_dir(&directory)? {
            check_archive_cancel(cancel)?;
            let entry = entry?;
            let source = entry.path();
            let relative = source.strip_prefix(staging).map_err(|_| {
                ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "staged archive path escaped its root",
                ))
            })?;
            let target = destination.join(relative);
            let metadata = std::fs::symlink_metadata(&source)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                directories.push((source.clone(), target));
                pending.push(source);
            } else if metadata.is_file() || metadata.file_type().is_symlink() {
                leaves.push((source, target));
            } else {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "archive contained an unsupported file type",
                )));
            }
        }
    }

    directories.sort_by_key(|(source, _)| source.components().count());

    // Preflight the complete merge before changing the destination.
    for (_, target) in &directories {
        check_archive_cancel(cancel)?;
        match std::fs::symlink_metadata(target) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("archive path conflicts with '{}'", target.display()),
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for (_, target) in &leaves {
        check_archive_cancel(cancel)?;
        match std::fs::symlink_metadata(target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("archive path conflicts with '{}'", target.display()),
                )));
            }
            Err(error) => return Err(error.into()),
        }
    }

    let mut created_directories = Vec::new();
    let mut created_directory_permissions = Vec::new();
    let mut installed_leaves = Vec::new();
    let mut published = Vec::new();

    // Last cancellation boundary: publication and rollback must run to completion.
    check_archive_cancel(cancel)?;
    let journal = if recorded {
        Some(crate::archive_journal::ArchiveJournal::prepare(
            staging,
            destination,
            &directories,
            &leaves,
        )?)
    } else {
        None
    };
    let destination_existed = std::fs::symlink_metadata(destination).is_ok();

    let publish_result: ArchiveResult<()> = (|| {
        if let Some(journal) = &journal {
            if let Some(index) = journal.before(destination)? {
                observer(&journal.path, index, "pending");
                journal.directory(destination)?;
                created_directories.push(ExtractedEntry::capture(destination, destination));
                observer(&journal.path, index, "published");
                journal.after(index)?;
                observer(&journal.path, index, "done");
            }
        } else if !destination_existed {
            std::fs::create_dir(destination)?;
            created_directories.push(ExtractedEntry::capture(destination, destination));
        }
        for (source, target) in &directories {
            let progress = journal
                .as_ref()
                .map(|j| j.before(target))
                .transpose()?
                .flatten();
            if let (Some(journal), Some(index)) = (&journal, progress) {
                observer(&journal.path, index, "pending");
            }
            let create = if let Some(journal) = &journal {
                journal.directory(target)
            } else {
                std::fs::create_dir(target)
            };
            match create {
                Ok(()) => {
                    if journal.is_none() || progress.is_some() {
                        if let Ok(permissions) = std::fs::metadata(source).map(|m| m.permissions())
                        {
                            created_directory_permissions.push((target.clone(), permissions));
                        }
                        created_directories.push(ExtractedEntry::capture(target, target));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = std::fs::symlink_metadata(target)?;
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err(error.into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
            if let (Some(journal), Some(index)) = (&journal, progress) {
                observer(&journal.path, index, "published");
                journal.after(index)?;
                observer(&journal.path, index, "done");
            }
            published.push(target.clone());
        }

        for (source, target) in &leaves {
            let progress = journal
                .as_ref()
                .map(|j| j.before(target))
                .transpose()?
                .flatten();
            if let (Some(journal), Some(index)) = (&journal, progress) {
                observer(&journal.path, index, "pending");
            }
            // A regular file is installed as a hard link, so its staged inode
            // is stronger evidence than inspecting a replaceable destination.
            let regular = std::fs::symlink_metadata(source)?.is_file();
            let entry = regular.then(|| ExtractedEntry::capture(target, source));
            install(source, target)?;
            installed_leaves.push(entry.unwrap_or_else(|| ExtractedEntry::capture(target, target)));
            published.push(target.clone());
            if let (Some(journal), Some(index)) = (&journal, progress) {
                observer(&journal.path, index, "published");
                journal.after(index)?;
                observer(&journal.path, index, "done");
            }
        }

        // Apply directory modes only after their contents are installed; an
        // archive may legitimately contain a read-only directory.
        for (target, permissions) in created_directory_permissions.iter().rev() {
            if let Err(error) = std::fs::set_permissions(target, permissions.clone()) {
                tracing::warn!(
                    path = %target.display(),
                    error = %error,
                    "Could not restore extracted directory permissions"
                );
            }
        }
        Ok(())
    })();

    if let Err(error) = publish_result {
        let mut cleanup_errors = Vec::new();
        for (entry, directory) in installed_leaves
            .iter()
            .rev()
            .map(|p| (p, false))
            .chain(created_directories.iter().rev().map(|p| (p, true)))
        {
            let cleanup = if let Some(journal) = &journal {
                journal.retain_entry(&entry.path)
            } else {
                entry.remove_if_unchanged(directory, &mut remove)
            };
            if let Err(cleanup_error) = cleanup {
                cleanup_errors.push((entry.path.clone(), cleanup_error));
            }
        }
        let error = if !cleanup_errors.is_empty() {
            ArchiveError::ExtractionRollback {
                destination: destination.to_path_buf(),
                source: Box::new(error),
                cleanup_errors,
            }
        } else {
            error
        };
        if let Some(journal) = journal {
            if !matches!(error, ArchiveError::ExtractionRollback { .. }) {
                let _ = journal.finish(false);
            }
            return Err(ArchiveError::ExtractionRecovery {
                transaction: journal.path.clone(),
                source: Box::new(error),
            });
        }
        return Err(error);
    }

    if let Some(journal) = journal {
        if let Err(error) = journal.finish(true) {
            return Err(ArchiveError::ExtractionRecovery {
                transaction: journal.path.clone(),
                source: Box::new(error.into()),
            });
        }
        journal.clean_committed();
    }

    Ok(published)
}

/// Copy from `reader` to `writer`, enforcing the per-entry and cumulative
/// extraction-size limits against the *actual* bytes written rather than the
/// attacker-declared header size. Returns the number of bytes written.
///
/// This is the ZIP counterpart to the TAR path's post-unpack byte accounting:
/// the `zip` crate's reader stops at the end of the compressed stream, which a
/// crafted archive can make far larger than the declared uncompressed size.
fn copy_with_limit<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    already_extracted: u64,
    entry_name: &str,
) -> ArchiveResult<u64> {
    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        written = written.saturating_add(n as u64);
        if written > MAX_SINGLE_ENTRY_SIZE {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Entry '{}' extracted past the maximum size of {} bytes",
                entry_name, MAX_SINGLE_ENTRY_SIZE
            )));
        }
        if already_extracted.saturating_add(written) > MAX_TOTAL_EXTRACTED_SIZE {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Archive exceeded maximum total extraction size of {} bytes",
                MAX_TOTAL_EXTRACTED_SIZE
            )));
        }
        writer.write_all(&buf[..n])?;
    }
    Ok(written)
}

/// Extract a ZIP archive.
///
/// # Security
/// This function validates all paths to prevent directory traversal attacks.
/// Paths containing `..` components or absolute paths are rejected.
/// Setuid/setgid bits are stripped from extracted file permissions.
/// Decompression bombs are detected via ratio and size limits, and the actual
/// bytes written per entry and in total are capped during extraction.
fn extract_zip(
    file: File,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    extract_zip_with_metadata_budget(file, destination, MAX_ZIP_METADATA_READ_BYTES, cancel)
}

fn extract_zip_with_metadata_budget(
    file: File,
    destination: &Path,
    metadata_read_bytes: u64,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    let file = ArchiveInput::new(file, cancel);
    let budget = parser_budget::MetadataBudget::new(metadata_read_bytes);
    let parsed = zip::ZipArchive::new(budget.wrap(file));
    // The ZIP parser can retry earlier directory records and return an earlier
    // error. Preserve the resource-limit diagnosis even in that case.
    if budget.exhausted() {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP metadata parsing exceeds read budget of {metadata_read_bytes} bytes"
        )));
    }
    let mut archive = parsed?;
    budget.finish_metadata();
    let mut extracted_files = Vec::new();

    // MED-3: canonicalization is required — return error on failure
    let canonical_dest = destination.canonicalize().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Failed to canonicalize destination '{}': {}",
                destination.display(),
                e
            ),
        )
    })?;

    // Bound metadata traversal before walking entries. ZipArchive has already
    // parsed its central directory; this is not a central-directory memory cap.
    if archive.len() as u64 > MAX_ENTRY_COUNT {
        return Err(ArchiveError::DecompressionBomb(format!(
            "Archive exceeds maximum entry count of {}",
            MAX_ENTRY_COUNT
        )));
    }
    let mut total_uncompressed = 0;
    for i in 0..archive.len() {
        check_archive_cancel(cancel)?;
        let entry = archive.by_index_raw(i)?;
        total_uncompressed = validate_zip_sizes(
            entry.compressed_size(),
            entry.size(),
            total_uncompressed,
            entry.name(),
        )?;
    }

    // Cumulative bytes actually written to disk, checked against the declared
    // pre-scan totals above as defense in depth.
    let mut total_extracted_bytes: u64 = 0;

    // MED-1: Extract in two passes — regular files and directories first,
    // symlinks last. This prevents a previously-extracted symlink from
    // redirecting `create_dir_all` outside the extraction root.
    let mut symlink_indices = Vec::new();

    for i in 0..archive.len() {
        check_archive_cancel(cancel)?;
        let mut entry = archive.by_index(i)?;
        let entry_path = entry.enclosed_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid file path in archive",
            )
        })?;

        // Security: Reject absolute paths
        if entry_path.is_absolute() {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Absolute path in archive: {}", entry_path.display()),
            )));
        }

        // Security: Reject paths with parent directory components
        if entry_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Path traversal attempt in archive: {}",
                    entry_path.display()
                ),
            )));
        }

        if skip_archive_root(&entry_path, entry.is_dir())? {
            continue;
        }

        // Defer symlinks to the second pass.
        if entry.is_symlink() {
            symlink_indices.push(i);
            continue;
        }

        let outpath = destination.join(&entry_path);

        // Security: Double-check that resolved path stays within destination
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
            let canonical_out = parent.canonicalize().map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Failed to canonicalize output path '{}': {}",
                        parent.display(),
                        e
                    ),
                )
            })?;
            if !canonical_out.starts_with(&canonical_dest) {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Path escapes destination: {}", entry_path.display()),
                )));
            }
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            // Regular file. Bound the copy by actual bytes written so a crafted
            // entry whose declared size lies can't write past the limits.
            let entry_label = entry_path.display().to_string();
            let mut outfile = File::create(&outpath)?;
            match copy_with_limit(
                &mut ArchiveInput::new(&mut entry, cancel),
                &mut outfile,
                total_extracted_bytes,
                &entry_label,
            ) {
                Ok(written) => {
                    total_extracted_bytes = total_extracted_bytes.saturating_add(written);
                }
                Err(e) => {
                    drop(outfile);
                    let _ = std::fs::remove_file(&outpath);
                    return Err(e);
                }
            }

            // Set permissions on Unix - strip setuid/setgid bits for security
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    let safe_mode = mode & 0o777;
                    std::fs::set_permissions(&outpath, std::fs::Permissions::from_mode(safe_mode))?;
                }
            }
        }

        extracted_files.push(outpath);
    }

    // Second pass: extract symlinks after all regular entries are in place.
    for i in symlink_indices {
        check_archive_cancel(cancel)?;
        let mut entry = archive.by_index(i)?;
        let entry_path = entry.enclosed_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid file path in archive",
            )
        })?;

        let outpath = destination.join(&entry_path);

        // Read the symlink target from the file content, bounded so a bogus
        // oversized entry cannot exhaust memory.
        let mut target_bytes = Vec::new();
        let read = std::io::copy(
            &mut (&mut entry).take(MAX_SYMLINK_TARGET_BYTES + 1),
            &mut target_bytes,
        )?;
        if read > MAX_SYMLINK_TARGET_BYTES {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Symlink entry '{}' declares a target longer than {} bytes",
                    entry_path.display(),
                    MAX_SYMLINK_TARGET_BYTES
                ),
            )));
        }
        let link_target = zip_symlink_target(&target_bytes)?;

        // Reject absolute symlink targets
        if link_target.is_absolute() {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Symlink with absolute target rejected: {} -> {}",
                    entry_path.display(),
                    link_target.display()
                ),
            )));
        }

        // Validate target does not escape destination (always, not only
        // when ParentDir is present — guards against chained symlinks).
        let symlink_parent = outpath.parent().unwrap_or(destination);
        let canonical_parent = symlink_parent
            .canonicalize()
            .unwrap_or_else(|_| symlink_parent.to_path_buf());
        let resolved = canonical_parent.join(link_target);
        let normalized = resolve_path(&resolved);

        if !normalized.starts_with(&canonical_dest) {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Symlink escapes destination: {} -> {}",
                    entry_path.display(),
                    link_target.display()
                ),
            )));
        }

        // Create the symlink
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(link_target, &outpath)?;
        }
        #[cfg(windows)]
        {
            tracing::warn!(
                "Skipping symlink {} -> {} (Windows symlinks not supported)",
                entry_path.display(),
                link_target.display()
            );
        }

        extracted_files.push(outpath);
    }

    Ok(extracted_files)
}

fn zip_symlink_target(bytes: &[u8]) -> std::io::Result<&Path> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ZIP symlink target is not UTF-8",
        )
    })?;
    if text.is_empty() || text.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ZIP symlink target is empty or contains NUL",
        ));
    }
    // Whitespace is part of a filename, not padding in a ZIP symlink payload.
    Ok(Path::new(text))
}

/// Check declared sizes before invoking a ZIP entry decoder.
fn validate_zip_sizes(
    compressed: u64,
    uncompressed: u64,
    total: u64,
    name: &str,
) -> ArchiveResult<u64> {
    if uncompressed > MAX_SINGLE_ENTRY_SIZE
        || uncompressed > MAX_TOTAL_EXTRACTED_SIZE.saturating_sub(total)
    {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP entry '{name}' exceeds the per-entry or remaining extraction budget"
        )));
    }
    if compressed == 0 && uncompressed > 0 {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP entry '{name}' declares nonempty output with zero compressed bytes"
        )));
    }
    if compressed > 0 && uncompressed as f64 / compressed as f64 > MAX_DECOMPRESSION_RATIO {
        return Err(ArchiveError::DecompressionBomb(format!(
            "ZIP entry '{name}' exceeds the maximum decompression ratio of {MAX_DECOMPRESSION_RATIO}:1"
        )));
    }
    Ok(total + uncompressed)
}

/// Extract a plain TAR archive.
fn extract_tar(
    file: File,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    extract_complete_tar(ArchiveInput::new(file, cancel), destination)
}

/// Extract a TAR.GZ archive.
fn extract_tar_gz(
    file: File,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    let decoder = flate2::read::MultiGzDecoder::new(file);
    extract_complete_tar(ArchiveInput::new(decoder, cancel), destination)
}

/// Extract a TAR.BZ2 archive.
fn extract_tar_bz2(
    file: File,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    let decoder = bzip2::read::MultiBzDecoder::new(file);
    extract_complete_tar(ArchiveInput::new(decoder, cancel), destination)
}

/// Extract a TAR.XZ archive.
fn extract_tar_xz(
    file: File,
    destination: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<Vec<PathBuf>> {
    let stream =
        xz2::stream::Stream::new_stream_decoder(MAX_XZ_DECODER_MEMORY, xz2::stream::CONCATENATED)
            .map_err(std::io::Error::other)?;
    let decoder = xz2::read::XzDecoder::new_stream(file, stream);
    extract_complete_tar(ArchiveInput::new(decoder, cancel), destination)
}

/// Finish decoding after TAR's end marker so compression checksums/trailers
/// are verified before staged files can be published. Permit bounded zero
/// padding; do not silently ignore additional payload after the TAR stream.
fn extract_complete_tar<R: Read>(mut reader: R, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let extracted = extract_tar_from_reader(&mut reader, destination)?;
    const MAX_PADDING: usize = 1024 * 1024;
    let mut padding = 0usize;
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(extracted);
        }
        padding += count;
        if padding > MAX_PADDING {
            return Err(ArchiveError::DecompressionBomb(
                "TAR trailing padding exceeds 1 MiB".into(),
            ));
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Nonzero data follows the TAR end marker",
            )));
        }
    }
}

/// Resolve `..` and `.` in a path while preserving root/prefix components.
/// Used for symlink target validation where the resolved path is absolute.
fn resolve_path(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other);
            }
        }
    }
    resolved
}

/// Validate that a TAR symlink target does not escape the destination.
///
/// Returns `Err` if the target is absolute (including Windows Prefix), or if normalizing
/// the resolved path places it outside `canonical_dest`.
fn validate_tar_symlink_target(
    link_target: &Path,
    outpath: &Path,
    destination: &Path,
    canonical_dest: &Path,
) -> ArchiveResult<()> {
    // MED-4: Reject Prefix (Windows absolute path) as absolute
    for component in link_target.components() {
        if matches!(component, Component::Prefix(_)) {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Symlink with absolute (prefixed) target rejected: {}",
                    link_target.display()
                ),
            )));
        }
    }

    // Reject standard absolute symlink targets
    if link_target.is_absolute() {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Symlink with absolute target rejected: {}",
                link_target.display()
            ),
        )));
    }

    // Always validate the resolved target against canonical_dest,
    // not only when ParentDir is present. A target like `safe_subdir`
    // could itself be a previously-extracted symlink pointing outside
    // the root (chained symlink attack / TOCTOU zip-slip variant).
    //
    // Canonicalize the parent directory (which must already exist) to
    // resolve any filesystem-level symlinks (e.g. /var -> /private/var
    // on macOS) so the starts_with check against canonical_dest works.
    let symlink_parent = outpath.parent().unwrap_or(destination);
    let canonical_parent = symlink_parent
        .canonicalize()
        .unwrap_or_else(|_| symlink_parent.to_path_buf());
    let resolved = canonical_parent.join(link_target);
    let normalized = resolve_path(&resolved);

    if !normalized.starts_with(canonical_dest) {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Symlink escapes destination: {}", link_target.display()),
        )));
    }

    Ok(())
}

/// TAR hard links use archive-root-relative names, unlike symbolic links.
/// Only already extracted regular files are accepted as targets.
fn validate_tar_hardlink_target(target: &Path, root: &Path) -> ArchiveResult<()> {
    if target.is_absolute()
        || target
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "TAR hard-link target must be relative to the archive root without parent traversal",
        )));
    }
    let path = root.join(target);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || !path.canonicalize()?.starts_with(root) {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "TAR hard-link target must be an extracted regular file inside the archive root",
        )));
    }
    Ok(())
}

/// The staging root belongs to the host. Archive root-directory headers are
/// common (`tar ... .`) but must not alter its permissions or other metadata.
fn skip_archive_root(path: &Path, is_directory: bool) -> ArchiveResult<bool> {
    let is_root = path
        .components()
        .all(|component| matches!(component, Component::CurDir));
    if is_root && !is_directory {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Archive entry resolves to the extraction root but is not a directory",
        )));
    }
    Ok(is_root)
}

/// Extract a TAR archive from a reader.
///
/// # Security
/// This function validates all paths to prevent directory traversal attacks.
/// Paths containing `..` components or absolute paths are rejected.
///
/// # Bomb protection (CRIT-4)
/// - Checks effective entry size (including sparse extents/PAX overrides) before unpacking.
/// - Maintains a cumulative byte counter against `MAX_TOTAL_EXTRACTED_SIZE`.
/// - Limits entry count to `MAX_ENTRY_COUNT`.
fn extract_tar_from_reader<R: Read>(reader: R, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    extract_tar_with_metadata_budget(
        reader,
        destination,
        MAX_TAR_METADATA_READ_BYTES,
        MAX_TAR_ENTRY_METADATA_READ_BYTES,
    )
}

fn extract_tar_with_metadata_budget<R: Read>(
    reader: R,
    destination: &Path,
    total_budget: u64,
    entry_budget: u64,
) -> ArchiveResult<Vec<PathBuf>> {
    let budget = parser_budget::MetadataBudget::new(total_budget);
    let mut archive = tar::Archive::new(budget.wrap(reader));
    // Security: Don't preserve setuid/setgid bits
    archive.set_preserve_permissions(false);
    // Security: Don't restore extended attributes
    archive.set_unpack_xattrs(false);

    let mut extracted_files = Vec::new();

    // MED-3: canonicalization is required — return error on failure
    let canonical_dest = destination.canonicalize().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Failed to canonicalize destination '{}': {}",
                destination.display(),
                e
            ),
        )
    })?;

    // CRIT-4: bomb protection counters
    let mut entry_count: u64 = 0;
    let mut total_extracted_bytes: u64 = 0;

    let mut entries = archive.entries()?;
    loop {
        budget.resume_metadata(entry_budget);
        let next = entries.next();
        if budget.exhausted() {
            return Err(ArchiveError::DecompressionBomb(format!(
                "TAR metadata exceeds cumulative {total_budget}-byte or per-entry {entry_budget}-byte read budget"
            )));
        }
        budget.finish_metadata();
        let Some(entry_result) = next else { break };
        let mut entry = entry_result?;

        // CRIT-4: enforce entry count limit
        entry_count += 1;
        if entry_count > MAX_ENTRY_COUNT {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Archive exceeds maximum entry count of {}",
                MAX_ENTRY_COUNT
            )));
        }

        let entry_path = entry.path()?.into_owned();

        // Security: Reject absolute paths
        if entry_path.is_absolute() {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Absolute path in archive: {}", entry_path.display()),
            )));
        }

        // Security: Reject paths with parent directory components
        if entry_path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Path traversal attempt in archive: {}",
                    entry_path.display()
                ),
            )));
        }

        if skip_archive_root(&entry_path, entry.header().entry_type().is_dir())? {
            continue;
        }

        let entry_type = entry.header().entry_type();
        if entry_type.is_fifo()
            || entry_type.is_character_special()
            || entry_type.is_block_special()
        {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Unsupported special-file TAR entry: {}",
                    entry_path.display()
                ),
            )));
        }

        // The parser's effective size includes GNU sparse logical extents and
        // PAX overrides. The raw header size alone can understate allocation.
        let declared_size = entry.size();
        if declared_size > MAX_SINGLE_ENTRY_SIZE
            || declared_size > MAX_TOTAL_EXTRACTED_SIZE.saturating_sub(total_extracted_bytes)
        {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Entry '{}' effective size {} exceeds the per-entry or remaining extraction budget",
                entry_path.display(),
                declared_size,
            )));
        }

        let outpath = destination.join(&entry_path);

        // Security: Double-check that resolved path stays within destination
        // (handles symlink attacks where intermediate directories are symlinks)
        if let Some(parent) = outpath.parent() {
            std::fs::create_dir_all(parent)?;
            let canonical_out = parent.canonicalize().map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "Failed to canonicalize output path '{}': {}",
                        parent.display(),
                        e
                    ),
                )
            })?;
            if !canonical_out.starts_with(&canonical_dest) {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Path escapes destination: {}", entry_path.display()),
                )));
            }
        }

        // Security: Validate symlink targets (MED-4 normalizer used).
        // A symlink/hardlink entry whose link name can't be read is rejected
        // rather than unpacked — otherwise a malformed header would bypass the
        // escape check yet still create the link.
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            match entry.link_name() {
                Ok(Some(link_target)) => {
                    if entry_type.is_hard_link() {
                        validate_tar_hardlink_target(&link_target, &canonical_dest)?;
                    } else {
                        validate_tar_symlink_target(
                            &link_target,
                            &outpath,
                            destination,
                            &canonical_dest,
                        )?;
                    }
                }
                _ => {
                    return Err(ArchiveError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "Link entry '{}' has an unreadable or missing target",
                            entry_path.display()
                        ),
                    )));
                }
            }
        }

        // `unpack(path)` resolves hard-link targets relative to the process
        // cwd. `unpack_in` supplies the archive root and validates containment.
        if !entry.unpack_in(destination)? {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "TAR entry could not be safely unpacked: {}",
                    entry_path.display()
                ),
            )));
        }

        // CRIT-2 fix: count *actual* bytes written to disk rather than
        // trusting the attacker-controlled declared header size. A crafted
        // archive can set all header sizes to 0 while encoding arbitrarily
        // large content, bypassing the pre-flight check above.
        let actual_size = std::fs::symlink_metadata(&outpath)
            .map(|m| m.len())
            .unwrap_or(0);

        if actual_size > MAX_SINGLE_ENTRY_SIZE {
            // Best-effort cleanup of the oversized entry.
            let _ = std::fs::remove_file(&outpath);
            return Err(ArchiveError::DecompressionBomb(format!(
                "Entry '{}' extracted to {} bytes (max {} bytes)",
                entry_path.display(),
                actual_size,
                MAX_SINGLE_ENTRY_SIZE
            )));
        }

        total_extracted_bytes = total_extracted_bytes.saturating_add(actual_size);
        if total_extracted_bytes > MAX_TOTAL_EXTRACTED_SIZE {
            // Best-effort cleanup of the entry that pushed us over.
            let _ = std::fs::remove_file(&outpath);
            return Err(ArchiveError::DecompressionBomb(format!(
                "Archive exceeded maximum total extraction size of {} bytes",
                MAX_TOTAL_EXTRACTED_SIZE
            )));
        }

        extracted_files.push(outpath);
    }

    Ok(extracted_files)
}

/// Create an archive from a list of files and/or directories.
///
/// Creates a new archive containing the specified files and directories.
/// Directories are added recursively with all their contents. On Unix the
/// resulting archive is created with owner-only mode `0600` (subject to umask).
/// Change permissions explicitly if it should be shared. Windows uses inherited ACLs.
///
/// # Arguments
///
/// * `files` - Paths to files and directories to include in the archive
/// * `archive_path` - Path where the archive will be created
/// * `format` - The archive format to use
///
/// # Errors
///
/// Returns an error if:
/// - The destination already exists ([`ArchiveError::DestinationExists`])
/// - Any source file cannot be read
/// - The archive cannot be written
/// - The archive was published, but syncing its parent failed
///   ([`ArchiveError::PublishedDurability`]); inspect it before retrying.
///
/// Parent-directory synchronization is currently a no-op on Windows.
///
/// # Example
///
/// ```no_run
/// use std::path::PathBuf;
/// use gravityfile_ops::{create_archive, ArchiveFormat};
///
/// create_archive(
///     &[PathBuf::from("src/"), PathBuf::from("Cargo.toml")],
///     &PathBuf::from("backup.tar.gz"),
///     ArchiveFormat::TarGz,
/// )?;
/// # Ok::<(), gravityfile_ops::ArchiveError>(())
/// ```
pub fn create_archive(
    files: &[PathBuf],
    archive_path: &Path,
    format: ArchiveFormat,
) -> ArchiveResult<()> {
    create_archive_with_sync(files, archive_path, format, crate::recovery::sync_directory)
}

/// Create an archive with cooperative cancellation. Cancellation before
/// publication removes private staging; after publication the archive is retained.
/// Filesystem calls and encoder internals have no hard interruption deadline.
pub fn create_archive_cancellable(
    files: &[PathBuf],
    archive_path: &Path,
    format: ArchiveFormat,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    create_archive_impl(
        files,
        archive_path,
        format,
        cancel,
        crate::recovery::sync_directory,
    )
}

fn create_archive_with_sync(
    files: &[PathBuf],
    archive_path: &Path,
    format: ArchiveFormat,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> ArchiveResult<()> {
    create_archive_impl(
        files,
        archive_path,
        format,
        &CancellationToken::new(),
        sync_parent,
    )
}

fn create_archive_impl(
    files: &[PathBuf],
    archive_path: &Path,
    format: ArchiveFormat,
    cancel: &CancellationToken,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> ArchiveResult<()> {
    check_archive_cancel(cancel)?;
    if std::fs::symlink_metadata(archive_path).is_ok() {
        return Err(ArchiveError::DestinationExists(archive_path.to_path_buf()));
    }

    let mut names = std::collections::HashSet::new();
    for source in files {
        check_archive_cancel(cancel)?;
        let name = source.file_name().ok_or_else(|| {
            ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Archive source '{}' has no filename", source.display()),
            ))
        })?;
        if !names.insert(name.to_os_string()) {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Archive sources share the entry name '{}'",
                    name.to_string_lossy()
                ),
            )));
        }
        let same_path = matches!(
            (
                crate::copy::canonicalize_with_missing_suffix(source),
                crate::copy::canonicalize_with_missing_suffix(archive_path),
            ),
            (Some(source), Some(destination)) if source == destination
        );
        if same_path || crate::copy::destination_is_within_directory(source, archive_path) {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Archive destination '{}' cannot be inside source '{}'",
                    archive_path.display(),
                    source.display()
                ),
            )));
        }
    }

    // Write to a sibling first so an encoder/read failure never leaves a
    // corrupt archive at the user-requested destination.
    let parent = archive_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging_directory = crate::staging::private_directory(parent, ".gravityfile-archive-")?;
    let staged_path = staging_directory.path().join("payload");
    let result = match format {
        ArchiveFormat::Zip => create_zip(files, &staged_path, cancel),
        ArchiveFormat::Tar => create_tar(files, &staged_path, cancel),
        ArchiveFormat::TarGz => create_tar_gz(files, &staged_path, cancel),
        ArchiveFormat::TarBz2 => create_tar_bz2(files, &staged_path, cancel),
        ArchiveFormat::TarXz => create_tar_xz(files, &staged_path, cancel),
    };

    if let Err(error) = result {
        let _ = crate::copy::remove_path(&staged_path);
        return if cancel.is_cancelled() {
            Err(ArchiveError::Cancelled)
        } else {
            Err(error)
        };
    }
    // Finalize and sync all bytes before making the archive visible. Publication
    // must remain exclusive even when hard links are unsupported.
    check_archive_cancel(cancel)?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&staged_path)?
        .sync_all()?;
    check_archive_cancel(cancel)?;
    publish_archive(&staged_path, archive_path)?;
    // Publication is complete: a subsequent persistence failure must never
    // remove the visible archive or masquerade as a pre-publication error.
    sync_parent(parent).map_err(|source| ArchiveError::PublishedDurability {
        destination: archive_path.to_owned(),
        source,
    })?;

    Ok(())
}

fn publish_archive(staged_path: &Path, archive_path: &Path) -> ArchiveResult<()> {
    use crate::copy::{RenameError, rename_noreplace};
    rename_noreplace(staged_path, archive_path).map_err(|error| match error {
        RenameError::DestinationExists => {
            ArchiveError::DestinationExists(archive_path.to_path_buf())
        }
        RenameError::Io(error) => ArchiveError::Io(error),
        RenameError::CrossDevice => ArchiveError::Io(std::io::Error::other(
            "archive staging and destination must share a filesystem",
        )),
    })
}

fn check_archive_cancel(cancel: &CancellationToken) -> ArchiveResult<()> {
    if cancel.is_cancelled() {
        Err(ArchiveError::Cancelled)
    } else {
        Ok(())
    }
}

struct ArchiveOutput {
    file: File,
    cancel: CancellationToken,
}
impl ArchiveOutput {
    fn check(&self) -> std::io::Result<()> {
        if self.cancel.is_cancelled() {
            // Interrupted would make some copy loops retry indefinitely.
            Err(std::io::Error::other("archive creation cancelled"))
        } else {
            Ok(())
        }
    }
}
impl Write for ArchiveOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.check()?;
        self.file.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.check()?;
        self.file.flush()
    }
}
impl std::io::Seek for ArchiveOutput {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.check()?;
        std::io::Seek::seek(&mut self.file, position)
    }
}

// Archives can aggregate private files. Keep the container owner-only after
// it leaves private staging; entry permissions do not protect compressed bytes.
fn create_archive_file(path: &Path, cancel: &CancellationToken) -> std::io::Result<ArchiveOutput> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(ArchiveOutput {
        file: options.open(path)?,
        cancel: cancel.clone(),
    })
}

/// Create a ZIP archive.
fn create_zip(
    files: &[PathBuf],
    archive_path: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    let file = create_archive_file(archive_path, cancel)?;
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for path in files {
        // MED-5: return error when file_name() is None
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Path '{}' has no filename component", path.display()),
                )));
            }
        };
        add_path_to_zip(&mut archive, path, &name, &options, cancel)?;
    }

    archive.finish()?;
    Ok(())
}

/// Recursively add a path to a ZIP archive.
///
/// Handles regular files, directories, and symlinks.
/// Uses a visited set to detect and prevent symlink loops.
fn add_path_to_zip<W: Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
    options: &zip::write::SimpleFileOptions,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    add_path_to_zip_with_visited(
        archive,
        path,
        name,
        options,
        &mut std::collections::HashSet::new(),
        cancel,
    )
}

fn zip_utf8(value: &std::ffi::OsStr) -> ArchiveResult<&str> {
    value.to_str().ok_or_else(|| {
        ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ZIP names and symlink targets must be UTF-8; use TAR for native path bytes",
        ))
    })
}

/// Internal implementation with visited tracking for loop detection.
fn add_path_to_zip_with_visited<W: Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
    options: &zip::write::SimpleFileOptions,
    visited: &mut std::collections::HashSet<PathBuf>,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    check_archive_cancel(cancel)?;
    // Use symlink_metadata to detect symlinks without following them
    let metadata = std::fs::symlink_metadata(path)?;

    if metadata.is_symlink() {
        // Handle symlink
        let target = std::fs::read_link(path)?;

        let target = zip_utf8(target.as_os_str())?;
        archive.add_symlink(name, target, *options)?;
    } else if metadata.is_dir() {
        // Detect symlink loops by checking canonical path
        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(_) => path.to_path_buf(),
        };

        if !visited.insert(canonical.clone()) {
            // Already visited this directory (symlink loop)
            // LOW-3: use tracing::warn! instead of eprintln!
            tracing::warn!("Skipping symlink loop at {}", path.display());
            return Ok(());
        }

        // Add directory entry
        let dir_name = if name.ends_with('/') {
            name.to_string()
        } else {
            format!("{}/", name)
        };
        archive.add_directory(&dir_name, *options)?;

        // Recursively add contents
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();
            let filename = entry.file_name();
            let filename = zip_utf8(&filename)?;
            let entry_name = format!("{}{}", dir_name, filename);
            add_path_to_zip_with_visited(
                archive,
                &entry_path,
                &entry_name,
                options,
                visited,
                cancel,
            )?;
        }

        // Remove from visited when done with this branch
        visited.remove(&canonical);
    } else if metadata.is_file() {
        let file_options = *options;
        #[cfg(unix)]
        let file_options = {
            use std::os::unix::fs::PermissionsExt;
            file_options.unix_permissions(metadata.permissions().mode() & 0o777)
        };
        archive.start_file(name, file_options)?;
        let mut file = open_zip_source(path, &metadata)?;
        copy_zip_source(&mut file, &metadata, archive)?;
    } else {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Unsupported file type: {}", path.display()),
        )));
    }

    Ok(())
}

fn copy_zip_source(
    file: &mut File,
    inspected: &std::fs::Metadata,
    writer: &mut impl Write,
) -> std::io::Result<()> {
    if !crate::copy::same_copy_source(inspected, &file.metadata()?) {
        return Err(std::io::Error::other("ZIP source changed before reading"));
    }
    // Never chase a growing file. No bytes beyond the inspected length may
    // enter the staged archive, even if the writer appends concurrently.
    let copied = std::io::copy(&mut (&mut *file).take(inspected.len()), writer)?;
    if copied != inspected.len() || !crate::copy::same_copy_source(inspected, &file.metadata()?) {
        return Err(std::io::Error::other("ZIP source changed while reading"));
    }
    Ok(())
}

fn open_zip_source(path: &Path, inspected: &std::fs::Metadata) -> std::io::Result<File> {
    if !inspected.is_file() {
        return Err(std::io::Error::other("ZIP source is not a regular file"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(std::io::Error::other(
            "ZIP source changed type while opening",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if inspected.dev() != opened.dev() || inspected.ino() != opened.ino() {
            return Err(std::io::Error::other(
                "ZIP source was replaced while opening",
            ));
        }
    }
    Ok(file)
}

/// Create a plain TAR archive.
fn create_tar(
    files: &[PathBuf],
    archive_path: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    let file = create_archive_file(archive_path, cancel)?;
    create_tar_to_writer(files, file).map(|_| ())
}

/// Create a TAR.GZ archive.
fn create_tar_gz(
    files: &[PathBuf],
    archive_path: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    let file = create_archive_file(archive_path, cancel)?;
    create_compressed_tar(files, file, ArchiveFormat::TarGz).map(|_| ())
}

/// Create a TAR.BZ2 archive.
fn create_tar_bz2(
    files: &[PathBuf],
    archive_path: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    let file = create_archive_file(archive_path, cancel)?;
    create_compressed_tar(files, file, ArchiveFormat::TarBz2).map(|_| ())
}

/// Create a TAR.XZ archive.
fn create_tar_xz(
    files: &[PathBuf],
    archive_path: &Path,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    let file = create_archive_file(archive_path, cancel)?;
    create_compressed_tar(files, file, ArchiveFormat::TarXz).map(|_| ())
}

fn create_compressed_tar<W: Write>(
    files: &[PathBuf],
    writer: W,
    format: ArchiveFormat,
) -> ArchiveResult<W> {
    // TAR completion does not finish its compression writer. Explicitly finish
    // both layers so trailer/flush failures cannot be discarded by Drop.
    Ok(match format {
        ArchiveFormat::TarGz => create_tar_to_writer(
            files,
            flate2::write::GzEncoder::new(writer, flate2::Compression::default()),
        )?
        .finish()?,
        ArchiveFormat::TarBz2 => create_tar_to_writer(
            files,
            bzip2::write::BzEncoder::new(writer, bzip2::Compression::default()),
        )?
        .finish()?,
        ArchiveFormat::TarXz => {
            create_tar_to_writer(files, xz2::write::XzEncoder::new(writer, 6))?.finish()?
        }
        _ => return Err(ArchiveError::UnsupportedFormat(format.extension().into())),
    })
}

/// Create a TAR archive to a writer.
///
/// Handles regular files, directories, and symlinks.
/// The `tar::Builder::append_dir_all` method handles symlinks within directories.
fn create_tar_to_writer<W: Write>(files: &[PathBuf], writer: W) -> ArchiveResult<W> {
    let mut archive = tar::Builder::new(writer);
    archive.follow_symlinks(false); // Store symlinks as symlinks, not as their targets

    for path in files {
        // TAR represents native Unix path bytes; do not route its top-level
        // names through ZIP's UTF-8-only name handling.
        let name = match path.file_name() {
            Some(n) => Path::new(n),
            None => {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Path '{}' has no filename component", path.display()),
                )));
            }
        };

        // Use symlink_metadata to detect type without following symlinks
        let metadata = std::fs::symlink_metadata(path)?;

        if metadata.is_symlink() {
            // Handle top-level symlink explicitly
            let target = std::fs::read_link(path)?;

            // Create symlink entry in TAR
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_mtime(
                metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );

            // Set the link name (symlink target)
            header.set_link_name(&target)?;
            header.set_cksum();

            archive.append_data(&mut header, name, std::io::empty())?;
        } else if metadata.is_dir() {
            archive.append_dir_all(name, path)?;
        } else if metadata.is_file() {
            archive.append_path_with_name(path, name)?;
        } else {
            return Err(ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Unsupported file type: {}", path.display()),
            )));
        }
    }

    Ok(archive.into_inner()?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn extraction_crash_worker() {
        let Some(root) = std::env::var_os("GRAVITYFILE_EXTRACTION_CRASH_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root).canonicalize().unwrap();
        let staging = crate::staging::private_directory(&root, ".gravityfile-extract-").unwrap();
        std::fs::create_dir(staging.path().join("nested")).unwrap();
        std::fs::write(staging.path().join("nested/file"), b"nested payload").unwrap();
        std::fs::write(staging.path().join("top"), b"top payload").unwrap();
        fn observe(_: &std::path::Path, index: usize, phase: &str) {
            if std::env::var("GRAVITYFILE_EXTRACTION_CRASH_AT").unwrap()
                == format!("{index}-{phase}")
            {
                std::process::exit(83);
            }
        }
        super::publish_extraction_observed(
            staging.path(),
            &root.join("destination"),
            &tokio_util::sync::CancellationToken::new(),
            super::install_extracted_leaf,
            super::remove_extracted_path,
            true,
            observe,
        )
        .unwrap();
        panic!("extraction crash phase not reached");
    }

    #[test]
    fn extraction_journal_preserves_and_retains_output_after_every_publication_boundary() {
        for existing in [false, true] {
            for index in 0..if existing { 3 } else { 4 } {
                for phase in ["pending", "published", "done"] {
                    let root = tempfile::tempdir().unwrap();
                    let destination = root.path().join("destination");
                    if existing {
                        std::fs::create_dir(&destination).unwrap();
                        std::fs::write(destination.join("unrelated"), b"existing data").unwrap();
                    }
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "archive::tests::extraction_crash_worker",
                            "--nocapture",
                        ])
                        .env("GRAVITYFILE_EXTRACTION_CRASH_ROOT", root.path())
                        .env(
                            "GRAVITYFILE_EXTRACTION_CRASH_AT",
                            format!("{index}-{phase}"),
                        )
                        .output()
                        .unwrap();
                    assert_eq!(
                        output.status.code(),
                        Some(83),
                        "{}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                    let report = crate::discover_recovery_report(root.path()).unwrap();
                    assert!(report.issues.is_empty(), "{:?}", report.issues);
                    assert_eq!(report.archive_records.len(), 1);
                    let record = &report.archive_records[0];
                    assert_eq!(record.status, "interrupted_or_prepared");
                    let mut bytes = Vec::new();
                    report.write_json(root.path(), &mut bytes).unwrap();
                    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(json["archive_records"].as_array().unwrap().len(), 1);
                    for _ in 0..2 {
                        crate::retain_archive_extraction(&record.transaction).unwrap();
                    }
                    assert_eq!(
                        std::fs::read(record.staging.join("nested/file")).unwrap(),
                        b"nested payload"
                    );
                    assert_eq!(
                        std::fs::read(record.staging.join("top")).unwrap(),
                        b"top payload"
                    );
                    assert!(
                        !destination.join("nested").exists() && !destination.join("top").exists()
                    );
                    if existing {
                        assert_eq!(
                            std::fs::read(destination.join("unrelated")).unwrap(),
                            b"existing data"
                        );
                    } else {
                        assert!(!destination.exists());
                    }
                    assert!(record.transaction.join("retained.archive").exists());
                }
            }
        }
    }
    #[test]
    fn journaled_extraction_failure_retains_published_data_without_unlinking() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let staging = crate::staging::private_directory(&root, ".gravityfile-extract-").unwrap();
        std::fs::write(staging.path().join("first"), b"original payload").unwrap();
        std::fs::write(staging.path().join("second"), b"second payload").unwrap();
        let destination = root.join("destination");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("unrelated"), b"existing data").unwrap();
        let mut calls = 0;
        let mut installed = None;
        let result = super::publish_extraction_observed(
            staging.path(),
            &destination,
            &tokio_util::sync::CancellationToken::new(),
            |source, target| {
                calls += 1;
                if calls == 2 {
                    // Same-object edits must survive failed publication too.
                    std::fs::write(installed.as_ref().unwrap(), b"edited payload")?;
                    return Err(std::io::Error::other("injected publication failure").into());
                }
                super::install_extracted_leaf(source, target)?;
                installed = Some(target.to_path_buf());
                Ok(())
            },
            |_, _| panic!("journaled rollback must never unlink published data"),
            true,
            |_, _, _| {},
        );
        let Err(super::ArchiveError::ExtractionRecovery { transaction, .. }) = result else {
            panic!("failed publication must expose retained recovery journal");
        };
        assert!(!destination.join("first").exists());
        assert!(!destination.join("second").exists());
        assert_eq!(
            std::fs::read(destination.join("unrelated")).unwrap(),
            b"existing data"
        );
        assert_eq!(
            std::fs::read(transaction.join("retained-0")).unwrap(),
            b"edited payload"
        );
        let other = if installed.as_ref().unwrap().file_name().unwrap() == "first" {
            "second"
        } else {
            "first"
        };
        assert_eq!(
            std::fs::read(staging.path().join(other)).unwrap(),
            if other == "second" {
                &b"second payload"[..]
            } else {
                &b"original payload"[..]
            }
        );
        let report = crate::discover_recovery_report(&root).unwrap();
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(report.archive_records[0].status, "rolled_back");
        crate::retain_archive_extraction(&transaction).unwrap();
        assert_eq!(
            std::fs::read(transaction.join("retained-0")).unwrap(),
            b"edited payload"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rollback_identity_guard_covers_directories_links_and_unknown_identity() {
        let root = tempfile::tempdir().unwrap();
        for directory in [false, true] {
            let path = root.path().join("entry");
            if directory {
                std::fs::create_dir(&path).unwrap();
            } else {
                std::os::unix::fs::symlink("original", &path).unwrap();
            }
            let entry = ExtractedEntry::capture(&path, &path);
            let retained = root.path().join("retained");
            std::fs::rename(&path, &retained).unwrap();
            if directory {
                std::fs::create_dir(&path).unwrap();
            } else {
                std::os::unix::fs::symlink("replacement", &path).unwrap();
            }
            let mut called = false;
            assert!(
                entry
                    .remove_if_unchanged(directory, &mut |_, _| {
                        called = true;
                        Ok(())
                    })
                    .is_err()
            );
            assert!(!called);
            if directory {
                assert!(path.is_dir());
                std::fs::remove_dir(&path).unwrap();
                std::fs::remove_dir(&retained).unwrap();
            } else {
                assert_eq!(std::fs::read_link(&path).unwrap(), Path::new("replacement"));
                std::fs::remove_file(&path).unwrap();
                std::fs::remove_file(&retained).unwrap();
            }
            entry
                .remove_if_unchanged(directory, &mut |_, _| {
                    panic!("absent path needs no removal")
                })
                .unwrap();
        }
        let path = root.path().join("unrecorded");
        let unknown = ExtractedEntry::capture(&path, &path);
        std::fs::write(&path, b"unrelated").unwrap();
        assert!(
            unknown
                .remove_if_unchanged(false, &mut |_, _| panic!(
                    "unknown identity must not be removed"
                ))
                .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"unrelated");
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rollback_preserves_replaced_leaf_identity() {
        for symlink in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let staging = root.path().join("staging");
            let destination = root.path().join("destination");
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join("a"), b"a").unwrap();
            std::fs::write(staging.join("b"), b"b").unwrap();
            let unrelated = root.path().join("unrelated");
            std::fs::write(&unrelated, b"must survive").unwrap();
            let mut installed: Option<PathBuf> = None;
            let result = publish_extraction_with_io(
                &staging,
                &destination,
                &CancellationToken::new(),
                |source, target| {
                    if let Some(first) = &installed {
                        std::fs::remove_file(first).unwrap();
                        if symlink {
                            std::os::unix::fs::symlink(&unrelated, first).unwrap();
                        } else {
                            std::fs::write(first, b"replacement").unwrap();
                        }
                        return Err(std::io::Error::other("injected publication failure").into());
                    }
                    install_extracted_leaf(source, target)?;
                    installed = Some(target.to_path_buf());
                    Ok(())
                },
                remove_extracted_path,
            );
            let first = installed.unwrap();
            if symlink {
                assert_eq!(std::fs::read_link(&first).unwrap(), unrelated);
            } else {
                assert_eq!(std::fs::read(&first).unwrap(), b"replacement");
            }
            assert_eq!(std::fs::read(&unrelated).unwrap(), b"must survive");
            let ArchiveError::ExtractionRollback { cleanup_errors, .. } = result.unwrap_err()
            else {
                panic!("expected incomplete rollback")
            };
            assert_eq!(cleanup_errors[0].0, first);
            assert!(cleanup_errors[0].1.to_string().contains("identity"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn extraction_leaf_cleanup_unlinks_symlinks_without_touching_targets() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"keep").unwrap();
        for link_target in [&target, &root.path().join("missing")] {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(link_target, &link).unwrap();
            remove_extracted_path(&link, false).unwrap();
            assert!(std::fs::symlink_metadata(&link).is_err());
            remove_extracted_path(&link, false).unwrap();
        }
        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn extraction_rollback_preserves_replacement_directory_contents() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        let destination = root.path().join("destination");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("a"), b"a").unwrap();
        std::fs::write(staging.join("b"), b"b").unwrap();
        let mut installed: Option<PathBuf> = None;
        let result = publish_extraction_with_io(
            &staging,
            &destination,
            &CancellationToken::new(),
            |source, target| {
                if let Some(first) = &installed {
                    std::fs::remove_file(first).unwrap();
                    std::fs::create_dir(first).unwrap();
                    std::fs::write(first.join("unrelated"), b"must survive").unwrap();
                    return Err(
                        std::io::Error::other("publication failed after replacement").into(),
                    );
                }
                install_extracted_leaf(source, target)?;
                installed = Some(target.to_path_buf());
                Ok(())
            },
            remove_extracted_path,
        );
        let replacement = installed.unwrap();
        assert_eq!(
            std::fs::read(replacement.join("unrelated")).unwrap(),
            b"must survive"
        );
        let ArchiveError::ExtractionRollback { cleanup_errors, .. } = result.unwrap_err() else {
            panic!("replacement must prevent rollback");
        };
        assert_eq!(cleanup_errors.len(), 2);
        assert_eq!(cleanup_errors[0].0, replacement);
        assert_eq!(cleanup_errors[1].0, destination);
    }

    #[test]
    fn extraction_publication_reports_incomplete_rollback_and_continues_cleanup() {
        for fail_cleanup in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let staging = root.path().join("staging");
            let destination = root.path().join("destination");
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join("a"), b"payload a").unwrap();
            std::fs::write(staging.join("b"), b"payload b").unwrap();
            let mut installed = None;
            let mut removals = Vec::new();
            let result = publish_extraction_with_io(
                &staging,
                &destination,
                &CancellationToken::new(),
                |source, target| {
                    if installed.is_some() {
                        return Err(std::io::Error::other("injected publication failure").into());
                    }
                    install_extracted_leaf(source, target)?;
                    installed = Some(target.to_path_buf());
                    Ok(())
                },
                |path, directory| {
                    removals.push(path.to_path_buf());
                    if fail_cleanup && !directory {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "injected removal failure",
                        ));
                    }
                    remove_extracted_path(path, directory)
                },
            );
            let installed = installed.unwrap();
            assert_eq!(removals, vec![installed.clone(), destination.clone()]);
            let error = result.unwrap_err();
            if fail_cleanup {
                let ArchiveError::ExtractionRollback {
                    destination: reported,
                    source,
                    cleanup_errors,
                } = error
                else {
                    panic!("expected incomplete rollback")
                };
                assert_eq!(reported, destination);
                assert!(source.to_string().contains("injected publication failure"));
                assert_eq!(cleanup_errors.len(), 2);
                assert_eq!(cleanup_errors[0].0, installed);
                assert_eq!(
                    cleanup_errors[0].1.kind(),
                    std::io::ErrorKind::PermissionDenied
                );
                assert_eq!(cleanup_errors[1].0, destination);
                assert!(installed.is_file());
            } else {
                assert!(matches!(error, ArchiveError::Io(_)));
                assert!(error.to_string().contains("injected publication failure"));
                assert!(!destination.exists());
            }
            assert_eq!(std::fs::read(staging.join("a")).unwrap(), b"payload a");
            assert_eq!(std::fs::read(staging.join("b")).unwrap(), b"payload b");
        }
    }

    #[test]
    fn tar_extension_reads_stop_before_advertised_gigabyte_body() {
        use std::cell::Cell;
        use std::io::{Cursor, Read};
        use std::rc::Rc;
        struct Counted<R>(R, Rc<Cell<u64>>);
        impl<R: Read> Read for Counted<R> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let read = self.0.read(bytes)?;
                self.1.set(self.1.get() + read as u64);
                Ok(read)
            }
        }
        for kind in *b"LKx" {
            let directory = tempfile::tempdir().unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_path("extension").unwrap();
            header.set_entry_type(tar::EntryType::new(kind));
            header.set_size(1 << 30);
            header.set_cksum();
            let reads = Rc::new(Cell::new(0));
            let reader = Counted(
                Cursor::new(header.as_bytes().to_vec()).chain(std::io::repeat(0).take(1 << 30)),
                reads.clone(),
            );
            let error =
                super::extract_tar_with_metadata_budget(reader, directory.path(), 8192, 4096)
                    .unwrap_err();
            assert!(
                matches!(error, super::ArchiveError::DecompressionBomb(_)),
                "{error}"
            );
            assert_eq!(reads.get(), 4096);
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn tar_metadata_budget_accumulates_but_excludes_regular_payload() {
        let directory = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(8192);
        header.set_mode(0o600);
        builder
            .append_data(&mut header, "payload", &[42; 8192][..])
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        super::extract_tar_with_metadata_budget(bytes.as_slice(), directory.path(), 1536, 1024)
            .unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("payload")).unwrap(),
            vec![42; 8192]
        );

        let mut builder = tar::Builder::new(Vec::new());
        for i in 0..4 {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o600);
            builder
                .append_data(&mut header, format!("empty-{i}"), std::io::empty())
                .unwrap();
        }
        let bytes = builder.into_inner().unwrap();
        let error =
            super::extract_tar_with_metadata_budget(bytes.as_slice(), directory.path(), 1536, 1024)
                .unwrap_err();
        assert!(matches!(error, super::ArchiveError::DecompressionBomb(_)));
        assert!(!directory.path().join("empty-3").exists());
    }

    #[test]
    fn oversized_compressed_tar_metadata_never_publishes_destination() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("bomb.tar.gz");
        let destination = directory.path().join("output");
        let mut encoder = flate2::write::GzEncoder::new(
            std::fs::File::create(&archive).unwrap(),
            flate2::Compression::fast(),
        );
        let mut header = tar::Header::new_gnu();
        header.set_path("extension").unwrap();
        header.set_entry_type(tar::EntryType::new(b'L'));
        header.set_size(2 * super::MAX_TAR_ENTRY_METADATA_READ_BYTES);
        header.set_cksum();
        encoder.write_all(header.as_bytes()).unwrap();
        std::io::copy(
            &mut std::io::repeat(0).take(header.size().unwrap()),
            &mut encoder,
        )
        .unwrap();
        encoder.finish().unwrap();
        let error = super::extract_archive(&archive, &destination).unwrap_err();
        assert!(
            matches!(error, super::ArchiveError::DecompressionBomb(_)),
            "{error}"
        );
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn zip_metadata_budget_rejects_before_materialization_but_not_payload_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.zip");
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for index in 0..32 {
            archive
                .start_file(format!("file-{index:02}"), options)
                .unwrap();
            archive.write_all(&vec![b'x'; 64 * 1024]).unwrap();
        }
        std::fs::write(&path, archive.finish().unwrap().into_inner()).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let error = extract_zip_with_metadata_budget(
            open_archive_input(&path).unwrap(),
            staging.path(),
            1024,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(
            matches!(error, ArchiveError::DecompressionBomb(_)),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
        // The same archive fits a larger metadata budget. Its 2 MiB payload is
        // deliberately much larger than that budget, and must still extract.
        extract_zip_with_metadata_budget(
            open_archive_input(&path).unwrap(),
            staging.path(),
            64 * 1024,
            &CancellationToken::new(),
        )
        .unwrap();
        for index in 0..32 {
            assert_eq!(
                std::fs::read(staging.path().join(format!("file-{index:02}"))).unwrap(),
                vec![b'x'; 64 * 1024]
            );
        }
    }

    #[test]
    fn zip_size_preflight_checks_individual_total_and_zero_input_limits() {
        assert_eq!(validate_zip_sizes(0, 0, 0, "empty").unwrap(), 0);
        assert!(validate_zip_sizes(0, 1, 0, "impossible").is_err());
        assert!(
            validate_zip_sizes(
                MAX_SINGLE_ENTRY_SIZE + 1,
                MAX_SINGLE_ENTRY_SIZE + 1,
                0,
                "huge"
            )
            .is_err()
        );
        assert!(validate_zip_sizes(2, 2, MAX_TOTAL_EXTRACTED_SIZE - 1, "over-total").is_err());
        assert_eq!(
            validate_zip_sizes(1, 1, MAX_TOTAL_EXTRACTED_SIZE - 1, "boundary").unwrap(),
            MAX_TOTAL_EXTRACTED_SIZE
        );
        assert!(validate_zip_sizes(1, 1001, 0, "ratio").is_err());
        assert_eq!(
            validate_zip_sizes(1, 1000, 0, "ratio-boundary").unwrap(),
            1000
        );
    }

    #[test]
    fn zip_zero_compressed_nonempty_entry_is_rejected_before_materialization() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.zip");
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive
            .start_file(
                "payload",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(b"content").unwrap();
        let mut bytes = archive.finish().unwrap().into_inner();
        let central = bytes
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        // Central-directory compressed-size field; preserve nonempty output size.
        bytes[central + 20..central + 24].copy_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let error = extract_zip(
            open_archive_input(&path).unwrap(),
            staging.path(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(
            matches!(error, ArchiveError::DecompressionBomb(_)),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    }

    #[test]
    fn xz_dictionary_memory_request_is_rejected_before_publication() {
        // XZ stream + block headers with valid CRC32s. The LZMA2 property 0x28
        // requests a 4 GiB dictionary. No compressed payload is needed to reject
        // the header, and this fixture never constructs a large encoder.
        let header = [
            253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 2, 0, 33, 1, 40, 0, 0, 0, 230, 160,
            17, 179,
        ];
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("large-dictionary.tar.xz");
        let destination = directory.path().join("output");
        std::fs::write(&archive, header).unwrap();
        let error = extract_archive(&archive, &destination).unwrap_err();
        assert!(
            error.to_string().to_lowercase().contains("memory"),
            "{error}"
        );
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    fn encode_test_member(format: ArchiveFormat, bytes: &[u8]) -> Vec<u8> {
        match format {
            ArchiveFormat::TarGz => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(bytes).unwrap();
                encoder.finish().unwrap()
            }
            ArchiveFormat::TarBz2 => {
                let mut encoder =
                    bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
                encoder.write_all(bytes).unwrap();
                encoder.finish().unwrap()
            }
            ArchiveFormat::TarXz => {
                let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
                encoder.write_all(bytes).unwrap();
                encoder.finish().unwrap()
            }
            _ => panic!("compressed TAR format required"),
        }
    }

    #[test]
    fn concatenated_members_can_split_tar_headers_payloads_and_padding() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(1024);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "payload", [b'x'; 1024].as_slice())
            .unwrap();
        let bytes = tar.into_inner().unwrap();
        for format in [
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            for split in [13, 600, bytes.len() - 100] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory
                    .path()
                    .join(format!("input{}", format.extension()));
                let mut encoded = encode_test_member(format, &bytes[..split]);
                encoded.extend_from_slice(&encode_test_member(format, &bytes[split..]));
                std::fs::write(&path, encoded).unwrap();
                let destination = directory.path().join("output");
                extract_archive(&path, &destination).unwrap();
                assert_eq!(
                    std::fs::read(destination.join("payload")).unwrap(),
                    [b'x'; 1024]
                );
            }
        }
    }

    #[test]
    fn later_compression_members_cannot_hide_data_or_truncated_trailers() {
        for format in [
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            for truncated in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory
                    .path()
                    .join(format!("input{}", format.extension()));
                let mut encoded = encode_test_member(format, &[0; 1024]);
                let mut second =
                    encode_test_member(format, if truncated { &[0; 512] } else { b"hidden data" });
                if truncated {
                    second.truncate(second.len() - 4);
                }
                encoded.extend_from_slice(&second);
                std::fs::write(&path, encoded).unwrap();
                let destination = directory.path().join("output");
                assert!(
                    extract_archive(&path, &destination).is_err(),
                    "{format:?}, truncated={truncated}"
                );
                assert!(!destination.exists());
            }
        }
    }

    #[test]
    fn truncated_compression_trailers_fail_before_publication() {
        for format in [
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            std::fs::write(&source, b"content").unwrap();
            let archive_path = directory
                .path()
                .join(format!("archive{}", format.extension()));
            create_archive(&[source], &archive_path, format).unwrap();
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&archive_path)
                .unwrap();
            let length = file.metadata().unwrap().len();
            file.set_len(length - 4).unwrap();
            drop(file);
            let destination = directory.path().join("destination");
            assert!(
                extract_archive(&archive_path, &destination).is_err(),
                "{format:?}"
            );
            assert!(!destination.exists(), "{format:?}");
        }
    }

    #[test]
    fn corrupt_gzip_checksum_fails_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        let archive_path = directory.path().join("archive.tar.gz");
        create_archive(&[source], &archive_path, ArchiveFormat::TarGz).unwrap();
        let mut bytes = std::fs::read(&archive_path).unwrap();
        let crc_start = bytes.len() - 8;
        bytes[crc_start] ^= 1;
        std::fs::write(&archive_path, bytes).unwrap();
        let destination = directory.path().join("destination");
        assert!(extract_archive(&archive_path, &destination).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn tar_trailing_padding_is_bounded_and_cannot_hide_data() {
        let directory = tempfile::tempdir().unwrap();
        let mut valid = vec![0u8; 1024];
        valid.extend_from_slice(&[0; 8192]);
        assert!(
            extract_complete_tar(valid.as_slice(), directory.path())
                .unwrap()
                .is_empty()
        );
        let mut nonzero = vec![0u8; 1024];
        nonzero.extend_from_slice(b"hidden content");
        assert!(
            extract_complete_tar(nonzero.as_slice(), directory.path())
                .unwrap_err()
                .to_string()
                .contains("Nonzero data")
        );
        let oversized = vec![0u8; 1024 * 1024 + 1024];
        assert!(matches!(
            extract_complete_tar(oversized.as_slice(), directory.path()),
            Err(ArchiveError::DecompressionBomb(_))
        ));
    }

    #[test]
    fn archive_input_errors_do_not_create_destination_parents() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("new-parent/output");
        let missing = directory.path().join("missing.zip");
        assert!(matches!(
            extract_archive(&missing, &destination),
            Err(ArchiveError::NotFound(_))
        ));
        let input_directory = directory.path().join("directory.zip");
        std::fs::create_dir(&input_directory).unwrap();
        assert!(matches!(
            extract_archive(&input_directory, &destination),
            Err(ArchiveError::Io(_))
        ));
        assert!(!directory.path().join("new-parent").exists());
    }

    #[cfg(unix)]
    #[test]
    fn archive_input_rejects_fifo_without_waiting_for_writer() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("input.tar");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is a valid NUL-terminated path throughout the call.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let error = extract_archive(&fifo, &directory.path().join("out")).unwrap_err();
        assert!(error.to_string().contains("regular file"));
        assert!(!directory.path().join("out").exists());
    }

    #[cfg(unix)]
    #[test]
    fn archive_input_accepts_regular_symlink_and_keeps_opened_identity() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.tar");
        let alias = directory.path().join("alias.tar");
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(7);
        header.set_mode(0o600);
        header.set_cksum();
        archive
            .append_data(&mut header, "payload", b"content".as_slice())
            .unwrap();
        std::fs::write(&input, archive.into_inner().unwrap()).unwrap();
        symlink(&input, &alias).unwrap();
        let file = open_archive_input(&alias).unwrap();
        std::fs::rename(&input, directory.path().join("original.tar")).unwrap();
        std::fs::write(&input, b"replacement is not an archive").unwrap();
        let staging = tempfile::tempdir().unwrap();
        extract_tar(file, staging.path(), &CancellationToken::new()).unwrap();
        assert_eq!(
            std::fs::read(staging.path().join("payload")).unwrap(),
            b"content"
        );
    }

    fn sparse_test_archive(logical_size: u64) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::GNUSparse);
        header.set_mode(0o600);
        header.set_size(1);
        let gnu = header.as_gnu_mut().unwrap();
        gnu.set_real_size(logical_size);
        gnu.sparse[0].set_offset(logical_size - 1);
        gnu.sparse[0].set_length(1);
        header.set_cksum();
        archive
            .append_data(&mut header, "nested/sparse", b"x".as_slice())
            .unwrap();
        archive.into_inner().unwrap()
    }

    #[test]
    fn oversized_sparse_tar_is_rejected_before_creating_output_paths() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = sparse_test_archive(MAX_SINGLE_ENTRY_SIZE + 1);
        let error = extract_tar_from_reader(bytes.as_slice(), directory.path()).unwrap_err();
        assert!(matches!(error, ArchiveError::DecompressionBomb(_)));
        assert!(!directory.path().join("nested").exists());
    }

    #[test]
    fn bounded_sparse_tar_retains_its_logical_contents() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = sparse_test_archive(64);
        extract_tar_from_reader(bytes.as_slice(), directory.path()).unwrap();
        let content = std::fs::read(directory.path().join("nested/sparse")).unwrap();
        assert_eq!(content.len(), 64);
        assert!(content[..63].iter().all(|byte| *byte == 0));
        assert_eq!(content[63], b'x');
    }

    #[test]
    fn tar_special_file_headers_are_rejected_before_materialization() {
        for kind in [
            tar::EntryType::Fifo,
            tar::EntryType::Char,
            tar::EntryType::Block,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut archive = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(0o600);
            header.set_size(0);
            header.set_cksum();
            archive
                .append_data(&mut header, "nested/special", std::io::empty())
                .unwrap();
            let bytes = archive.into_inner().unwrap();
            let error = extract_tar_from_reader(bytes.as_slice(), directory.path()).unwrap_err();
            assert!(error.to_string().contains("Unsupported special-file"));
            assert!(!directory.path().join("nested").exists());
        }
    }

    fn append_test_hardlink(archive: &mut tar::Builder<Vec<u8>>, name: &str, target: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_mode(0o644);
        header.set_size(0);
        header.set_link_name(target).unwrap();
        header.set_cksum();
        archive
            .append_data(&mut header, name, std::io::empty())
            .unwrap();
    }

    #[test]
    fn tar_hardlinks_resolve_from_archive_root_including_nested_entries() {
        let directory = tempfile::tempdir().unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(7);
        header.set_cksum();
        archive
            .append_data(&mut header, "payload", b"content".as_slice())
            .unwrap();
        append_test_hardlink(&mut archive, "nested/link", "payload");
        let bytes = archive.into_inner().unwrap();
        extract_tar_from_reader(bytes.as_slice(), directory.path()).unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("nested/link")).unwrap(),
            b"content"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(directory.path().join("payload"))
                    .unwrap()
                    .ino(),
                std::fs::metadata(directory.path().join("nested/link"))
                    .unwrap()
                    .ino()
            );
        }
    }

    #[test]
    fn tar_hardlinks_cannot_import_files_from_process_working_directory() {
        // Cargo runs this test from a package directory that contains Cargo.toml.
        assert!(Path::new("Cargo.toml").is_file());
        let directory = tempfile::tempdir().unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        append_test_hardlink(&mut archive, "imported", "Cargo.toml");
        let bytes = archive.into_inner().unwrap();
        assert!(extract_tar_from_reader(bytes.as_slice(), directory.path()).is_err());
        assert!(!directory.path().join("imported").exists());
    }

    #[cfg(unix)]
    #[test]
    fn tar_hardlink_validation_rejects_symlinks_and_parent_traversal() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("payload"), b"content").unwrap();
        symlink("payload", root.join("link")).unwrap();
        for target in [Path::new("link"), Path::new("../payload"), Path::new(".")] {
            assert!(validate_tar_hardlink_target(target, &root).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn tar_root_headers_cannot_change_private_staging_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let staging = crate::staging::private_directory(parent.path(), ".extract-test-").unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        for mode in [0o777, 0o000] {
            let mut root = tar::Header::new_gnu();
            root.set_entry_type(tar::EntryType::Directory);
            root.set_mode(mode);
            root.set_size(0);
            root.set_cksum();
            archive
                .append_data(&mut root, ".", std::io::empty())
                .unwrap();
        }
        let mut file = tar::Header::new_gnu();
        file.set_mode(0o644);
        file.set_size(7);
        file.set_cksum();
        archive
            .append_data(&mut file, "payload", b"content".as_slice())
            .unwrap();
        let bytes = archive.into_inner().unwrap();
        let extracted = extract_tar_from_reader(bytes.as_slice(), staging.path()).unwrap();
        assert_eq!(extracted, vec![staging.path().join("payload")]);
        assert_eq!(
            std::fs::metadata(staging.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::read(staging.path().join("payload")).unwrap(),
            b"content"
        );
    }

    #[test]
    fn tar_rejects_regular_file_at_extraction_root() {
        let directory = tempfile::tempdir().unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        let mut root = tar::Header::new_gnu();
        root.set_mode(0o644);
        root.set_size(0);
        root.set_cksum();
        archive
            .append_data(&mut root, ".", std::io::empty())
            .unwrap();
        let bytes = archive.into_inner().unwrap();
        let error = extract_tar_from_reader(bytes.as_slice(), directory.path()).unwrap_err();
        assert!(error.to_string().contains("extraction root"));
        assert!(directory.path().is_dir());
    }

    #[test]
    fn zip_root_directory_is_skipped_without_losing_contents() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("root.zip");
        let mut archive = zip::ZipWriter::new(File::create(&archive_path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        archive.add_directory("./", options).unwrap();
        archive.start_file("payload", options).unwrap();
        archive.write_all(b"content").unwrap();
        archive.finish().unwrap();
        let staging =
            crate::staging::private_directory(directory.path(), ".extract-test-").unwrap();
        let extracted = extract_zip(
            open_archive_input(&archive_path).unwrap(),
            staging.path(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(extracted, vec![staging.path().join("payload")]);
        assert_eq!(
            std::fs::read(staging.path().join("payload")).unwrap(),
            b"content"
        );
    }

    use super::*;
    use tempfile::TempDir;

    #[test]
    fn colliding_source_names_are_rejected_before_publication() {
        for format in [
            ArchiveFormat::Zip,
            ArchiveFormat::Tar,
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            let root = TempDir::new().unwrap();
            let mut sources = Vec::new();
            for parent in ["one", "two"] {
                let dir = root.path().join(parent);
                std::fs::create_dir(&dir).unwrap();
                let file = dir.join("same.txt");
                std::fs::write(&file, parent).unwrap();
                sources.push(file);
            }
            let destination = root.path().join("output");
            assert!(create_archive(&sources, &destination, format).is_err());
            assert!(!destination.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn zip_symlinks_round_trip_as_links() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("target"), "contents").unwrap();
        std::os::unix::fs::symlink("target", source.join("link")).unwrap();
        let archive = root.path().join("archive.zip");
        create_archive(&[source], &archive, ArchiveFormat::Zip).unwrap();
        let destination = root.path().join("extracted");
        extract_archive(&archive, &destination).unwrap();
        let link = destination.join("source/link");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("target"));
        assert_eq!(std::fs::read_to_string(link).unwrap(), "contents");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tar_round_trips_non_utf8_top_level_names_and_symlink_targets() {
        use std::os::unix::ffi::OsStringExt;
        for format in [
            ArchiveFormat::Tar,
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            let root = TempDir::new().unwrap();
            let file_name = std::ffi::OsString::from_vec(b"file-\xff".to_vec());
            let directory_name = std::ffi::OsString::from_vec(b"dir-\xfe".to_vec());
            let link_name = std::ffi::OsString::from_vec(b"link-\xfd".to_vec());
            let file = root.path().join(&file_name);
            let directory = root.path().join(&directory_name);
            let link = root.path().join(&link_name);
            std::fs::write(&file, b"file contents").unwrap();
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join(&file_name), b"nested contents").unwrap();
            std::os::unix::fs::symlink(&file_name, &link).unwrap();
            let archive = root.path().join(format!("archive{}", format.extension()));
            create_archive(&[file, directory, link], &archive, format).unwrap();
            let destination = root.path().join("extracted");
            extract_archive(&archive, &destination).unwrap();
            assert_eq!(
                std::fs::read(destination.join(&file_name)).unwrap(),
                b"file contents"
            );
            assert_eq!(
                std::fs::read(destination.join(&directory_name).join(&file_name)).unwrap(),
                b"nested contents"
            );
            assert_eq!(
                std::fs::read_link(destination.join(&link_name)).unwrap(),
                PathBuf::from(&file_name)
            );
            assert_eq!(
                std::fs::read(destination.join(&link_name)).unwrap(),
                b"file contents"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn zip_rejects_lossy_nested_names_and_link_targets() {
        use std::os::unix::ffi::OsStringExt;
        let root = TempDir::new().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let invalid = std::ffi::OsString::from_vec(vec![b'x', 255]);
        std::fs::write(source.join(&invalid), "contents").unwrap();
        let destination = root.path().join("archive.zip");
        assert!(create_archive(&[source], &destination, ArchiveFormat::Zip).is_err());
        assert!(!destination.exists());
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&invalid, &link).unwrap();
        assert!(create_archive(&[link], &destination, ArchiveFormat::Zip).is_err());
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn zip_path_encoding_never_replaces_native_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(vec![b'x', 255]);
        assert!(zip_utf8(&invalid).is_err());
        assert_eq!(zip_utf8(std::ffi::OsStr::new("résumé")).unwrap(), "résumé");
    }

    #[test]
    fn compressed_tar_reports_final_output_failure() {
        struct LimitedWriter {
            remaining: usize,
        }
        impl Write for LimitedWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Err(std::io::Error::other("injected storage exhaustion"));
                }
                let count = self.remaining.min(bytes.len());
                self.remaining -= count;
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for format in [
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            // Empty TAR has a real compressed trailer. Fail on the very last
            // output byte, after the encoder has accepted the TAR contents.
            let complete = create_compressed_tar(&[], Vec::new(), format).unwrap();
            let error = create_compressed_tar(
                &[],
                LimitedWriter {
                    remaining: complete.len() - 1,
                },
                format,
            )
            .err()
            .expect("final write failure was swallowed");
            assert!(error.to_string().contains("injected storage exhaustion"));
        }
    }

    #[test]
    fn archive_publication_preserves_a_concurrent_destination() {
        let root = TempDir::new().unwrap();
        let staged = root.path().join("payload");
        let destination = root.path().join("report.tar");
        std::fs::write(&staged, "complete archive").unwrap();
        // A second writer appeared after the initial existence check.
        std::fs::write(&destination, "concurrent writer").unwrap();
        assert!(matches!(
            publish_archive(&staged, &destination),
            Err(ArchiveError::DestinationExists(_))
        ));
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "concurrent writer"
        );
        assert_eq!(
            std::fs::read_to_string(&staged).unwrap(),
            "complete archive"
        );
    }

    #[test]
    fn all_archive_formats_finish_and_round_trip() {
        for format in [
            ArchiveFormat::Zip,
            ArchiveFormat::Tar,
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            let root = TempDir::new().unwrap();
            let source = root.path().join("source.txt");
            std::fs::write(&source, "archived contents").unwrap();
            let destination = root.path().join(format!("archive.{}", format.extension()));
            create_archive(&[source], &destination, format).unwrap();
            let extracted = root.path().join("extracted");
            extract_archive(&destination, &extracted).unwrap();
            assert_eq!(
                std::fs::read_to_string(extracted.join("source.txt")).unwrap(),
                "archived contents"
            );
            assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".gravityfile-archive-")
            }));
        }
    }

    #[test]
    fn test_archive_format_detection() {
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.tar.gz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.tgz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.tar")),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(ArchiveFormat::from_path(Path::new("test.txt")), None);
    }

    #[test]
    fn test_create_and_extract_zip() {
        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();

        // Create test files
        std::fs::write(source_dir.join("test.txt"), "Hello, World!").unwrap();
        std::fs::create_dir(source_dir.join("subdir")).unwrap();
        std::fs::write(source_dir.join("subdir/nested.txt"), "Nested content").unwrap();

        // Create archive
        let archive_path = temp_dir.path().join("test.zip");
        create_archive(
            std::slice::from_ref(&source_dir),
            &archive_path,
            ArchiveFormat::Zip,
        )
        .unwrap();

        assert!(archive_path.exists());

        // Extract archive
        let extract_dir = temp_dir.path().join("extracted");
        let extracted = extract_archive(&archive_path, &extract_dir).unwrap();

        assert!(!extracted.is_empty());
    }

    #[test]
    fn extraction_conflict_preserves_existing_files_and_publishes_nothing() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let existing_source = source.join("existing.txt");
        let new_source = source.join("new.txt");
        std::fs::write(&existing_source, "archive version").unwrap();
        std::fs::write(&new_source, "new content").unwrap();
        std::fs::write(destination.join("existing.txt"), "original version").unwrap();
        let archive = temp.path().join("conflict.zip");
        create_archive(&[existing_source, new_source], &archive, ArchiveFormat::Zip).unwrap();

        let error = extract_archive(&archive, &destination).unwrap_err();

        assert!(matches!(error, ArchiveError::Io(_)));
        assert_eq!(
            std::fs::read_to_string(destination.join("existing.txt")).unwrap(),
            "original version"
        );
        assert!(!destination.join("new.txt").exists());
        assert!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .contains("gravityfile-tmp"))
        );
    }

    #[test]
    fn test_create_and_extract_tar_gz() {
        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();

        // Create test files
        std::fs::write(source_dir.join("test.txt"), "Hello from tar.gz!").unwrap();
        std::fs::create_dir(source_dir.join("subdir")).unwrap();
        std::fs::write(source_dir.join("subdir/nested.txt"), "Nested in tar").unwrap();

        // Create archive
        let archive_path = temp_dir.path().join("test.tar.gz");
        create_archive(
            std::slice::from_ref(&source_dir),
            &archive_path,
            ArchiveFormat::TarGz,
        )
        .unwrap();

        assert!(archive_path.exists());

        // Extract archive
        let extract_dir = temp_dir.path().join("extracted");
        let extracted = extract_archive(&archive_path, &extract_dir).unwrap();

        assert!(!extracted.is_empty());
        // Verify content was extracted correctly
        assert!(extract_dir.join("source").exists() || extract_dir.join("test.txt").exists());
    }

    #[test]
    fn test_path_traversal_prevention_absolute_path() {
        let path = Path::new("/etc/passwd");
        assert!(path.is_absolute());

        let has_parent_dir = path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        assert!(path.is_absolute() || has_parent_dir);
    }

    #[test]
    fn test_path_traversal_prevention_parent_dir() {
        let path = Path::new("../../../etc/passwd");
        let has_parent_dir = path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        assert!(has_parent_dir);

        let path2 = Path::new("foo/../../../bar");
        let has_parent_dir2 = path2
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        assert!(has_parent_dir2);

        let safe_path = Path::new("foo/bar/baz.txt");
        let has_parent_dir3 = safe_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        assert!(!has_parent_dir3);
    }

    #[test]
    fn test_empty_archive_handling() {
        let temp_dir = TempDir::new().unwrap();

        // Create empty zip
        let archive_path = temp_dir.path().join("empty.zip");
        create_archive(&[], &archive_path, ArchiveFormat::Zip).unwrap();

        // Extract should succeed with empty result
        let extract_dir = temp_dir.path().join("extracted");
        let extracted = extract_archive(&archive_path, &extract_dir).unwrap();
        assert!(extracted.is_empty());
    }

    #[test]
    fn test_archive_destination_not_exists() {
        let temp_dir = TempDir::new().unwrap();

        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test content").unwrap();

        let archive_path = temp_dir.path().join("test.zip");
        create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

        // Extract to non-existent directory (should create it)
        let extract_dir = temp_dir.path().join("new_dir/nested/deep");
        let extracted = extract_archive(&archive_path, &extract_dir).unwrap();
        assert!(!extracted.is_empty());
        assert!(extract_dir.exists());
    }

    #[test]
    fn zip_source_copy_rejects_growth_and_truncation_during_read() {
        struct MutatingWriter {
            source: File,
            append: bool,
            mutated: bool,
            bytes: Vec<u8>,
        }
        impl Write for MutatingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if !self.mutated {
                    self.mutated = true;
                    if self.append {
                        self.source.write_all(&[2; 1024])?;
                    } else {
                        self.source.set_len(0)?;
                    }
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for append in [true, false] {
            let root = TempDir::new().unwrap();
            let path = root.path().join("source");
            std::fs::write(&path, [1; 1024]).unwrap();
            let inspected = std::fs::symlink_metadata(&path).unwrap();
            let mut source = open_zip_source(&path, &inspected).unwrap();
            let mut writer = MutatingWriter {
                source: std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap(),
                append,
                mutated: false,
                bytes: Vec::new(),
            };
            let error = copy_zip_source(&mut source, &inspected, &mut writer).unwrap_err();
            assert!(error.to_string().contains("ZIP source changed"), "{error}");
            assert!(writer.bytes.len() <= inspected.len() as usize);
        }
    }

    #[cfg(unix)]
    #[test]
    fn zip_source_open_rejects_replacement_after_inspection() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("source");
        let retained = root.path().join("retained");
        std::fs::write(&path, b"original").unwrap();
        let inspected = std::fs::symlink_metadata(&path).unwrap();
        let mut file = open_zip_source(&path, &inspected).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"original");
        drop(file);
        std::fs::rename(&path, &retained).unwrap();
        std::os::unix::fs::symlink(&retained, &path).unwrap();
        assert!(
            open_zip_source(&path, &inspected).is_err(),
            "must not follow a replacement symlink"
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(
            open_zip_source(&path, &inspected).is_err(),
            "must not archive a replacement inode"
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            open_zip_source(&path, &inspected).is_err(),
            "must reject a replacement directory"
        );
        std::fs::remove_dir(&path).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: native is a live, NUL-terminated path in this private fixture.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let _ = send.send(open_zip_source(&path, &inspected).is_err());
        });
        assert!(
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("replacement FIFO must not block source open")
        );
        task.join().unwrap();
        assert_eq!(std::fs::read(&retained).unwrap(), b"original");
    }

    #[test]
    fn extraction_cancellation_discards_decoded_staging_before_merge() {
        for format in [
            ArchiveFormat::Zip,
            ArchiveFormat::Tar,
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            for existing in [false, true] {
                let root = TempDir::new().unwrap();
                let source = root.path().join("source");
                std::fs::write(&source, b"archive contents").unwrap();
                let archive = root.path().join(format!("archive{}", format.extension()));
                create_archive(&[source], &archive, format).unwrap();
                let destination = root.path().join("destination");
                if existing {
                    std::fs::create_dir(&destination).unwrap();
                    std::fs::write(destination.join("sentinel"), b"untouched").unwrap();
                }
                let cancel = CancellationToken::new();
                let result =
                    extract_archive_impl(&archive, &destination, &cancel, || cancel.cancel());
                assert!(matches!(result, Err(ArchiveError::Cancelled)), "{result:?}");
                assert!(!destination.join("source").exists());
                if existing {
                    assert_eq!(
                        std::fs::read(destination.join("sentinel")).unwrap(),
                        b"untouched"
                    );
                    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 1);
                } else {
                    assert!(!destination.exists());
                }
                assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".gravityfile-extract-")
                }));
                assert!(matches!(
                    extract_archive_cancellable(&archive, &destination, &cancel),
                    Err(ArchiveError::Cancelled)
                ));
            }
        }
    }

    #[test]
    fn extraction_reader_stops_after_midstream_cancellation() {
        struct CancelAfterRead {
            token: CancellationToken,
            calls: usize,
        }
        impl Read for CancelAfterRead {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.calls += 1;
                bytes[0] = b'x';
                self.token.cancel();
                Ok(1)
            }
        }
        let cancel = CancellationToken::new();
        let mut reader = ArchiveInput::new(
            CancelAfterRead {
                token: cancel.clone(),
                calls: 0,
            },
            &cancel,
        );
        let mut output = Vec::new();
        let error = reader.read_to_end(&mut output).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(output, b"x");
        assert_eq!(reader.reader.calls, 1);
        let mut seekable = ArchiveInput::new(std::io::Cursor::new(b"data"), &cancel);
        assert!(std::io::Seek::seek(&mut seekable, std::io::SeekFrom::Start(0)).is_err());
    }

    #[test]
    fn archive_cancellation_respects_publication_boundary() {
        for format in [
            ArchiveFormat::Zip,
            ArchiveFormat::Tar,
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            let root = TempDir::new().unwrap();
            let source = root.path().join("source");
            std::fs::write(&source, b"contents").unwrap();
            let destination = root.path().join(format!("archive{}", format.extension()));
            let cancel = CancellationToken::new();
            cancel.cancel();
            assert!(matches!(
                create_archive_cancellable(
                    std::slice::from_ref(&source),
                    &destination,
                    format,
                    &cancel
                ),
                Err(ArchiveError::Cancelled)
            ));
            assert!(!destination.exists());
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
            let cancel = CancellationToken::new();
            create_archive_impl(&[source], &destination, format, &cancel, |parent| {
                assert!(destination.is_file());
                cancel.cancel();
                crate::recovery::sync_directory(parent)
            })
            .unwrap();
            let extracted = root.path().join("extracted");
            extract_archive(&destination, &extracted).unwrap();
            assert_eq!(
                std::fs::read(extracted.join("source")).unwrap(),
                b"contents"
            );
        }
    }

    #[test]
    fn archive_output_cancellation_stops_writes_without_interrupted_retry() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("payload");
        let cancel = CancellationToken::new();
        let mut output = create_archive_file(&path, &cancel).unwrap();
        output.write_all(b"before").unwrap();
        cancel.cancel();
        let error = std::io::copy(&mut std::io::Cursor::new(b"after"), &mut output).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert!(output.flush().is_err());
        assert!(std::io::Seek::seek(&mut output, std::io::SeekFrom::Start(0)).is_err());
        drop(output);
        assert_eq!(std::fs::read(path).unwrap(), b"before");
    }

    #[cfg(unix)]
    #[test]
    fn published_archives_keep_payload_private() {
        use std::os::unix::fs::PermissionsExt;
        for format in [
            ArchiveFormat::Zip,
            ArchiveFormat::Tar,
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            let root = TempDir::new().unwrap();
            let source = root.path().join("private");
            std::fs::write(&source, b"private contents").unwrap();
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
            let destination = root.path().join(format!("archive{}", format.extension()));
            create_archive(&[source], &destination, format).unwrap();
            let mode = std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "published {format:?} must not grant group/other access"
            );
            assert_eq!(mode & 0o7000, 0);
            let extracted = root.path().join("extracted");
            extract_archive(&destination, &extracted).unwrap();
            assert_eq!(
                std::fs::read(extracted.join("private")).unwrap(),
                b"private contents"
            );
        }
    }

    #[test]
    fn archive_sync_failure_preserves_complete_published_output() {
        for format in [
            ArchiveFormat::Zip,
            ArchiveFormat::Tar,
            ArchiveFormat::TarGz,
            ArchiveFormat::TarBz2,
            ArchiveFormat::TarXz,
        ] {
            let root = TempDir::new().unwrap();
            let source = root.path().join("source");
            std::fs::write(&source, b"complete contents").unwrap();
            let destination = root.path().join(format!("archive{}", format.extension()));
            let error = create_archive_with_sync(&[source], &destination, format, |parent| {
                assert_eq!(parent, root.path());
                assert!(destination.is_file());
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected directory sync failure",
                ))
            })
            .unwrap_err();
            match error {
                ArchiveError::PublishedDurability {
                    destination: published,
                    source,
                } => {
                    assert_eq!(published, destination);
                    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
                }
                other => panic!("expected published-state error, received {other:?}"),
            }
            let extracted = root.path().join("extracted");
            extract_archive(&destination, &extracted).unwrap();
            assert_eq!(
                std::fs::read(extracted.join("source")).unwrap(),
                b"complete contents"
            );
            assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".gravityfile-archive-")
            }));
        }
    }

    #[test]
    fn failed_archive_creation_does_not_sync_a_destination() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("archive.zip");
        let result = create_archive_with_sync(
            &[root.path().join("missing")],
            &destination,
            ArchiveFormat::Zip,
            |_| panic!("must not sync before publication"),
        );
        assert!(result.is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn test_create_archive_failure_leaves_no_partial_destination() {
        let temp_dir = TempDir::new().unwrap();
        let missing = temp_dir.path().join("missing.txt");
        let archive_path = temp_dir.path().join("failed.zip");

        let result = create_archive(&[missing], &archive_path, ArchiveFormat::Zip);

        assert!(result.is_err());
        assert!(!archive_path.exists());
        assert!(
            std::fs::read_dir(temp_dir.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".gravityfile-archive-")),
            "failed archive creation left a staging file"
        );
    }

    #[test]
    fn test_create_archive_rejects_destination_inside_source() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("data.txt"), "data").unwrap();
        let archive_path = source.join("recursive.zip");

        let result = create_archive(
            std::slice::from_ref(&source),
            &archive_path,
            ArchiveFormat::Zip,
        );

        assert!(result.is_err());
        assert!(!archive_path.exists());
    }

    #[test]
    fn test_archive_already_exists_error() {
        let temp_dir = TempDir::new().unwrap();

        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test content").unwrap();

        let archive_path = temp_dir.path().join("test.zip");

        create_archive(
            std::slice::from_ref(&test_file),
            &archive_path,
            ArchiveFormat::Zip,
        )
        .unwrap();

        let result = create_archive(&[test_file], &archive_path, ArchiveFormat::Zip);
        assert!(matches!(result, Err(ArchiveError::DestinationExists(_))));
    }

    #[test]
    fn test_zip_path_validation_consistency() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test").unwrap();

        let archive_path = temp_dir.path().join("test.zip");
        create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

        let extract_dir = temp_dir.path().join("extracted");
        let result = extract_archive(&archive_path, &extract_dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_decompression_constants() {
        const {
            assert!(MAX_DECOMPRESSION_RATIO >= 100.0);
            assert!(MAX_DECOMPRESSION_RATIO <= 10000.0);
            assert!(MAX_TOTAL_EXTRACTED_SIZE >= 1024 * 1024 * 1024);
            assert!(MAX_TOTAL_EXTRACTED_SIZE <= 100 * 1024 * 1024 * 1024);
            assert!(MAX_ENTRY_COUNT >= 1_000);
            assert!(MAX_ENTRY_COUNT <= 10_000_000);
        }
    }

    #[test]
    fn zip_symlink_target_rejects_invalid_encoding_without_replacement() {
        for bytes in [b"".as_slice(), b"target\xff", b"target\0suffix"] {
            assert_eq!(
                zip_symlink_target(bytes).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
        assert_eq!(
            zip_symlink_target(" café ".as_bytes()).unwrap(),
            Path::new(" café ")
        );
    }

    #[cfg(unix)]
    #[test]
    fn zip_symlink_targets_preserve_significant_whitespace() {
        for target in [" target ", "target\n", "\ttarget", " "] {
            let root = TempDir::new().unwrap();
            let source = root.path().join("source");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join(target), b"exact target contents").unwrap();
            std::os::unix::fs::symlink(target, source.join("link")).unwrap();
            let archive = root.path().join("archive.zip");
            create_archive(&[source], &archive, ArchiveFormat::Zip).unwrap();
            let extracted = root.path().join("extracted");
            extract_archive(&archive, &extracted).unwrap();
            let link = extracted.join("source/link");
            assert_eq!(std::fs::read_link(&link).unwrap(), Path::new(target));
            assert_eq!(std::fs::read(link).unwrap(), b"exact target contents");
        }
    }

    #[cfg(unix)]
    #[test]
    fn zip_round_trip_preserves_regular_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        for mode in [0o600, 0o640, 0o700, 0o755] {
            let root = TempDir::new().unwrap();
            let top = root.path().join("top");
            let directory = root.path().join("nested");
            std::fs::create_dir(&directory).unwrap();
            let nested = directory.join("script");
            for path in [&top, &nested] {
                std::fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            let archive = root.path().join("archive.zip");
            create_archive(&[top, directory], &archive, ArchiveFormat::Zip).unwrap();
            let mut inventory = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
            for name in ["top", "nested/script"] {
                let entry = inventory.by_name(name).unwrap();
                assert_eq!(
                    entry.unix_mode().unwrap() & 0o7777,
                    mode,
                    "stored mode for {name}"
                );
            }
            let extracted = root.path().join("extracted");
            extract_archive(&archive, &extracted).unwrap();
            for name in ["top", "nested/script"] {
                assert_eq!(
                    std::fs::metadata(extracted.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o7777,
                    mode,
                    "extracted mode for {name}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_permission_stripping() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test content").unwrap();

        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&test_file, perms).unwrap();

        let archive_path = temp_dir.path().join("test.zip");
        create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

        let extract_dir = temp_dir.path().join("extracted");
        extract_archive(&archive_path, &extract_dir).unwrap();

        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());

        let extracted_perms = std::fs::metadata(&extracted_file).unwrap().permissions();
        let mode = extracted_perms.mode();
        assert_eq!(mode & 0o7000, 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_in_tar_archive() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();

        let test_file = source_dir.join("target.txt");
        std::fs::write(&test_file, "target content").unwrap();

        let symlink_path = source_dir.join("link.txt");
        symlink("target.txt", &symlink_path).unwrap();

        let archive_path = temp_dir.path().join("test.tar");
        create_archive(
            std::slice::from_ref(&source_dir),
            &archive_path,
            ArchiveFormat::Tar,
        )
        .unwrap();

        assert!(archive_path.exists());

        let extract_dir = temp_dir.path().join("extracted");
        let result = extract_archive(&archive_path, &extract_dir);
        assert!(result.is_ok(), "extract failed: {:?}", result.err());
    }

    #[test]
    fn test_symlink_validation_paths() {
        let safe_target = Path::new("subdir/file.txt");
        let has_parent = safe_target
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        assert!(!has_parent);

        let escape_target = Path::new("../../../etc/passwd");
        let has_parent2 = escape_target
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        assert!(has_parent2);

        let absolute_target = Path::new("/etc/passwd");
        assert!(absolute_target.is_absolute());
    }
}
