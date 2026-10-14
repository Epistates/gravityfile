//! Blocking copy engine shared by copy and cross-filesystem move.
//!
//! A transfer copies one top-level item into a private staging directory on
//! the destination filesystem and publishes it with a single no-clobber
//! rename, so a cancelled or failed transfer never leaves a partial item at
//! the user-visible path. Problems with individual entries inside a directory
//! (unreadable files, sockets, FIFOs) are recorded and skipped rather than
//! discarding everything that did copy.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio_util::sync::CancellationToken;

use crate::OperationError;
use crate::fs::{RenameError, cancelled, rename_noreplace, staging_dir, unchanged_file};

const CHUNK: usize = 1024 * 1024;
const MAX_RETAINED_ERRORS: usize = 1000;

/// How an existing destination is disposed of when the user chooses to
/// overwrite it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplacePolicy {
    /// Move the existing item to the system Trash first (recoverable).
    #[default]
    Trash,
    /// Delete the existing item permanently.
    Permanent,
}

/// Metadata preservation settings for a transfer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Preserve {
    pub times: bool,
    pub xattrs: bool,
}

/// One transfer's state: cancellation, live byte counter, and the non-fatal
/// per-entry errors collected along the way.
pub(crate) struct Transfer<'a> {
    token: &'a CancellationToken,
    bytes: &'a AtomicU64,
    preserve: Preserve,
    errors: Vec<OperationError>,
    omitted_errors: usize,
    /// First destination path for each multiply-linked source inode.
    links: HashMap<(u64, u64), PathBuf>,
}

impl<'a> Transfer<'a> {
    pub fn new(token: &'a CancellationToken, bytes: &'a AtomicU64, preserve: Preserve) -> Self {
        Self {
            token,
            bytes,
            preserve,
            errors: Vec::new(),
            omitted_errors: 0,
            links: HashMap::new(),
        }
    }

    /// Non-fatal errors, with an omission record when the list was capped.
    pub fn into_errors(self) -> Vec<OperationError> {
        let mut errors = self.errors;
        if self.omitted_errors > 0 {
            errors.push(OperationError::omitted(self.omitted_errors));
        }
        errors
    }

    fn record(&mut self, path: &Path, message: impl Into<String>) {
        if self.errors.len() < MAX_RETAINED_ERRORS {
            self.errors
                .push(OperationError::new(path.to_path_buf(), message));
        } else {
            self.omitted_errors += 1;
        }
    }

    fn check(&self) -> io::Result<()> {
        if self.token.is_cancelled() {
            Err(cancelled())
        } else {
            Ok(())
        }
    }

    /// Copy `source` to the not-yet-existing `destination` through staging.
    ///
    /// With `replace`, an existing destination is disposed of (per the
    /// policy) only after the new copy is complete.
    pub fn copy_staged(
        &mut self,
        source: &Path,
        destination: &Path,
        replace: Option<ReplacePolicy>,
    ) -> io::Result<()> {
        let staging = staging_dir(destination, ".gravityfile-copy-")?;
        let payload = staging.path().join("payload");
        self.copy_entry(source, &payload)?;
        self.check()?;
        if let Some(policy) = replace
            && fs::symlink_metadata(destination).is_ok()
        {
            dispose(destination, policy)?;
        }
        rename_noreplace(&payload, destination).map_err(|error| match error {
            RenameError::DestinationExists => io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("'{}' appeared while copying", destination.display()),
            ),
            other => other.into(),
        })?;
        if let Err(error) = staging.close() {
            tracing::warn!(%error, "copy published; staging cleanup incomplete");
        }
        Ok(())
    }

    /// Copy one entry of any type. Errors returned here are fatal for the
    /// whole top-level item; problems with nested entries are recorded.
    fn copy_entry(&mut self, source: &Path, destination: &Path) -> io::Result<()> {
        self.check()?;
        let metadata = fs::symlink_metadata(source)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            self.copy_symlink(source, destination)
        } else if file_type.is_dir() {
            self.copy_tree(source, destination)
        } else if file_type.is_file() {
            self.copy_file(source, destination, &metadata)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "sockets, FIFOs and device files cannot be copied",
            ))
        }
    }

    fn copy_symlink(&mut self, source: &Path, destination: &Path) -> io::Result<()> {
        let target = fs::read_link(source)?;
        crate::fs::symlink(&target, source, destination)?;
        if self.preserve.times {
            let metadata = fs::symlink_metadata(source)?;
            filetime::set_symlink_file_times(
                destination,
                filetime::FileTime::from_last_access_time(&metadata),
                filetime::FileTime::from_last_modification_time(&metadata),
            )?;
        }
        Ok(())
    }

    fn copy_tree(&mut self, source: &Path, destination: &Path) -> io::Result<()> {
        let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
        let mut finished: Vec<(PathBuf, PathBuf, fs::Metadata)> = Vec::new();
        let mut visited: HashSet<(u64, u64)> = HashSet::new();
        let mut is_root = true;

        while let Some((source_dir, dest_dir)) = pending.pop() {
            self.check()?;
            let metadata = match fs::symlink_metadata(&source_dir) {
                Ok(metadata) if metadata.is_dir() => metadata,
                Ok(_) if !is_root => {
                    self.record(&source_dir, "changed type while copying; skipped");
                    continue;
                }
                Ok(_) => return Err(io::Error::other("source directory changed type")),
                Err(error) if !is_root => {
                    self.record(&source_dir, error.to_string());
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some(identity) = identity(&metadata)
                && !visited.insert(identity)
            {
                self.record(&source_dir, "directory reached twice (mount loop); skipped");
                continue;
            }
            // The root must be created; nested directories that fail are
            // recorded and skipped.
            match fs::create_dir(&dest_dir) {
                Ok(()) => {}
                Err(error) if is_root => return Err(error),
                Err(error) => {
                    self.record(&dest_dir, format!("cannot create directory: {error}"));
                    continue;
                }
            }
            is_root = false;
            finished.push((source_dir.clone(), dest_dir.clone(), metadata));

            let entries = match fs::read_dir(&source_dir) {
                Ok(entries) => entries,
                Err(error) => {
                    self.record(&source_dir, format!("cannot read directory: {error}"));
                    continue;
                }
            };
            for entry in entries {
                self.check()?;
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        self.record(&source_dir, format!("cannot read entry: {error}"));
                        continue;
                    }
                };
                let path = entry.path();
                let target = dest_dir.join(entry.file_name());
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(error) => {
                        self.record(&path, error.to_string());
                        continue;
                    }
                };
                if file_type.is_dir() {
                    pending.push((path, target));
                    continue;
                }
                let result = if file_type.is_symlink() {
                    self.copy_symlink(&path, &target)
                } else if file_type.is_file() {
                    fs::symlink_metadata(&path)
                        .and_then(|metadata| self.copy_file(&path, &target, &metadata))
                } else {
                    self.record(&path, "special file (socket, FIFO or device) skipped");
                    continue;
                };
                match result {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                    Err(error) => {
                        let _ = fs::remove_file(&target);
                        self.record(&path, error.to_string());
                    }
                }
            }
        }

        // Apply modes and times bottom-up, after contents exist: a read-only
        // directory must not prevent its own population.
        for (source_dir, directory, metadata) in finished.into_iter().rev() {
            self.check()?;
            if self.preserve.xattrs {
                copy_xattrs(&source_dir, &directory);
            }
            if self.preserve.times {
                let _ = filetime::set_file_times(
                    &directory,
                    filetime::FileTime::from_last_access_time(&metadata),
                    filetime::FileTime::from_last_modification_time(&metadata),
                );
            }
            if let Err(error) = fs::set_permissions(&directory, metadata.permissions()) {
                self.record(&directory, format!("cannot restore permissions: {error}"));
            }
        }
        Ok(())
    }

    fn copy_file(
        &mut self,
        source: &Path,
        destination: &Path,
        metadata: &fs::Metadata,
    ) -> io::Result<()> {
        if let Some(key) = identity(metadata).filter(|_| link_count(metadata) > 1) {
            if let Some(first) = self.links.get(&key) {
                if fs::hard_link(first, destination).is_ok() {
                    return Ok(());
                }
            } else {
                self.links.insert(key, destination.to_path_buf());
            }
        }

        // Copy-on-write clone: instant on APFS, Btrfs, XFS, ReFS. The
        // destination lives in private staging, so cleanup after a failed
        // attempt cannot touch anything else.
        if metadata.len() > 0 && reflink_copy::reflink(source, destination).is_ok() {
            self.bytes.fetch_add(metadata.len(), Ordering::Relaxed);
            self.finish_file(source, destination, metadata, cfg!(target_vendor = "apple"))?;
            return Ok(());
        }
        let _ = fs::remove_file(destination);

        let mut input = open_regular(source)?;
        let opened = input.metadata()?;
        if !unchanged_file(metadata, &opened) {
            return Err(io::Error::other("file changed while opening"));
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        self.copy_contents(&mut input, &mut output, opened.len())?;
        if !unchanged_file(&opened, &input.metadata()?) {
            return Err(io::Error::other("file changed while copying"));
        }
        output.set_permissions(opened.permissions())?;
        drop(output);
        self.finish_file(source, destination, &opened, false)
    }

    fn finish_file(
        &self,
        source: &Path,
        destination: &Path,
        metadata: &fs::Metadata,
        metadata_cloned: bool,
    ) -> io::Result<()> {
        if self.preserve.xattrs && !metadata_cloned {
            copy_xattrs(source, destination);
        }
        if self.preserve.times {
            filetime::set_file_times(
                destination,
                filetime::FileTime::from_last_access_time(metadata),
                filetime::FileTime::from_last_modification_time(metadata),
            )?;
        }
        Ok(())
    }

    /// Copy exactly `length` bytes in cancellable chunks.
    fn copy_contents(&self, input: &mut File, output: &mut File, length: u64) -> io::Result<()> {
        let mut remaining = length;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // `io::copy` between files uses copy_file_range/sendfile.
            while remaining > 0 {
                self.check()?;
                let chunk = remaining.min(CHUNK as u64);
                let copied = io::copy(&mut Read::by_ref(input).take(chunk), output)?;
                if copied == 0 {
                    return Err(io::Error::other("file shrank while copying"));
                }
                remaining -= copied;
                self.bytes.fetch_add(copied, Ordering::Relaxed);
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            use std::io::{Seek, Write};
            let mut buffer = vec![0u8; (length.clamp(1, CHUNK as u64)) as usize];
            let mut sparse = cfg!(unix);
            while remaining > 0 {
                self.check()?;
                let amount = remaining.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..amount])?;
                if sparse && buffer[..amount].iter().all(|byte| *byte == 0) {
                    if output.seek(io::SeekFrom::Current(amount as i64)).is_err() {
                        sparse = false;
                        output.write_all(&buffer[..amount])?;
                    }
                } else {
                    output.write_all(&buffer[..amount])?;
                }
                remaining -= amount as u64;
                self.bytes.fetch_add(amount as u64, Ordering::Relaxed);
            }
            // A trailing hole is only materialized by extending the length.
            output.set_len(length)?;
        }
        let mut probe = [0u8; 1];
        if input.read(&mut probe)? != 0 {
            return Err(io::Error::other("file grew while copying"));
        }
        Ok(())
    }
}

/// Dispose of an existing destination before replacing it.
pub(crate) fn dispose(path: &Path, policy: ReplacePolicy) -> io::Result<()> {
    match policy {
        ReplacePolicy::Trash => crate::delete::trash_context()
            .delete(path)
            .map_err(|error| {
                io::Error::other(format!(
                    "could not move the existing item to the Trash ({error}); it was left in place"
                ))
            }),
        ReplacePolicy::Permanent => crate::fs::remove_path(path),
    }
}

fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a symlink swapped in after inspection, and never block
        // on a FIFO swapped in for a regular file.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn identity(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[cfg(unix)]
fn link_count(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(not(unix))]
fn link_count(_metadata: &fs::Metadata) -> u64 {
    1
}

/// Best-effort extended attribute copy (Finder tags, quarantine, SELinux
/// labels...). Filesystems without xattr support are silently skipped.
#[cfg(unix)]
fn copy_xattrs(source: &Path, destination: &Path) {
    let Ok(names) = xattr::list(source) else {
        return;
    };
    for name in names {
        if let Ok(Some(value)) = xattr::get(source, &name)
            && let Err(error) = xattr::set(destination, &name, &value)
        {
            tracing::debug!(path = %destination.display(), ?name, %error, "xattr not copied");
        }
    }
}

#[cfg(not(unix))]
fn copy_xattrs(_source: &Path, _destination: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &Path, destination: &Path) -> (io::Result<()>, Vec<OperationError>, u64) {
        let token = CancellationToken::new();
        let bytes = AtomicU64::new(0);
        let mut transfer = Transfer::new(
            &token,
            &bytes,
            Preserve {
                times: true,
                xattrs: true,
            },
        );
        let result = transfer.copy_staged(source, destination, None);
        (result, transfer.into_errors(), bytes.into_inner())
    }

    #[test]
    fn copies_tree_with_contents_and_counts_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        fs::create_dir_all(source.join("a/b")).unwrap();
        fs::write(source.join("a/b/file"), vec![7u8; 3 * CHUNK + 5]).unwrap();
        fs::write(source.join("top"), "x").unwrap();
        let destination = dir.path().join("dst");
        let (result, errors, bytes) = run(&source, &destination);
        result.unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(bytes, 3 * CHUNK as u64 + 6);
        assert_eq!(
            fs::read(destination.join("a/b/file")).unwrap().len(),
            3 * CHUNK + 5
        );
        assert_eq!(fs::read_to_string(destination.join("top")).unwrap(), "x");
        assert!(!dir.path().read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".gravityfile-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_and_special_entries_are_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("ok"), "fine").unwrap();
        let secret = source.join("secret");
        fs::write(&secret, "no").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
        let fifo =
            std::ffi::CString::new(source.join("fifo").into_os_string().into_encoded_bytes())
                .unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let destination = dir.path().join("dst");
        let (result, errors, _) = run(&source, &destination);
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        result.unwrap();
        assert_eq!(fs::read_to_string(destination.join("ok")).unwrap(), "fine");
        assert!(fs::symlink_metadata(destination.join("fifo")).is_err());
        assert!(errors.iter().any(|error| error.path.ends_with("fifo")));
        // Root may read anything; only assert the skip when it actually failed.
        if !destination.join("secret").exists() {
            assert!(errors.iter().any(|error| error.path.ends_with("secret")));
        }
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_and_read_only_directories_are_preserved() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        let locked = source.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(locked.join("a"), "shared").unwrap();
        fs::hard_link(locked.join("a"), locked.join("b")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let destination = dir.path().join("dst");
        let (result, errors, _) = run(&source, &destination);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        result.unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        let copied = destination.join("locked");
        assert_eq!(
            fs::metadata(&copied).unwrap().permissions().mode() & 0o777,
            0o555
        );
        assert_eq!(
            fs::metadata(copied.join("a")).unwrap().ino(),
            fs::metadata(copied.join("b")).unwrap().ino()
        );
        fs::set_permissions(&copied, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn cancellation_leaves_nothing_at_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), "x").unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let bytes = AtomicU64::new(0);
        let mut transfer = Transfer::new(
            &token,
            &bytes,
            Preserve {
                times: false,
                xattrs: false,
            },
        );
        let destination = dir.path().join("dst");
        let error = transfer
            .copy_staged(&source, &destination, None)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!destination.exists());
        assert_eq!(dir.path().read_dir().unwrap().count(), 1);
    }

    #[test]
    fn replace_disposes_existing_item_only_after_copy_completes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("new");
        let destination = dir.path().join("old");
        fs::write(&source, "new").unwrap();
        fs::write(&destination, "old").unwrap();
        let token = CancellationToken::new();
        let bytes = AtomicU64::new(0);
        let mut transfer = Transfer::new(
            &token,
            &bytes,
            Preserve {
                times: true,
                xattrs: false,
            },
        );
        transfer
            .copy_staged(&source, &destination, Some(ReplacePolicy::Permanent))
            .unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "new");
    }
}
