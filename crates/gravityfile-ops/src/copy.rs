//! Async copy operation with progress reporting.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conflict::{
    Conflict, ConflictKind, ConflictRequest, ConflictResolution, auto_rename_path,
    request_resolution,
};
use crate::progress::publish_progress;
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Result sent through the channel during copy operations.
#[derive(Debug)]
pub enum CopyResult {
    /// Progress update.
    Progress(OperationProgress),
    /// A conflict was detected. The operation is paused until the request is
    /// answered (or dropped, which skips the item).
    Conflict(ConflictRequest),
    /// The operation completed.
    Complete(CopyComplete),
}

/// Completion result for copy operations, including the created paths
/// needed by the undo system.
#[derive(Debug)]
pub struct CopyComplete {
    /// Standard operation completion info.
    pub inner: OperationComplete,
    /// List of all created files and directories for undo recording.
    pub created_paths: Vec<PathBuf>,
}

/// Options for copy operations.
#[derive(Debug, Clone, Default)]
pub struct CopyOptions {
    /// How to handle conflicts (None means ask for each).
    pub conflict_resolution: Option<ConflictResolution>,
    /// Whether to preserve timestamps.
    pub preserve_timestamps: bool,
}

/// Start an async copy operation.
///
/// Returns a receiver for progress updates and results.
pub fn start_copy(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: CopyOptions,
    token: CancellationToken,
) -> mpsc::Receiver<CopyResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    if sources.is_empty() {
        // Send immediate completion for empty sources
        let complete = CopyComplete {
            inner: OperationComplete {
                cancelled: token.is_cancelled(),
                operation_type: OperationType::Copy,
                succeeded: 0,
                failed: 0,
                skipped: 0,
                bytes_processed: 0,
                errors: vec![],
            },
            created_paths: vec![],
        };
        tokio::spawn(async move {
            let _ = tx.send(CopyResult::Complete(complete)).await;
        });
        return rx;
    }

    tokio::spawn(async move {
        let receiver_closed = tx.clone();
        let work = copy_impl(sources, destination, options, token.clone(), tx);
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = receiver_closed.closed() => {
                token.cancel();
                // Finish cooperative cleanup; dropping the future would detach
                // an in-flight blocking filesystem operation.
                work.await;
            }
            _ = &mut work => {}
        }
    });

    rx
}

/// Internal implementation of copy operation.
async fn copy_impl(
    sources: Vec<PathBuf>,
    destination: PathBuf,
    options: CopyOptions,
    token: CancellationToken,
    tx: mpsc::Sender<CopyResult>,
) {
    // Calculating totals walks every source tree, so it must not run on an
    // async worker thread.
    let (total_files, total_bytes) = {
        let sources = sources.clone();
        let token = token.clone();
        tokio::task::spawn_blocking(move || calculate_totals(&sources, &token))
            .await
            .unwrap_or((0, 0))
    };

    let mut progress = OperationProgress::new(OperationType::Copy, total_files, total_bytes);
    let mut global_resolution: Option<ConflictResolution> = options.conflict_resolution;
    let mut succeeded = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut created_paths = Vec::new();

    // Creating the destination before this validation would itself mutate a
    // source tree and make a later recursive copy discover its own output.
    if let Some(source) = sources
        .iter()
        .find(|source| destination_is_within_directory(source, &destination))
    {
        progress.add_error(OperationError::new(
            source.clone(),
            format!(
                "Cannot copy a directory into itself: destination '{}' is inside the source",
                destination.display()
            ),
        ));
        let _ = tx
            .send(CopyResult::Complete(CopyComplete {
                inner: OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Copy,
                    succeeded: 0,
                    failed: sources.len(),
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors,
                },
                created_paths,
            }))
            .await;
        return;
    }

    // Ensure destination exists and is a directory
    if !destination.exists()
        && let Err(e) = fs::create_dir_all(&destination)
    {
        progress.add_error(OperationError::new(
            destination.clone(),
            format!("Failed to create destination: {}", e),
        ));
        let _ = tx
            .send(CopyResult::Complete(CopyComplete {
                inner: OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Copy,
                    succeeded: 0,
                    failed: sources.len(),
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors.clone(),
                },
                created_paths: vec![],
            }))
            .await;
        return;
    }
    if !fs::metadata(&destination).is_ok_and(|metadata| metadata.is_dir()) {
        progress.add_error(OperationError::new(
            destination.clone(),
            "Copy destination is not an accessible directory".to_string(),
        ));
        let _ = tx
            .send(CopyResult::Complete(CopyComplete {
                inner: OperationComplete {
                    cancelled: token.is_cancelled(),
                    operation_type: OperationType::Copy,
                    succeeded: 0,
                    failed: sources.len(),
                    skipped: 0,
                    bytes_processed: 0,
                    errors: progress.errors,
                },
                created_paths,
            }))
            .await;
        return;
    }

    for source in sources {
        // HIGH-3: check for cancellation before each item
        if token.is_cancelled() {
            break;
        }

        // MED-5: return error when file_name() is None (e.g. path is "/" or ends with "..")
        let file_name = match source.file_name() {
            Some(n) => n.to_owned(),
            None => {
                progress.add_error(OperationError::new(
                    source.clone(),
                    "Source path has no filename component".to_string(),
                ));
                failed += 1;
                continue;
            }
        };
        let dest_path = destination.join(&file_name);

        // Source and destination resolving to the same path can never be
        // satisfied by any resolution, so it is surfaced and always skipped.
        if source == dest_path {
            let resolution = request_resolution(
                &tx,
                Conflict::new(source.clone(), dest_path.clone(), ConflictKind::SameFile),
                CopyResult::Conflict,
                &token,
            )
            .await;
            if resolution == ConflictResolution::Abort {
                token.cancel();
                break;
            }
            skipped += 1;
            continue;
        }

        // Check for conflicts using symlink_metadata so we see the link itself
        let dest_meta = fs::symlink_metadata(&dest_path).ok();
        let final_dest = if dest_meta.is_some() {
            let conflict_kind = if dest_meta.as_ref().is_some_and(|m| m.is_dir()) {
                ConflictKind::DirectoryExists
            } else {
                ConflictKind::FileExists
            };

            let resolution = match global_resolution {
                Some(res) => res.to_single(),
                None => {
                    let answer = request_resolution(
                        &tx,
                        Conflict::new(source.clone(), dest_path.clone(), conflict_kind),
                        CopyResult::Conflict,
                        &token,
                    )
                    .await;
                    // "…All" answers apply to every remaining conflict.
                    if answer.is_global() {
                        global_resolution = Some(answer);
                    }
                    answer.to_single()
                }
            };

            match resolution {
                ConflictResolution::Skip | ConflictResolution::SkipAll => {
                    skipped += 1;
                    continue;
                }
                ConflictResolution::Abort => {
                    let _ = tx
                        .send(CopyResult::Complete(CopyComplete {
                            inner: OperationComplete {
                                cancelled: true,
                                operation_type: OperationType::Copy,
                                succeeded,
                                failed,
                                skipped,
                                bytes_processed: progress.bytes_processed,
                                errors: progress.errors.clone(),
                            },
                            created_paths,
                        }))
                        .await;
                    return;
                }
                ConflictResolution::AutoRename => (auto_rename_path(&dest_path), false),
                // Overwrite publishes a complete staged copy through a recovery
                // journal that retains the original on failed publication.
                ConflictResolution::Overwrite | ConflictResolution::OverwriteAll => {
                    (dest_path.clone(), true)
                }
            }
        } else {
            (dest_path.clone(), false)
        };

        let (final_dest, overwrite) = final_dest;

        // Perform the copy
        progress.set_current_file(Some(source.clone()));
        publish_progress(&tx, || CopyResult::Progress(progress.clone()), &token);

        if let Err(e) = copy_item(
            &source,
            &final_dest,
            overwrite,
            options.preserve_timestamps,
            &mut progress,
            &tx,
            &token,
        )
        .await
        {
            progress.add_error(OperationError::new(source.clone(), e));
            failed += 1;
        } else {
            succeeded += 1;
            // An overwritten destination existed before this operation. The
            // current undo model can safely remove only newly created paths;
            // recording an overwrite here would make undo delete user data.
            if !overwrite {
                created_paths.push(final_dest);
            }
        }
    }

    // Send completion
    let _ = tx
        .send(CopyResult::Complete(CopyComplete {
            inner: OperationComplete {
                cancelled: token.is_cancelled(),
                operation_type: OperationType::Copy,
                succeeded,
                failed,
                skipped,
                bytes_processed: progress.bytes_processed,
                errors: progress.errors,
            },
            created_paths,
        }))
        .await;
}

/// Copy a single item (file, directory, or symlink).
///
/// When `overwrite` is set and `dest` already exists, the copy is staged into a
/// private staging directory before journaled publication. A mid-copy failure
/// cannot destroy the pre-existing destination.
async fn copy_item(
    source: &Path,
    dest: &Path,
    overwrite: bool,
    preserve_timestamps: bool,
    progress: &mut OperationProgress,
    tx: &mpsc::Sender<CopyResult>,
    token: &CancellationToken,
) -> Result<(), String> {
    let source = source.to_path_buf();
    let dest = dest.to_path_buf();
    let work_token = token.clone();

    let result = tokio::task::spawn_blocking(move || {
        if overwrite && fs::symlink_metadata(&dest).is_ok() {
            staged_overwrite_copy(&source, &dest, preserve_timestamps, &work_token)
        } else {
            staged_new_copy(&source, &dest, preserve_timestamps, &work_token)
        }
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?;

    match result {
        Ok(bytes) => {
            progress.complete_file(bytes);
            publish_progress(tx, || CopyResult::Progress(progress.clone()), token);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn copy_path_blocking_with_options(
    source: &Path,
    dest: &Path,
    preserve_timestamps: bool,
    token: &CancellationToken,
) -> Result<u64, String> {
    if token.is_cancelled() {
        return Err(CANCELLED_MESSAGE.to_string());
    }
    let metadata =
        fs::symlink_metadata(source).map_err(|e| format!("Failed to read metadata: {}", e))?;

    if metadata.is_symlink() {
        let target = fs::read_link(source).map_err(|e| format!("Failed to read symlink: {}", e))?;
        recreate_symlink(&target, source, dest)?;
        if preserve_timestamps {
            preserve_times(source, dest, true)?;
        }
        Ok(0) // Symlinks have no real size
    } else if metadata.is_dir() {
        copy_dir_recursive_with_options(
            source,
            dest,
            &mut HashSet::new(),
            preserve_timestamps,
            token,
        )
    } else {
        copy_file(source, dest, preserve_timestamps, token)
    }
}

/// Stage a new copy beside its destination so an interrupted or failed copy
/// never leaves a partial file or directory at the user-visible path.
pub(crate) fn staged_new_copy(
    source: &Path,
    dest: &Path,
    preserve_timestamps: bool,
    token: &CancellationToken,
) -> Result<u64, String> {
    let staging = create_staging_directory(dest)?;
    let temp = staging.path().join("payload");
    let result = (|| {
        let bytes = copy_path_blocking_with_options(source, &temp, preserve_timestamps, token)?;
        if token.is_cancelled() {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        if fs::symlink_metadata(dest).is_ok() {
            return Err(format!(
                "Destination appeared while copying: '{}'",
                dest.display()
            ));
        }
        rename_noreplace(&temp, dest)
            .map_err(|error| format!("Failed to finalize copy: {error}"))?;
        Ok(bytes)
    })();
    finish_copy_staging(staging, result)
}

/// Stage a complete copy, then publish through a persistent overwrite journal.
/// Failed publication retains recovery paths; a process crash never triggers
/// automatic destruction of the saved original.
fn staged_overwrite_copy(
    source: &Path,
    dest: &Path,
    preserve_timestamps: bool,
    token: &CancellationToken,
) -> Result<u64, String> {
    let staging = create_staging_directory(dest)?;
    let temp = staging.path().join("payload");

    let result = (|| {
        let bytes = copy_path_blocking_with_options(source, &temp, preserve_timestamps, token)?;
        if token.is_cancelled() {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        let mut journal = crate::recovery::OverwriteJournal::begin(dest, None)
            .map_err(|error| format!("Failed to reserve overwrite journal: {error}"))?;
        rename_noreplace(&temp, &journal.payload()).map_err(|error| journal.error(error))?;
        journal.publish()?;
        let recovery_path = journal.path().to_path_buf();
        if let Err(error) = journal.finish() {
            tracing::warn!(path = %recovery_path.display(), %error, "Overwrite succeeded; recovery cleanup incomplete");
        }

        Ok(bytes)
    })();
    finish_copy_staging(staging, result)
}

fn finish_copy_staging(
    staging: tempfile::TempDir,
    result: Result<u64, String>,
) -> Result<u64, String> {
    match result {
        Ok(bytes) => {
            let path = staging.path().to_owned();
            if let Err(error) = staging.close() {
                tracing::warn!(path = %path.display(), %error, "Copy succeeded; staging directory cleanup incomplete");
            }
            Ok(bytes)
        }
        Err(error) => Err(cleanup_failed_copy(staging, error, |path| {
            fs::remove_dir_all(path)
        })),
    }
}

fn cleanup_failed_copy(
    staging: tempfile::TempDir,
    error: String,
    cleanup: impl FnOnce(&Path) -> std::io::Result<()>,
) -> String {
    // Disable Drop's silent second cleanup attempt. A failed removal can be
    // partial; retain its remaining contents and identify the exact location.
    let path = staging.keep();
    match cleanup(&path) {
        Ok(()) => error,
        Err(cleanup_error) => format!(
            "{error}; staging cleanup failed: {cleanup_error}; inspect staging path '{}' for remaining data (contents may be partial)",
            path.display()
        ),
    }
}

/// Reserve a private directory atomically, on the destination filesystem.
/// An unreserved temporary filename lets another process substitute a symlink
/// before `fs::copy` opens it, potentially truncating an unrelated file.
fn create_staging_directory(dest: &Path) -> Result<tempfile::TempDir, String> {
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    crate::staging::private_directory(parent, ".gravityfile-stage-")
        .map_err(|error| format!("Failed to reserve staging directory: {error}"))
}

/// Whether `destination` is the same as or nested below a directory `source`.
///
/// Existing prefixes are canonicalized so symlink aliases cannot bypass the
/// check. A non-existent suffix is then appended lexically.
pub(crate) fn destination_is_within_directory(source: &Path, destination: &Path) -> bool {
    if !fs::symlink_metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
        return false;
    }

    match (
        canonicalize_with_missing_suffix(source),
        canonicalize_with_missing_suffix(destination),
    ) {
        (Some(source), Some(destination)) => destination.starts_with(source),
        _ => false,
    }
}

pub(crate) fn canonicalize_with_missing_suffix(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = path.canonicalize() {
        return Some(path);
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();

    while let Some(name) = existing.file_name() {
        suffix.push(name.to_owned());
        existing = existing.parent()?;
        if let Ok(mut canonical) = existing.canonicalize() {
            for component in suffix.iter().rev() {
                canonical.push(component);
            }
            return Some(canonical);
        }
    }

    None
}

/// Recreate `target` as a symlink at `dest`. `source` is the original link,
/// used on Windows to decide between a file and directory symlink.
pub(crate) fn recreate_symlink(target: &Path, source: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let _ = source;
        std::os::unix::fs::symlink(target, dest)
            .map_err(|e| format!("Failed to create symlink: {}", e))
    }
    #[cfg(windows)]
    {
        // Mirror the original link kind based on what it pointed at.
        let result = if source.is_dir() {
            std::os::windows::fs::symlink_dir(target, dest)
        } else {
            std::os::windows::fs::symlink_file(target, dest)
        };
        result.map_err(|e| format!("Failed to create symlink: {}", e))
    }
}

/// Remove a path whether it is a file, symlink, or directory (no symlink follow).
pub(crate) fn remove_path(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Why a no-replace rename did not succeed.
///
/// Callers must distinguish these: treating "destination exists" as a generic
/// failure and falling back to a copy silently destroys the existing file.
#[derive(Debug)]
pub(crate) enum RenameError {
    /// Something already exists at the destination. Never clobber it.
    DestinationExists,
    /// Source and destination are on different filesystems; the caller has to
    /// fall back to copy + delete.
    CrossDevice,
    /// Any other I/O failure.
    Io(std::io::Error),
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DestinationExists => f.write_str("destination already exists"),
            Self::CrossDevice => f.write_str("source and destination are on different filesystems"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RenameError {}

impl RenameError {
    fn classify(error: std::io::Error) -> Self {
        if is_cross_device(&error) {
            Self::CrossDevice
        } else if error.kind() == std::io::ErrorKind::AlreadyExists {
            Self::DestinationExists
        } else {
            Self::Io(error)
        }
    }
}

#[cfg(unix)]
fn is_cross_device(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::EXDEV)
}

#[cfg(windows)]
fn is_cross_device(error: &std::io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE
    error.raw_os_error() == Some(17)
}

#[cfg(not(any(unix, windows)))]
fn is_cross_device(_error: &std::io::Error) -> bool {
    false
}

/// True when the failure means the platform or filesystem cannot perform an
/// atomic no-replace rename at all (as opposed to refusing this specific one).
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_flag_unsupported(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == libc::ENOSYS || code == libc::EINVAL || code == libc::EOPNOTSUPP
    )
}

/// Atomic no-replace rename primitive.
///
/// `None` means this platform/filesystem pair has no atomic primitive, so the
/// caller must fail closed.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_noreplace_atomic(source: &Path, destination: &Path) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let Ok(source) = CString::new(source.as_os_str().as_bytes()) else {
        return Some(Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in source path",
        )));
    };
    let Ok(destination) = CString::new(destination.as_os_str().as_bytes()) else {
        return Some(Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in destination path",
        )));
    };

    // SAFETY: both C strings are NUL-terminated and remain alive for the call.
    let result = unsafe {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        }
        #[cfg(target_vendor = "apple")]
        {
            libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL)
        }
    };

    if result == 0 {
        return Some(Ok(()));
    }
    let error = std::io::Error::last_os_error();
    if rename_flag_unsupported(&error) {
        // e.g. pre-3.15 kernels, NFS, SMB, exFAT, many FUSE filesystems.
        return None;
    }
    Some(Err(error))
}

#[cfg(windows)]
fn rename_noreplace_atomic(source: &Path, destination: &Path) -> Option<std::io::Result<()>> {
    use std::os::windows::ffi::OsStrExt;

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if source[..source.len() - 1].contains(&0) || destination[..destination.len() - 1].contains(&0)
    {
        return Some(Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in rename path",
        )));
    }
    // SAFETY: these UTF-16 buffers are NUL-terminated and live for the call.
    // Zero flags explicitly excludes MOVEFILE_REPLACE_EXISTING and COPY_ALLOWED:
    // a destination collision or cross-device move must be reported to callers.
    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            0,
        )
    };
    Some(if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    })
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    windows
)))]
fn rename_noreplace_atomic(_source: &Path, _destination: &Path) -> Option<std::io::Result<()>> {
    None
}

/// Rename `source` to `destination`, never replacing an existing destination.
///
/// Uses the platform's atomic primitive where available. When the filesystem
/// does not support it, fail closed. A check followed by an ordinary Unix
/// rename cannot uphold this contract against concurrent writers.
pub(crate) fn rename_noreplace(source: &Path, destination: &Path) -> Result<(), RenameError> {
    match rename_noreplace_atomic(source, destination) {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(RenameError::classify(error)),
        None => Err(RenameError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem does not support atomic no-replace rename",
        ))),
    }
}

/// Copy a regular file through validated handles in bounded, cancellable chunks.
fn copy_file(
    source: &Path,
    dest: &Path,
    preserve_timestamps: bool,
    token: &CancellationToken,
) -> Result<u64, String> {
    let result = (|| -> std::io::Result<u64> {
        let before = fs::symlink_metadata(source)?;
        if !before.is_file() {
            return Err(std::io::Error::other("copy source is not a regular file"));
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut input = options.open(source)?;
        let opened = input.metadata()?;
        if !same_copy_source(&before, &opened) {
            return Err(std::io::Error::other("copy source changed while opening"));
        }
        if token.is_cancelled() {
            return Err(std::io::Error::other(CANCELLED_MESSAGE));
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)?;
        #[cfg(unix)]
        copy_contents(
            &mut input,
            &mut SparseOutput(&mut output),
            opened.len(),
            token,
        )?;
        #[cfg(not(unix))]
        copy_contents(&mut input, &mut output, opened.len(), token)?;
        // Seeking across the last zero chunk does not extend the file itself.
        output.set_len(opened.len())?;
        if !same_copy_source(&opened, &input.metadata()?) {
            return Err(std::io::Error::other("copy source changed while reading"));
        }
        output.set_permissions(opened.permissions())?;
        if preserve_timestamps {
            filetime::set_file_handle_times(
                &output,
                Some(filetime::FileTime::from_last_access_time(&opened)),
                Some(filetime::FileTime::from_last_modification_time(&opened)),
            )?;
        }
        Ok(opened.len())
    })();
    result.map_err(|error| format!("Failed to copy: {error}"))
}

/// Skip all-zero chunks on filesystems supporting holes. This preserves bytes,
/// not the source's exact extent layout, and still reads the entire source.
#[cfg(unix)]
struct SparseOutput<'a>(&'a mut fs::File);

#[cfg(unix)]
impl std::io::Write for SparseOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use std::io::{Seek, SeekFrom};
        if bytes.iter().all(|&byte| byte == 0) {
            let distance = i64::try_from(bytes.len()).map_err(std::io::Error::other)?;
            self.0.seek(SeekFrom::Current(distance))?;
            Ok(bytes.len())
        } else {
            self.0.write(bytes)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub(crate) fn same_copy_source(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    if !after.is_file()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return false;
        }
    }
    true
}

fn copy_contents(
    input: &mut impl std::io::Read,
    output: &mut impl std::io::Write,
    length: u64,
    token: &CancellationToken,
) -> std::io::Result<()> {
    let mut buffer = vec![0; length.clamp(1, 256 * 1024) as usize];
    let mut remaining = length;
    while remaining > 0 {
        if token.is_cancelled() {
            return Err(std::io::Error::other(CANCELLED_MESSAGE));
        }
        let amount = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..amount])?;
        if token.is_cancelled() {
            return Err(std::io::Error::other(CANCELLED_MESSAGE));
        }
        output.write_all(&buffer[..amount])?;
        remaining -= amount as u64;
    }
    if token.is_cancelled() {
        return Err(std::io::Error::other(CANCELLED_MESSAGE));
    }
    let mut extra = [0];
    if input.read(&mut extra)? != 0 {
        return Err(std::io::Error::other("copy source grew while reading"));
    }
    Ok(())
}

fn preserve_times(source: &Path, dest: &Path, symlink: bool) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(source).map_err(|e| format!("Failed to read timestamps: {e}"))?;
    let atime = filetime::FileTime::from_last_access_time(&metadata);
    let mtime = filetime::FileTime::from_last_modification_time(&metadata);
    let result = if symlink {
        filetime::set_symlink_file_times(dest, atime, mtime)
    } else {
        filetime::set_file_times(dest, atime, mtime)
    };
    result.map_err(|e| format!("Failed to preserve timestamps: {e}"))
}

/// Message returned when a copy is aborted by its cancellation token.
pub(crate) const CANCELLED_MESSAGE: &str = "operation cancelled";

fn copy_dir_recursive_with_options(
    source: &Path,
    dest: &Path,
    visited: &mut HashSet<(u64, u64)>,
    preserve_timestamps: bool,
    token: &CancellationToken,
) -> Result<u64, String> {
    let mut total_bytes = 0u64;
    let mut pending = vec![(source.to_path_buf(), dest.to_path_buf())];
    let mut copied_directories = Vec::new();

    while let Some((source_dir, dest_dir)) = pending.pop() {
        // Cancellation is checked per directory and per entry so aborting a
        // large recursive copy takes effect promptly instead of only between
        // top-level sources. File contents are copied in bounded chunks with cancellation checks;
        // a blocked filesystem call itself cannot be preempted.
        if token.is_cancelled() {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        let metadata = fs::symlink_metadata(&source_dir)
            .map_err(|error| format!("Failed to read directory metadata: {error}"))?;
        if !metadata.is_dir() {
            return Err("Copy source directory changed type".to_string());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if !visited.insert((metadata.dev(), metadata.ino())) {
                return Err(format!(
                    "Repeated source directory identity at '{}'; refusing an incomplete copy",
                    source_dir.display()
                ));
            }
        }

        fs::create_dir_all(&dest_dir).map_err(|e| format!("Failed to create directory: {}", e))?;
        copied_directories.push((dest_dir.clone(), metadata));
        let entries =
            fs::read_dir(&source_dir).map_err(|e| format!("Failed to read directory: {}", e))?;

        for entry in entries {
            if token.is_cancelled() {
                return Err(CANCELLED_MESSAGE.to_string());
            }
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let file_type = entry
                .file_type()
                .map_err(|e| format!("Failed to read file type: {}", e))?;
            let path = entry.path();
            let dest_path = dest_dir.join(entry.file_name());

            if file_type.is_symlink() {
                let target =
                    fs::read_link(&path).map_err(|e| format!("Failed to read symlink: {}", e))?;
                recreate_symlink(&target, &path, &dest_path)?;
                if preserve_timestamps {
                    preserve_times(&path, &dest_path, true)?;
                }
            } else if file_type.is_dir() {
                pending.push((path, dest_path));
            } else {
                total_bytes = total_bytes.saturating_add(copy_file(
                    &path,
                    &dest_path,
                    preserve_timestamps,
                    token,
                )?);
            }
        }
    }

    // Populate every directory before restoring restrictive permissions. Work
    // bottom-up so parent permissions cannot prevent finalizing descendants.
    for (dest_dir, metadata) in copied_directories.into_iter().rev() {
        if token.is_cancelled() {
            return Err(CANCELLED_MESSAGE.to_string());
        }
        if preserve_timestamps {
            filetime::set_file_times(
                &dest_dir,
                filetime::FileTime::from_last_access_time(&metadata),
                filetime::FileTime::from_last_modification_time(&metadata),
            )
            .map_err(|error| format!("Failed to preserve directory timestamps: {error}"))?;
        }
        fs::set_permissions(&dest_dir, metadata.permissions())
            .map_err(|error| format!("Failed to preserve directory permissions: {error}"))?;
    }

    Ok(total_bytes)
}

/// Calculate total files and bytes for a list of sources.
///
/// HIGH-2: use symlink_metadata; skip symlinks in size calculations.
fn calculate_totals(sources: &[PathBuf], token: &CancellationToken) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;

    for source in sources {
        if token.is_cancelled() {
            break;
        }
        match fs::symlink_metadata(source) {
            Ok(meta) if meta.is_dir() => {
                let (f, b) = calculate_dir_totals(source, token);
                files = files.saturating_add(f);
                bytes = bytes.saturating_add(b);
            }
            Ok(meta) if !meta.is_symlink() => {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(meta.len());
            }
            _ => {} // skip symlinks and inaccessible entries
        }
    }

    (files, bytes)
}

/// Calculate totals for a directory recursively.
///
/// HIGH-2: use symlink_metadata; skip symlinks.
fn calculate_dir_totals(dir: &Path, token: &CancellationToken) -> (usize, u64) {
    calculate_dir_totals_with(dir, token, |path| fs::read_dir(path))
}

fn calculate_dir_totals_with<I>(
    dir: &Path,
    token: &CancellationToken,
    mut read_dir: impl FnMut(&Path) -> std::io::Result<I>,
) -> (usize, u64)
where
    I: Iterator<Item = std::io::Result<fs::DirEntry>>,
{
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut pending = vec![dir.to_path_buf()];

    while let Some(dir) = pending.pop() {
        if token.is_cancelled() {
            break;
        }
        let Ok(mut entries) = read_dir(&dir) else {
            continue;
        };
        while !token.is_cancelled() {
            let Some(entry) = entries.next() else { break };
            if token.is_cancelled() {
                return (files, bytes);
            }
            let Ok(entry) = entry else { continue };
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                pending.push(path);
            } else if let Ok(metadata) = fs::symlink_metadata(&path) {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }

    (files, bytes)
}

#[cfg(test)]
mod rename_contract_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn repeated_directory_identity_fails_instead_of_silently_omitting_contents() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let child = source.join("child");
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("important"), b"must not be omitted").unwrap();
        let destination = root.path().join("destination");
        let staging = create_staging_directory(&destination).unwrap();
        let staged_path = staging.path().to_owned();
        let metadata = fs::metadata(&child).unwrap();
        // Inject a previously observed identity without requiring directory
        // hard links or privileged bind mounts on the test host.
        let mut visited = HashSet::from([(metadata.dev(), metadata.ino())]);
        let result = copy_dir_recursive_with_options(
            &source,
            &staged_path.join("payload"),
            &mut visited,
            true,
            &CancellationToken::new(),
        );
        let error = finish_copy_staging(staging, result).unwrap_err();
        assert!(error.contains("Repeated source directory identity"));
        assert!(error.contains(&child.display().to_string()));
        assert!(!destination.exists());
        assert!(!staged_path.exists());
        assert_eq!(
            fs::read(child.join("important")).unwrap(),
            b"must not be omitted"
        );
    }

    #[test]
    fn failed_staging_cleanup_reports_and_retains_partial_contents() {
        let root = tempfile::tempdir().unwrap();
        let staging = create_staging_directory(&root.path().join("destination")).unwrap();
        let path = staging.path().to_owned();
        fs::write(path.join("removed"), b"partial").unwrap();
        fs::write(path.join("retained"), b"keep for inspection").unwrap();
        let error = cleanup_failed_copy(staging, "original copy failure".into(), |directory| {
            fs::remove_file(directory.join("removed"))?;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected cleanup failure",
            ))
        });
        assert!(error.contains("original copy failure"));
        assert!(error.contains("injected cleanup failure"));
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("contents may be partial"));
        // TempDir destruction must not silently retry and delete this evidence.
        assert_eq!(
            fs::read(path.join("retained")).unwrap(),
            b"keep for inspection"
        );
        assert!(!path.join("removed").exists());
    }

    #[test]
    fn successful_failed_copy_cleanup_preserves_original_error() {
        let root = tempfile::tempdir().unwrap();
        let staging = create_staging_directory(&root.path().join("destination")).unwrap();
        let path = staging.path().to_owned();
        fs::write(path.join("payload"), b"incomplete").unwrap();
        assert_eq!(
            finish_copy_staging(staging, Err(CANCELLED_MESSAGE.into())).unwrap_err(),
            CANCELLED_MESSAGE
        );
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn staged_directory_copy_preserves_modes_with_and_without_timestamps() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let child = source.join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("content"), b"private contents").unwrap();
        fs::set_permissions(child.join("content"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        let modified = filetime::FileTime::from_unix_time(1_600_000_000, 123_456_789);
        filetime::set_file_mtime(&source, modified).unwrap();
        filetime::set_file_mtime(&child, modified).unwrap();
        for preserve in [false, true] {
            let destination = root.path().join(format!("copy-{preserve}"));
            assert_eq!(
                staged_new_copy(&source, &destination, preserve, &CancellationToken::new())
                    .unwrap(),
                16
            );
            for (relative, mode) in [("", 0o700), ("child", 0o500), ("child/content", 0o600)] {
                assert_eq!(
                    fs::metadata(destination.join(relative))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o7777,
                    mode
                );
            }
            assert_eq!(
                fs::read(destination.join("child/content")).unwrap(),
                b"private contents"
            );
            if preserve {
                for relative in ["", "child"] {
                    assert_eq!(
                        filetime::FileTime::from_last_modification_time(
                            &fs::metadata(destination.join(relative)).unwrap()
                        ),
                        modified
                    );
                }
            }
            // Restore fixture write access so cleanup is reliable on all Unix hosts.
            fs::set_permissions(destination.join("child"), fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn copy_estimation_cancels_inside_a_wide_directory() {
        use std::cell::Cell;
        let root = tempfile::tempdir().unwrap();
        for index in 0..100 {
            fs::write(root.path().join(index.to_string()), b"four").unwrap();
        }
        for cancel_on_error in [false, true] {
            let token = CancellationToken::new();
            let visited = Cell::new(0);
            let totals = calculate_dir_totals_with(root.path(), &token, |path| {
                Ok(fs::read_dir(path)?.map(|entry| {
                    visited.set(visited.get() + 1);
                    if visited.get() == 5 {
                        token.cancel();
                        if cancel_on_error {
                            return Err(std::io::Error::other("injected enumeration failure"));
                        }
                    }
                    entry
                }))
            });
            assert_eq!(totals, (4, 16));
            assert_eq!(visited.get(), 5);
        }
        assert_eq!(
            calculate_dir_totals(root.path(), &CancellationToken::new()),
            (100, 400)
        );
        let token = CancellationToken::new();
        token.cancel();
        let totals = calculate_dir_totals_with::<fs::ReadDir>(root.path(), &token, |_| {
            panic!("cancelled estimation must not open a directory")
        });
        assert_eq!(totals, (0, 0));
    }

    #[test]
    fn copy_contents_bounds_input_and_honors_cancellation_between_read_and_write() {
        let token = CancellationToken::new();
        struct CancellingReader<'a>(&'a CancellationToken);
        impl std::io::Read for CancellingReader<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                bytes.fill(42);
                self.0.cancel();
                Ok(bytes.len())
            }
        }
        let mut output = Vec::new();
        let error = copy_contents(
            &mut CancellingReader(&token),
            &mut output,
            1_000_000,
            &token,
        )
        .unwrap_err();
        assert!(error.to_string().contains(CANCELLED_MESSAGE));
        assert!(output.is_empty());
        let token = CancellationToken::new();
        assert!(copy_contents(&mut &b"short"[..], &mut output, 10, &token).is_err());
        assert!(output.is_empty());
        assert!(
            copy_contents(&mut &b"grown"[..], &mut output, 4, &token)
                .unwrap_err()
                .to_string()
                .contains("grew")
        );
        assert_eq!(output, b"grow");
        output.clear();
        let data = vec![37; 700_001];
        copy_contents(&mut data.as_slice(), &mut output, data.len() as u64, &token).unwrap();
        assert_eq!(output, data);
    }

    #[test]
    fn file_copy_never_truncates_an_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let dest = root.path().join("dest");
        fs::write(&source, b"new contents").unwrap();
        fs::write(&dest, b"keep contents").unwrap();
        assert!(copy_file(&source, &dest, true, &CancellationToken::new()).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"keep contents");
        assert_eq!(fs::read(&source).unwrap(), b"new contents");
    }

    #[cfg(unix)]
    #[test]
    fn sparse_copy_preserves_contents_length_and_trailing_holes() {
        use std::io::{Seek, SeekFrom, Write};
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        for (index, length) in [0, 17, 64 * 1024 * 1024 + 17].into_iter().enumerate() {
            for populated in [false, true] {
                let source = root.path().join(format!("source-{index}-{populated}"));
                let dest = root.path().join(format!("dest-{index}-{populated}"));
                let mut file = fs::File::create(&source).unwrap();
                file.set_len(length).unwrap();
                if populated && length > 0 {
                    file.write_all(b"head").unwrap();
                    file.seek(SeekFrom::Start(length / 2)).unwrap();
                    file.write_all(b"middle").unwrap();
                }
                drop(file);
                assert_eq!(
                    copy_file(&source, &dest, true, &CancellationToken::new()).unwrap(),
                    length
                );
                assert_eq!(fs::metadata(&dest).unwrap().len(), length);
                assert_eq!(fs::read(&source).unwrap(), fs::read(&dest).unwrap());
                let source_bytes = fs::metadata(&source).unwrap().blocks() * 512;
                let dest_bytes = fs::metadata(&dest).unwrap().blocks() * 512;
                // Require compact output when this fixture's filesystem actually
                // supports holes. Small files can occupy an entire allocation unit.
                if length > 1024 * 1024 && source_bytes < length / 2 {
                    assert!(
                        dest_bytes < length / 2,
                        "source={source_bytes}, destination={dest_bytes}, logical={length}"
                    );
                }
                eprintln!(
                    "sparse copy: logical={length}, populated={populated}, source allocated={source_bytes}, destination allocated={dest_bytes}"
                );
            }
        }
    }

    #[test]
    fn copy_source_validation_detects_growth_and_same_length_timestamp_changes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"before").unwrap();
        filetime::set_file_mtime(&source, filetime::FileTime::from_unix_time(1000, 0)).unwrap();
        let before = fs::metadata(&source).unwrap();
        assert!(same_copy_source(&before, &fs::metadata(&source).unwrap()));
        fs::write(&source, b"after!").unwrap();
        filetime::set_file_mtime(&source, filetime::FileTime::from_unix_time(2000, 0)).unwrap();
        assert!(!same_copy_source(&before, &fs::metadata(&source).unwrap()));
        fs::write(&source, b"now longer").unwrap();
        filetime::set_file_mtime(&source, filetime::FileTime::from_unix_time(1000, 0)).unwrap();
        assert!(!same_copy_source(&before, &fs::metadata(&source).unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn file_copy_rejects_symlinks_and_fifos_without_creating_output() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("fifo");
        let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: native is a live, NUL-terminated pathname in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        let dest = root.path().join("dest");
        for source in [&fifo, &link] {
            assert!(
                copy_file(source, &dest, false, &CancellationToken::new())
                    .unwrap_err()
                    .contains("not a regular file")
            );
            assert!(!dest.exists());
        }
    }

    #[test]
    fn no_replace_rename_preserves_both_files_on_collision() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::write(&source, b"source").unwrap();
        fs::write(&destination, b"destination").unwrap();
        assert!(rename_noreplace(&source, &destination).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&destination).unwrap(), b"destination");
    }

    #[test]
    fn no_replace_rename_moves_to_an_unoccupied_path() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::write(&source, b"source").unwrap();
        rename_noreplace(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"source");
    }
}
