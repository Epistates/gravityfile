//! Exact, staged directory handoff for shell wrappers.
use std::{
    borrow::Cow,
    io::{self, Write},
    path::Path,
};

fn path_bytes(path: &Path) -> io::Result<Cow<'_, [u8]>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(Cow::Borrowed(path.as_os_str().as_bytes()))
    }
    #[cfg(not(unix))]
    {
        path.to_str()
            .map(|text| Cow::Borrowed(text.as_bytes()))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "shell directory handoff requires a Unicode path on this platform",
                )
            })
    }
}

pub(crate) fn write_cwd_file(destination: &Path, cwd: &Path) -> io::Result<()> {
    let bytes = path_bytes(cwd)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staged = tempfile::Builder::new()
        .prefix(".gravityfile-cwd-")
        .tempfile_in(parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(destination).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn print_cwd(cwd: &Path, output: &mut impl Write) -> io::Result<()> {
    output.write_all(&path_bytes(cwd)?)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_replacement_preserves_destination_and_removes_staging() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("occupied");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("keep"), b"original").unwrap();
        assert!(write_cwd_file(&destination, Path::new("/new")).is_err());
        assert_eq!(
            std::fs::read(destination.join("keep")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn handoff_preserves_non_unicode_and_trailing_newlines_without_following_symlinks() {
        use std::os::unix::{
            ffi::OsStrExt,
            fs::{PermissionsExt, symlink},
        };
        let temporary = tempfile::tempdir().unwrap();
        let victim = temporary.path().join("victim");
        std::fs::write(&victim, b"untouched").unwrap();
        let destination = temporary.path().join("cwd");
        symlink(&victim, &destination).unwrap();
        let cwd = Path::new(std::ffi::OsStr::from_bytes(b"/path/\xff\n\n"));
        write_cwd_file(&destination, cwd).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"/path/\xff\n\n");
        assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
        assert_eq!(
            std::fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut printed = Vec::new();
        print_cwd(cwd, &mut printed).unwrap();
        assert_eq!(printed, b"/path/\xff\n\n\n");
    }

    #[test]
    fn stdout_failure_is_returned_instead_of_panicking() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            print_cwd(Path::new("/cwd"), &mut Broken)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}
