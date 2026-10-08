//! Filesystem primitives shared by every operation.
//!
//! Everything here is synchronous and must run on a blocking thread.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Message used when cooperative cancellation aborts blocking work.
pub(crate) const CANCELLED: &str = "operation cancelled";

pub(crate) fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, CANCELLED)
}

/// Why a no-replace rename did not succeed.
///
/// Callers must distinguish these: treating "destination exists" as a generic
/// failure and falling back to a copy would silently destroy the existing item.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RenameError {
    #[error("destination already exists")]
    DestinationExists,
    #[error("source and destination are on different filesystems")]
    CrossDevice,
    #[error("{0}")]
    Io(io::Error),
}

impl RenameError {
    fn classify(error: io::Error) -> Self {
        if is_cross_device(&error) {
            Self::CrossDevice
        } else if error.kind() == io::ErrorKind::AlreadyExists {
            Self::DestinationExists
        } else {
            Self::Io(error)
        }
    }
}

impl From<RenameError> for io::Error {
    fn from(error: RenameError) -> Self {
        match error {
            RenameError::DestinationExists => {
                io::Error::new(io::ErrorKind::AlreadyExists, "destination already exists")
            }
            RenameError::CrossDevice => io::Error::new(
                io::ErrorKind::CrossesDevices,
                "source and destination are on different filesystems",
            ),
            RenameError::Io(error) => error,
        }
    }
}

#[cfg(unix)]
fn is_cross_device(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EXDEV)
}

#[cfg(windows)]
fn is_cross_device(error: &io::Error) -> bool {
    const ERROR_NOT_SAME_DEVICE: i32 = 17;
    error.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE)
}

#[cfg(not(any(unix, windows)))]
fn is_cross_device(_error: &io::Error) -> bool {
    false
}

/// Rename `source` to `destination`, never replacing an existing destination.
///
/// Uses `renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on
/// Apple platforms and `MoveFileExW` without `REPLACE_EXISTING` on Windows.
/// Filesystems that reject the flag (NFS, SMB, exFAT, many FUSE mounts) fall
/// back to primitives that are still exclusive: `link` + `unlink` for files and
/// symlinks, `mkdir` + `rename` over the freshly created empty directory for
/// directories.
pub(crate) fn rename_noreplace(source: &Path, destination: &Path) -> Result<(), RenameError> {
    match platform::rename_noreplace(source, destination) {
        Some(result) => result.map_err(RenameError::classify),
        None => rename_noreplace_fallback(source, destination),
    }
}

fn rename_noreplace_fallback(source: &Path, destination: &Path) -> Result<(), RenameError> {
    let metadata = fs::symlink_metadata(source).map_err(RenameError::Io)?;
    if metadata.is_dir() {
        // `mkdir` is exclusive; POSIX `rename` atomically replaces the empty
        // directory it created, so no foreign content can be clobbered.
        fs::create_dir(destination).map_err(RenameError::classify)?;
        if let Err(error) = fs::rename(source, destination) {
            let _ = fs::remove_dir(destination);
            return Err(RenameError::classify(error));
        }
        return Ok(());
    }
    match fs::hard_link(source, destination) {
        Ok(()) => fs::remove_file(source).map_err(|error| {
            // Both names now refer to the data; report the stray source.
            RenameError::Io(io::Error::new(
                error.kind(),
                format!("renamed by linking, but the original name remains: {error}"),
            ))
        }),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(RenameError::DestinationExists)
        }
        Err(error) if is_cross_device(&error) => Err(RenameError::CrossDevice),
        // Filesystems without hard links (FAT, exFAT): a check-then-rename
        // is the best the platform offers.
        Err(_) => {
            if fs::symlink_metadata(destination).is_ok() {
                return Err(RenameError::DestinationExists);
            }
            fs::rename(source, destination).map_err(RenameError::classify)
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
mod platform {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// `None` when the filesystem does not support an atomic no-replace rename.
    pub(super) fn rename_noreplace(source: &Path, destination: &Path) -> Option<io::Result<()>> {
        let (Ok(source), Ok(destination)) = (
            CString::new(source.as_os_str().as_bytes()),
            CString::new(destination.as_os_str().as_bytes()),
        ) else {
            return Some(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains a NUL byte",
            )));
        };
        // SAFETY: both C strings are NUL-terminated and outlive the call.
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
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code)
                if code == libc::ENOSYS
                    || code == libc::EINVAL
                    || code == libc::ENOTSUP
                    || code == libc::EOPNOTSUPP =>
            {
                None
            }
            _ => Some(Err(error)),
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    pub(super) fn rename_noreplace(source: &Path, destination: &Path) -> Option<io::Result<()>> {
        let wide =
            |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain(Some(0)).collect() };
        let (source, destination) = (wide(source), wide(destination));
        // SAFETY: NUL-terminated UTF-16 buffers that outlive the call. Zero
        // flags exclude REPLACE_EXISTING and COPY_ALLOWED.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                0,
            )
        };
        Some(if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        })
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    windows
)))]
mod platform {
    pub(super) fn rename_noreplace(
        _source: &std::path::Path,
        _destination: &std::path::Path,
    ) -> Option<std::io::Result<()>> {
        None
    }
}

/// Remove a file, symlink or directory tree without following symlinks.
pub(crate) fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => remove_link_or_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_link_or_file(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if fs::symlink_metadata(path)?.file_type().is_symlink_dir() {
            return fs::remove_dir(path);
        }
    }
    fs::remove_file(path)
}

/// Recreate the symlink `source` (pointing at `target`) at `destination`.
pub(crate) fn symlink(target: &Path, source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = source;
        std::os::unix::fs::symlink(target, destination)
    }
    #[cfg(windows)]
    {
        if fs::metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
            std::os::windows::fs::symlink_dir(target, destination)
        } else {
            std::os::windows::fs::symlink_file(target, destination)
        }
    }
}

/// Reserve an owner-only directory beside `sibling`, on the same filesystem,
/// so its contents can later be published with a single rename.
pub(crate) fn staging_dir(sibling: &Path, prefix: &str) -> io::Result<tempfile::TempDir> {
    let parent = parent_or_current(sibling);
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

/// Prefix of every staging directory this crate creates.
pub const STAGING_PREFIX: &str = ".gravityfile-";

pub(crate) fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Canonicalize the longest existing prefix of `path` and append the rest
/// lexically, so not-yet-created destinations can be compared with sources.
pub(crate) fn canonicalize_lenient(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = path.canonicalize() {
        return Some(path);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut suffix: Vec<OsString> = Vec::new();
    while let Some(name) = existing.file_name() {
        suffix.push(name.to_owned());
        existing = existing.parent()?;
        if let Ok(mut canonical) = existing.canonicalize() {
            canonical.extend(suffix.iter().rev());
            return Some(canonical);
        }
    }
    None
}

/// Whether `destination` equals or lies beneath the directory `source`,
/// resolving symlinked ancestors on both sides.
pub(crate) fn is_within_directory(source: &Path, destination: &Path) -> bool {
    if !fs::symlink_metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
        return false;
    }
    match (
        canonicalize_lenient(source),
        canonicalize_lenient(destination),
    ) {
        (Some(source), Some(destination)) => destination.starts_with(source),
        _ => false,
    }
}

/// Whether two paths name the same filesystem object.
pub(crate) fn same_object(a: &Path, b: &Path) -> bool {
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => same_identity(&a, &b),
        _ => false,
    }
}

#[cfg(unix)]
pub(crate) fn same_identity(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
pub(crate) fn same_identity(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len()
        && a.modified().ok() == b.modified().ok()
        && a.created().ok() == b.created().ok()
        && a.file_type() == b.file_type()
}

/// True when a regular file still matches an earlier observation.
pub(crate) fn unchanged_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    if !after.is_file()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.ino() != after.ino()
            || before.dev() != after.dev()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return false;
        }
    }
    true
}

/// Resolve `path` component by component, following symlinks the way the
/// kernel would, and report whether the result stays inside `root`.
///
/// Unlike a lexical normalization this handles chained links such as
/// `a -> ..` followed by `b -> a/../..`. Missing trailing components are
/// resolved lexically. `root` must be canonical.
pub(crate) fn resolves_within(root: &Path, path: &Path) -> io::Result<bool> {
    const MAX_LINKS: usize = 40;
    let mut resolved = PathBuf::new();
    let mut pending: Vec<OsString> = Vec::new();
    push_components(&mut pending, path, &mut resolved);
    let mut links = 0;
    while let Some(component) = pending.pop() {
        if component == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&component);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                links += 1;
                if links > MAX_LINKS {
                    return Err(io::Error::other("too many levels of symbolic links"));
                }
                let target = fs::read_link(&candidate)?;
                push_components(&mut pending, &target, &mut resolved);
            }
            Ok(_) => resolved = candidate,
            Err(error) if error.kind() == io::ErrorKind::NotFound => resolved = candidate,
            Err(error) => return Err(error),
        }
    }
    Ok(resolved.starts_with(root))
}

/// Queue `path`'s components (reversed, for popping). Absolute paths reset
/// the resolution to their root.
fn push_components(pending: &mut Vec<OsString>, path: &Path, resolved: &mut PathBuf) {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                *resolved = PathBuf::from(component.as_os_str());
                parts.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => parts.push(OsString::from("..")),
            Component::Normal(name) => parts.push(name.to_owned()),
        }
    }
    pending.extend(parts.into_iter().rev());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_noreplace_refuses_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        assert!(matches!(
            rename_noreplace(&a, &b),
            Err(RenameError::DestinationExists)
        ));
        assert_eq!(fs::read_to_string(&b).unwrap(), "b");
        fs::remove_file(&b).unwrap();
        rename_noreplace(&a, &b).unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "a");
    }

    #[test]
    fn fallback_is_exclusive_for_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        let taken = dir.path().join("taken");
        fs::write(&file, "data").unwrap();
        fs::write(&taken, "keep").unwrap();
        assert!(matches!(
            rename_noreplace_fallback(&file, &taken),
            Err(RenameError::DestinationExists)
        ));
        rename_noreplace_fallback(&file, &dir.path().join("moved")).unwrap();
        assert!(!file.exists());

        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("inner")).unwrap();
        let occupied = dir.path().join("occupied");
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("keep"), "x").unwrap();
        assert!(rename_noreplace_fallback(&tree, &occupied).is_err());
        assert!(occupied.join("keep").exists());
        rename_noreplace_fallback(&tree, &dir.path().join("tree2")).unwrap();
        assert!(dir.path().join("tree2/inner").is_dir());
    }

    #[test]
    fn lenient_canonicalization_appends_missing_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(
            canonicalize_lenient(&dir.path().join("missing/child")).unwrap(),
            root.join("missing/child")
        );
        assert!(is_within_directory(dir.path(), &dir.path().join("x/y")));
        assert!(!is_within_directory(&dir.path().join("x"), dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn chained_symlinks_are_resolved_like_the_kernel() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("root/d")).unwrap();
        let root = dir.path().join("root").canonicalize().unwrap();
        std::os::unix::fs::symlink("..", root.join("d/l1")).unwrap();
        // Lexically `d/l1/../..` is `root`, but `l1` resolves to `root`, so
        // the kernel lands on root's parent.
        std::os::unix::fs::symlink("l1/../..", root.join("d/esc")).unwrap();
        assert!(resolves_within(&root, &root.join("d/l1")).unwrap());
        assert!(!resolves_within(&root, &root.join("d/esc")).unwrap());
        assert!(resolves_within(&root, &root.join("d/missing/file")).unwrap());
    }
}
