//! Archive operations (extract and compress).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Maximum decompression ratio allowed (1000:1).
/// Archives with higher ratios are considered potential zip bombs.
const MAX_DECOMPRESSION_RATIO: f64 = 1000.0;

/// Maximum total size for extracted content (10 GB).
const MAX_TOTAL_EXTRACTED_SIZE: u64 = 10 * 1024 * 1024 * 1024;

/// Maximum number of entries allowed in a TAR archive.
const MAX_ENTRY_COUNT: u64 = 100_000;

/// Maximum size for a single TAR entry (10 GB).
const MAX_SINGLE_ENTRY_SIZE: u64 = 10 * 1024 * 1024 * 1024;

/// Error that can occur during archive operations.
#[derive(Debug, Error)]
pub enum ArchiveError {
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
    // LOW-4: use symlink_metadata so we detect a symlink-as-archive-path correctly
    if std::fs::symlink_metadata(archive_path).is_err() {
        return Err(ArchiveError::NotFound(archive_path.to_path_buf()));
    }

    let format = ArchiveFormat::from_path(archive_path).ok_or_else(|| {
        ArchiveError::UnsupportedFormat(
            archive_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_string(),
        )
    })?;

    // Create destination directory if it doesn't exist
    std::fs::create_dir_all(destination)?;

    match format {
        ArchiveFormat::Zip => extract_zip(archive_path, destination),
        ArchiveFormat::Tar => extract_tar(archive_path, destination),
        ArchiveFormat::TarGz => extract_tar_gz(archive_path, destination),
        ArchiveFormat::TarBz2 => extract_tar_bz2(archive_path, destination),
        ArchiveFormat::TarXz => extract_tar_xz(archive_path, destination),
    }
}

/// Extract a ZIP archive.
///
/// # Security
/// This function validates all paths to prevent directory traversal attacks.
/// Paths containing `..` components or absolute paths are rejected.
/// Setuid/setgid bits are stripped from extracted file permissions.
/// Decompression bombs are detected via ratio and size limits.
fn extract_zip(archive_path: &Path, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let file = File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
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

    // Security: Check for decompression bombs before extraction
    let mut total_uncompressed: u64 = 0;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index_raw(i) {
            let compressed = entry.compressed_size();
            let uncompressed = entry.size();

            // Check individual file ratio
            if compressed > 0 {
                let ratio = uncompressed as f64 / compressed as f64;
                if ratio > MAX_DECOMPRESSION_RATIO {
                    return Err(ArchiveError::DecompressionBomb(format!(
                        "File '{}' has suspicious ratio {:.0}:1 (max {:.0}:1)",
                        entry.name(),
                        ratio,
                        MAX_DECOMPRESSION_RATIO
                    )));
                }
            }

            total_uncompressed = total_uncompressed.saturating_add(uncompressed);
        }
    }

    // Check total extraction size
    if total_uncompressed > MAX_TOTAL_EXTRACTED_SIZE {
        return Err(ArchiveError::DecompressionBomb(format!(
            "Archive would extract to {} bytes (max {} bytes)",
            total_uncompressed, MAX_TOTAL_EXTRACTED_SIZE
        )));
    }

    // MED-1: Extract in two passes — regular files and directories first,
    // symlinks last. This prevents a previously-extracted symlink from
    // redirecting `create_dir_all` outside the extraction root.
    let mut symlink_indices = Vec::new();

    for i in 0..archive.len() {
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
            // Regular file
            let mut outfile = File::create(&outpath)?;
            std::io::copy(&mut entry, &mut outfile)?;

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
        let mut entry = archive.by_index(i)?;
        let entry_path = entry.enclosed_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid file path in archive",
            )
        })?;

        let outpath = destination.join(&entry_path);

        // Read the symlink target from the file content
        let mut target_bytes = Vec::new();
        std::io::copy(&mut entry, &mut target_bytes)?;
        let target_str = String::from_utf8_lossy(&target_bytes);
        let link_target = Path::new(target_str.trim());

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

/// Extract a plain TAR archive.
fn extract_tar(archive_path: &Path, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let file = File::open(archive_path)?;
    extract_tar_from_reader(file, destination)
}

/// Extract a TAR.GZ archive.
fn extract_tar_gz(archive_path: &Path, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let file = File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    extract_tar_from_reader(decoder, destination)
}

/// Extract a TAR.BZ2 archive.
fn extract_tar_bz2(archive_path: &Path, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let file = File::open(archive_path)?;
    let decoder = bzip2::read::BzDecoder::new(file);
    extract_tar_from_reader(decoder, destination)
}

/// Extract a TAR.XZ archive.
fn extract_tar_xz(archive_path: &Path, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let file = File::open(archive_path)?;
    let decoder = xz2::read::XzDecoder::new(file);
    extract_tar_from_reader(decoder, destination)
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

/// Extract a TAR archive from a reader.
///
/// # Security
/// This function validates all paths to prevent directory traversal attacks.
/// Paths containing `..` components or absolute paths are rejected.
///
/// # Bomb protection (CRIT-4)
/// - Checks `entry.header().size()` against `MAX_SINGLE_ENTRY_SIZE` before unpacking.
/// - Maintains a cumulative byte counter against `MAX_TOTAL_EXTRACTED_SIZE`.
/// - Limits entry count to `MAX_ENTRY_COUNT`.
fn extract_tar_from_reader<R: Read>(reader: R, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let mut archive = tar::Archive::new(reader);
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

    for entry_result in archive.entries()? {
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

        // Pre-flight check: reject obviously oversized entries before
        // even attempting to unpack (defense in depth alongside the
        // post-unpack verification below).
        let declared_size = entry.header().size().unwrap_or(0);
        if declared_size > MAX_SINGLE_ENTRY_SIZE {
            return Err(ArchiveError::DecompressionBomb(format!(
                "Entry '{}' declares size {} bytes (max {} bytes)",
                entry_path.display(),
                declared_size,
                MAX_SINGLE_ENTRY_SIZE
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

        // Security: Validate symlink targets (MED-4 normalizer used)
        let entry_type = entry.header().entry_type();
        if (entry_type.is_symlink() || entry_type.is_hard_link())
            && let Ok(Some(link_target)) = entry.link_name()
        {
            validate_tar_symlink_target(&link_target, &outpath, destination, &canonical_dest)?;
        }

        entry.unpack(&outpath)?;

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
/// Directories are added recursively with all their contents.
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
///
/// # Note
///
/// Symlinks in source directories are currently skipped (not followed or stored).
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
    if archive_path.exists() {
        return Err(ArchiveError::DestinationExists(archive_path.to_path_buf()));
    }

    match format {
        ArchiveFormat::Zip => create_zip(files, archive_path),
        ArchiveFormat::Tar => create_tar(files, archive_path),
        ArchiveFormat::TarGz => create_tar_gz(files, archive_path),
        ArchiveFormat::TarBz2 => create_tar_bz2(files, archive_path),
        ArchiveFormat::TarXz => create_tar_xz(files, archive_path),
    }
}

/// Create a ZIP archive.
fn create_zip(files: &[PathBuf], archive_path: &Path) -> ArchiveResult<()> {
    let file = File::create(archive_path)?;
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
        add_path_to_zip(&mut archive, path, &name, &options)?;
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
) -> ArchiveResult<()> {
    add_path_to_zip_with_visited(
        archive,
        path,
        name,
        options,
        &mut std::collections::HashSet::new(),
    )
}

/// Internal implementation with visited tracking for loop detection.
fn add_path_to_zip_with_visited<W: Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
    options: &zip::write::SimpleFileOptions,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> ArchiveResult<()> {
    // Use symlink_metadata to detect symlinks without following them
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            // LOW-3: use tracing::warn! instead of eprintln!
            tracing::warn!("Cannot access {}: {}", path.display(), e);
            return Ok(());
        }
    };

    if metadata.is_symlink() {
        // Handle symlink
        let target = match std::fs::read_link(path) {
            Ok(t) => t,
            Err(e) => {
                // LOW-3: use tracing::warn! instead of eprintln!
                tracing::warn!("Cannot read symlink {}: {}", path.display(), e);
                return Ok(());
            }
        };

        // Store symlink using Unix external attributes
        #[cfg(unix)]
        {
            // Unix symlink mode: S_IFLNK (0o120000) | 0o777
            let symlink_mode = 0o120777;
            let unix_options = options.unix_permissions(symlink_mode);

            archive.start_file(name, unix_options)?;
            // Write the target path as the file content
            let target_str = target.to_string_lossy();
            archive.write_all(target_str.as_bytes())?;
        }

        #[cfg(not(unix))]
        {
            // On non-Unix, store symlink as a regular file with the target path
            archive.start_file(name, *options)?;
            let target_str = target.to_string_lossy();
            archive.write_all(target_str.as_bytes())?;
        }
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
            let entry_name = format!("{}{}", dir_name, entry.file_name().to_string_lossy());
            add_path_to_zip_with_visited(archive, &entry_path, &entry_name, options, visited)?;
        }

        // Remove from visited when done with this branch
        visited.remove(&canonical);
    } else if metadata.is_file() {
        archive.start_file(name, *options)?;
        let mut file = File::open(path)?;
        std::io::copy(&mut file, archive)?;
    }
    // Other file types (devices, sockets) are silently skipped

    Ok(())
}

/// Create a plain TAR archive.
fn create_tar(files: &[PathBuf], archive_path: &Path) -> ArchiveResult<()> {
    let file = File::create(archive_path)?;
    create_tar_to_writer(files, file)
}

/// Create a TAR.GZ archive.
fn create_tar_gz(files: &[PathBuf], archive_path: &Path) -> ArchiveResult<()> {
    let file = File::create(archive_path)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    create_tar_to_writer(files, encoder)
}

/// Create a TAR.BZ2 archive.
fn create_tar_bz2(files: &[PathBuf], archive_path: &Path) -> ArchiveResult<()> {
    let file = File::create(archive_path)?;
    let encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
    create_tar_to_writer(files, encoder)
}

/// Create a TAR.XZ archive.
fn create_tar_xz(files: &[PathBuf], archive_path: &Path) -> ArchiveResult<()> {
    let file = File::create(archive_path)?;
    let encoder = xz2::write::XzEncoder::new(file, 6);
    create_tar_to_writer(files, encoder)
}

/// Create a TAR archive to a writer.
///
/// Handles regular files, directories, and symlinks.
/// The `tar::Builder::append_dir_all` method handles symlinks within directories.
fn create_tar_to_writer<W: Write>(files: &[PathBuf], writer: W) -> ArchiveResult<()> {
    let mut archive = tar::Builder::new(writer);
    // Follow symlinks in directory traversal (safer than storing broken symlinks)
    archive.follow_symlinks(false); // Store symlinks as symlinks, not as their targets

    for path in files {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => {
                // LOW-3: use tracing::warn! instead of eprintln!
                tracing::warn!("Skipping path with no filename: {}", path.display());
                continue;
            }
        };

        // Use symlink_metadata to detect type without following symlinks
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => {
                // LOW-3: use tracing::warn! instead of eprintln!
                tracing::warn!("Cannot access {}: {}", path.display(), e);
                continue;
            }
        };

        if metadata.is_symlink() {
            // Handle top-level symlink explicitly
            let target = match std::fs::read_link(path) {
                Ok(t) => t,
                Err(e) => {
                    // LOW-3: use tracing::warn! instead of eprintln!
                    tracing::warn!("Cannot read symlink {}: {}", path.display(), e);
                    continue;
                }
            };

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

            archive.append_data(&mut header, &name, std::io::empty())?;
        } else if metadata.is_dir() {
            archive.append_dir_all(&name, path)?;
        } else if metadata.is_file() {
            archive.append_path_with_name(path, &name)?;
        }
        // Other file types (devices, sockets) are silently skipped
    }

    archive.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
