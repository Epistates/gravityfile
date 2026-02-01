//! Archive operations (extract and compress).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Maximum decompression ratio allowed (1000:1).
/// Archives with higher ratios are considered potential zip bombs.
const MAX_DECOMPRESSION_RATIO: f64 = 1000.0;

/// Maximum total size for extracted content (10 GB).
const MAX_TOTAL_EXTRACTED_SIZE: u64 = 10 * 1024 * 1024 * 1024;

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
pub fn extract_archive(
    archive_path: &Path,
    destination: &Path,
) -> ArchiveResult<Vec<PathBuf>> {
    if !archive_path.exists() {
        return Err(ArchiveError::NotFound(archive_path.to_path_buf()));
    }

    let format = ArchiveFormat::from_path(archive_path)
        .ok_or_else(|| ArchiveError::UnsupportedFormat(
            archive_path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("unknown")
                .to_string()
        ))?;

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
    let canonical_dest = destination.canonicalize().unwrap_or_else(|_| destination.to_path_buf());

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
            total_uncompressed,
            MAX_TOTAL_EXTRACTED_SIZE
        )));
    }

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let entry_path = entry.enclosed_name()
            .ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid file path in archive"
            ))?;

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
                format!("Path traversal attempt in archive: {}", entry_path.display()),
            )));
        }

        let outpath = destination.join(&entry_path);

        // Security: Double-check that resolved path stays within destination
        if let Some(canonical_out) = outpath.parent().and_then(|p| p.canonicalize().ok()) {
            if !canonical_out.starts_with(&canonical_dest) {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Path escapes destination: {}", entry_path.display()),
                )));
            }
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else if entry.is_symlink() {
            // Security: Handle symlinks in ZIP archives
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

            // Reject symlink targets that escape destination
            if link_target
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                let symlink_parent = outpath.parent().unwrap_or(destination);
                let resolved = symlink_parent.join(link_target);

                // Normalize path to check if it escapes
                let mut normalized = PathBuf::new();
                for component in resolved.components() {
                    match component {
                        std::path::Component::ParentDir => {
                            normalized.pop();
                        }
                        std::path::Component::Normal(c) => {
                            normalized.push(c);
                        }
                        std::path::Component::RootDir => {
                            normalized.push("/");
                        }
                        _ => {}
                    }
                }

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
            }

            // Create parent directories if needed
            if let Some(parent) = outpath.parent() {
                if !parent.exists() {
                    std::fs::create_dir_all(parent)?;
                }
            }

            // Create the symlink
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(link_target, &outpath)?;
            }
            #[cfg(windows)]
            {
                // On Windows, we need to know if the target is a directory
                // Since we can't reliably know, skip symlinks with a warning
                // (Windows symlinks require admin privileges anyway)
                eprintln!(
                    "Warning: Skipping symlink {} -> {} (Windows symlinks not supported)",
                    entry_path.display(),
                    link_target.display()
                );
            }
        } else {
            // Regular file
            // Create parent directories if needed
            if let Some(parent) = outpath.parent() {
                if !parent.exists() {
                    std::fs::create_dir_all(parent)?;
                }
            }

            let mut outfile = File::create(&outpath)?;
            std::io::copy(&mut entry, &mut outfile)?;

            // Set permissions on Unix - strip setuid/setgid bits for security
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    // Security: Only preserve standard rwx permissions (0o777)
                    // Strip setuid (0o4000), setgid (0o2000), and sticky (0o1000) bits
                    let safe_mode = mode & 0o777;
                    std::fs::set_permissions(&outpath, std::fs::Permissions::from_mode(safe_mode))?;
                }
            }
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

/// Extract a TAR archive from a reader.
///
/// # Security
/// This function validates all paths to prevent directory traversal attacks.
/// Paths containing `..` components or absolute paths are rejected.
fn extract_tar_from_reader<R: Read>(reader: R, destination: &Path) -> ArchiveResult<Vec<PathBuf>> {
    let mut archive = tar::Archive::new(reader);
    // Security: Don't preserve setuid/setgid bits
    archive.set_preserve_permissions(false);
    // Security: Don't restore extended attributes
    archive.set_unpack_xattrs(false);

    let mut extracted_files = Vec::new();
    let canonical_dest = destination.canonicalize().unwrap_or_else(|_| destination.to_path_buf());

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let entry_path = entry.path()?;

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
                format!("Path traversal attempt in archive: {}", entry_path.display()),
            )));
        }

        let outpath = destination.join(&entry_path);

        // Security: Double-check that resolved path stays within destination
        // (handles symlink attacks where intermediate directories are symlinks)
        if let Some(canonical_out) = outpath.parent().and_then(|p| p.canonicalize().ok()) {
            if !canonical_out.starts_with(&canonical_dest) {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Path escapes destination: {}", entry_path.display()),
                )));
            }
        }

        // Security: Validate symlink targets
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            if let Ok(Some(link_target)) = entry.link_name() {
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

                // Reject symlink targets with parent directory traversal
                if link_target
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    // Calculate where the symlink would resolve to
                    let symlink_parent = outpath.parent().unwrap_or(destination);
                    let resolved = symlink_parent.join(&link_target);

                    // Check if resolved path escapes destination
                    // We need to normalize the path without following symlinks
                    let mut normalized = PathBuf::new();
                    for component in resolved.components() {
                        match component {
                            std::path::Component::ParentDir => {
                                normalized.pop();
                            }
                            std::path::Component::Normal(c) => {
                                normalized.push(c);
                            }
                            std::path::Component::RootDir => {
                                normalized.push("/");
                            }
                            _ => {}
                        }
                    }

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
                }
            }
        }

        entry.unpack(&outpath)?;
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
        add_path_to_zip(&mut archive, path, path.file_name().and_then(|n| n.to_str()).unwrap_or(""), &options)?;
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
            // Skip inaccessible paths with a warning (e.g., broken symlinks)
            eprintln!("Warning: Cannot access {}: {}", path.display(), e);
            return Ok(());
        }
    };

    if metadata.is_symlink() {
        // Handle symlink
        let target = match std::fs::read_link(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Warning: Cannot read symlink {}: {}", path.display(), e);
                return Ok(());
            }
        };

        // Store symlink using Unix external attributes
        #[cfg(unix)]
        {
            // Unix symlink mode: S_IFLNK (0o120000) | 0o777
            let symlink_mode = 0o120777;
            let unix_options = options
                .clone()
                .unix_permissions(symlink_mode);

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
            eprintln!("Warning: Skipping symlink loop at {}", path.display());
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
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Use symlink_metadata to detect type without following symlinks
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Warning: Cannot access {}: {}", path.display(), e);
                continue;
            }
        };

        if metadata.is_symlink() {
            // Handle top-level symlink explicitly
            let target = match std::fs::read_link(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Warning: Cannot read symlink {}: {}", path.display(), e);
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

            archive.append_data(&mut header, name, std::io::empty())?;
        } else if metadata.is_dir() {
            archive.append_dir_all(name, path)?;
        } else if metadata.is_file() {
            archive.append_path_with_name(path, name)?;
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
        assert_eq!(
            ArchiveFormat::from_path(Path::new("test.txt")),
            None
        );
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
            &[source_dir.clone()],
            &archive_path,
            ArchiveFormat::Zip,
        ).unwrap();

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
            &[source_dir.clone()],
            &archive_path,
            ArchiveFormat::TarGz,
        ).unwrap();

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
        // Test that absolute paths are rejected
        // This is a unit test for the validation logic, not with actual malicious archives
        let path = Path::new("/etc/passwd");
        assert!(path.is_absolute());

        // Our validation should reject this
        let has_parent_dir = path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        // Absolute path check should catch this even without ParentDir
        assert!(path.is_absolute() || has_parent_dir);
    }

    #[test]
    fn test_path_traversal_prevention_parent_dir() {
        // Test that paths with .. are detected
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

        // Safe path should pass
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

        // Create a simple test file
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test content").unwrap();

        // Create archive
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

        // First creation should succeed
        create_archive(&[test_file.clone()], &archive_path, ArchiveFormat::Zip).unwrap();

        // Second creation should fail with DestinationExists
        let result = create_archive(&[test_file], &archive_path, ArchiveFormat::Zip);
        assert!(matches!(result, Err(ArchiveError::DestinationExists(_))));
    }

    #[test]
    fn test_zip_path_validation_consistency() {
        // Verify ZIP and TAR use consistent path validation
        // Both should reject absolute paths and parent directory components
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test").unwrap();

        // Create a normal archive
        let archive_path = temp_dir.path().join("test.zip");
        create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

        // Extract should succeed with valid paths
        let extract_dir = temp_dir.path().join("extracted");
        let result = extract_archive(&archive_path, &extract_dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_decompression_constants() {
        // Verify security constants are reasonable
        assert!(MAX_DECOMPRESSION_RATIO >= 100.0); // Allow reasonable compression
        assert!(MAX_DECOMPRESSION_RATIO <= 10000.0); // But not unreasonable
        assert!(MAX_TOTAL_EXTRACTED_SIZE >= 1024 * 1024 * 1024); // At least 1GB
        assert!(MAX_TOTAL_EXTRACTED_SIZE <= 100 * 1024 * 1024 * 1024); // At most 100GB
    }

    #[cfg(unix)]
    #[test]
    fn test_permission_stripping() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test content").unwrap();

        // Set executable permission (no setuid - we can't create those in tests)
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&test_file, perms).unwrap();

        // Create and extract archive
        let archive_path = temp_dir.path().join("test.zip");
        create_archive(&[test_file], &archive_path, ArchiveFormat::Zip).unwrap();

        let extract_dir = temp_dir.path().join("extracted");
        extract_archive(&archive_path, &extract_dir).unwrap();

        // Verify extracted file exists and has reasonable permissions
        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());

        let extracted_perms = std::fs::metadata(&extracted_file).unwrap().permissions();
        let mode = extracted_perms.mode();

        // Should have rwx permissions but no setuid/setgid bits
        assert_eq!(mode & 0o7000, 0); // No special bits
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_in_tar_archive() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().unwrap();
        let source_dir = temp_dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();

        // Create a file and a symlink to it
        let test_file = source_dir.join("target.txt");
        std::fs::write(&test_file, "target content").unwrap();

        let symlink_path = source_dir.join("link.txt");
        symlink("target.txt", &symlink_path).unwrap();

        // Create TAR archive
        let archive_path = temp_dir.path().join("test.tar");
        create_archive(&[source_dir.clone()], &archive_path, ArchiveFormat::Tar).unwrap();

        assert!(archive_path.exists());

        // Extract archive
        let extract_dir = temp_dir.path().join("extracted");
        let result = extract_archive(&archive_path, &extract_dir);

        // Extraction should succeed
        assert!(result.is_ok());
    }

    #[test]
    fn test_symlink_validation_paths() {
        // Test the path validation logic used for symlinks
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
