//! Archive creation.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use super::{ArchiveError, ArchiveFormat, ArchiveResult, check_cancel};
use crate::fs::{
    RenameError, canonicalize_lenient, is_within_directory, rename_noreplace, staging_dir,
};

/// Create an archive at `archive_path` from files and directories.
///
/// Directories are added recursively; symlinks are stored as links. The
/// archive is written to private staging and published only when complete, so
/// a failure never leaves a corrupt archive behind. On Unix the archive is
/// owner-only (`0600`) because it can aggregate private files.
///
/// # Errors
///
/// [`ArchiveError::DestinationExists`] if `archive_path` exists; I/O errors
/// for unreadable sources or an unwritable destination.
///
/// # Example
///
/// ```no_run
/// use std::path::PathBuf;
/// use gravityfile_ops::{create_archive, ArchiveFormat};
///
/// create_archive(
///     &[PathBuf::from("src/"), PathBuf::from("Cargo.toml")],
///     &PathBuf::from("backup.tar.zst"),
///     ArchiveFormat::TarZst,
/// )?;
/// # Ok::<(), gravityfile_ops::ArchiveError>(())
/// ```
pub fn create_archive(
    files: &[PathBuf],
    archive_path: &Path,
    format: ArchiveFormat,
) -> ArchiveResult<()> {
    create_archive_cancellable(files, archive_path, format, &CancellationToken::new())
}

/// [`create_archive`] with cooperative cancellation up to publication.
pub fn create_archive_cancellable(
    files: &[PathBuf],
    archive_path: &Path,
    format: ArchiveFormat,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
    check_cancel(cancel)?;
    if std::fs::symlink_metadata(archive_path).is_ok() {
        return Err(ArchiveError::DestinationExists(archive_path.to_path_buf()));
    }
    let invalid = |message: String| {
        ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            message,
        ))
    };
    let mut names = std::collections::HashSet::new();
    for source in files {
        check_cancel(cancel)?;
        let name = source.file_name().ok_or_else(|| {
            invalid(format!(
                "Archive source '{}' has no filename",
                source.display()
            ))
        })?;
        if !names.insert(name.to_os_string()) {
            return Err(invalid(format!(
                "Archive sources share the entry name '{}'",
                name.to_string_lossy()
            )));
        }
        let same_path = matches!(
            (canonicalize_lenient(source), canonicalize_lenient(archive_path)),
            (Some(source), Some(destination)) if source == destination
        );
        if same_path || is_within_directory(source, archive_path) {
            return Err(invalid(format!(
                "Archive destination '{}' cannot be inside source '{}'",
                archive_path.display(),
                source.display()
            )));
        }
    }

    let staging = staging_dir(archive_path, ".gravityfile-archive-")?;
    let staged_path = staging.path().join("payload");
    let file = create_archive_file(&staged_path, cancel)?;
    let result = match format {
        ArchiveFormat::Zip => create_zip(files, file, cancel),
        ArchiveFormat::Tar => create_tar_to_writer(files, file).map(|_| ()),
        compressed => create_compressed_tar(files, file, compressed).map(|_| ()),
    };
    if let Err(error) = result {
        return Err(if cancel.is_cancelled() {
            ArchiveError::Cancelled
        } else {
            error
        });
    }
    check_cancel(cancel)?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&staged_path)?
        .sync_all()?;
    rename_noreplace(&staged_path, archive_path).map_err(|error| match error {
        RenameError::DestinationExists => {
            ArchiveError::DestinationExists(archive_path.to_path_buf())
        }
        other => ArchiveError::Io(other.into()),
    })?;
    if let Err(error) = staging.close() {
        tracing::warn!(%error, "archive published; staging cleanup incomplete");
    }
    Ok(())
}

pub(super) struct ArchiveOutput {
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
pub(super) fn create_archive_file(
    path: &Path,
    cancel: &CancellationToken,
) -> std::io::Result<ArchiveOutput> {
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

fn create_zip(
    files: &[PathBuf],
    file: ArchiveOutput,
    cancel: &CancellationToken,
) -> ArchiveResult<()> {
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

pub(super) fn zip_utf8(value: &std::ffi::OsStr) -> ArchiveResult<&str> {
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
    check_cancel(cancel)?;
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
        // ZIP64 must be declared up front for entries of 4 GiB or more.
        let file_options = options.large_file(metadata.len() >= u64::from(u32::MAX));
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

pub(super) fn copy_zip_source(
    file: &mut File,
    inspected: &std::fs::Metadata,
    writer: &mut impl Write,
) -> std::io::Result<()> {
    if !crate::fs::unchanged_file(inspected, &file.metadata()?) {
        return Err(std::io::Error::other("ZIP source changed before reading"));
    }
    // Never chase a growing file. No bytes beyond the inspected length may
    // enter the staged archive, even if the writer appends concurrently.
    let copied = std::io::copy(&mut (&mut *file).take(inspected.len()), writer)?;
    if copied != inspected.len() || !crate::fs::unchanged_file(inspected, &file.metadata()?) {
        return Err(std::io::Error::other("ZIP source changed while reading"));
    }
    Ok(())
}

pub(super) fn open_zip_source(path: &Path, inspected: &std::fs::Metadata) -> std::io::Result<File> {
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

pub(super) fn create_compressed_tar<W: Write>(
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
        ArchiveFormat::TarZst => {
            let mut encoder = zstd::stream::write::Encoder::new(writer, 3)?;
            encoder
                .multithread(std::thread::available_parallelism().map_or(1, |n| n.get() as u32))?;
            create_tar_to_writer(files, encoder)?.finish()?
        }
        ArchiveFormat::Zip | ArchiveFormat::Tar => {
            return Err(ArchiveError::UnsupportedFormat(format.extension().into()));
        }
    })
}

/// Create a TAR archive to a writer.
///
/// Handles regular files, directories, and symlinks.
/// The `tar::Builder::append_dir_all` method handles symlinks within directories.
pub(super) fn create_tar_to_writer<W: Write>(files: &[PathBuf], writer: W) -> ArchiveResult<W> {
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
