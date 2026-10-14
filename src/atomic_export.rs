//! Private export staging. Unix operations remain relative to one open parent.
//! This does not protect against writers who can replace entries inside that
//! directory, nor constrain ancestor resolution when the parent is first opened.

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
pub(crate) struct StagedExport {
    file: File,
    parent: File,
    staging_name: Option<std::ffi::OsString>,
    output_name: std::ffi::OsString,
}

#[cfg(unix)]
impl StagedExport {
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        use rustix::fs::{Mode, OFlags, open, openat};
        use std::os::unix::ffi::OsStrExt;

        // Path::file_name normalizes trailing separators and `.`; do not turn
        // an invalid directory destination into replacement of a regular file.
        let tail = path.as_os_str().as_bytes().rsplit(|b| *b == b'/').next();
        if matches!(tail, None | Some(b"" | b"." | b"..")) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "export destination must name a file",
            ));
        }
        let output_name = path
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "export destination must name a file",
                )
            })?
            .to_owned();
        let parent_path = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = File::from(open(
            parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        for _ in 0..128 {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
            let name = format!(".gravityfile-export-{:032x}", u128::from_ne_bytes(nonce));
            match openat(
                &parent,
                name.as_str(),
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(fd) => {
                    return Ok(Self {
                        file: File::from(fd),
                        parent,
                        staging_name: Some(name.into()),
                        output_name,
                    });
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique export staging file",
        ))
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub(crate) fn persist(&mut self) -> io::Result<()> {
        let name = self
            .staging_name
            .as_ref()
            .ok_or_else(|| io::Error::other("export already published"))?;
        rustix::fs::renameat(&self.parent, name, &self.parent, &self.output_name)?;
        self.staging_name = None;
        Ok(())
    }

    pub(crate) fn sync_parent(&self) -> io::Result<()> {
        self.parent.sync_all()
    }
}

#[cfg(unix)]
impl Drop for StagedExport {
    fn drop(&mut self) {
        if let Some(name) = &self.staging_name {
            let _ = rustix::fs::unlinkat(&self.parent, name, rustix::fs::AtFlags::empty());
        }
    }
}

#[cfg(not(unix))]
pub(crate) struct StagedExport {
    file: File,
    staging: Option<tempfile::TempPath>,
    output: std::path::PathBuf,
}

#[cfg(not(unix))]
impl StagedExport {
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let (file, staging) = tempfile::NamedTempFile::new_in(parent)?.into_parts();
        Ok(Self {
            file,
            staging: Some(staging),
            output: path.to_owned(),
        })
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub(crate) fn persist(&mut self) -> io::Result<()> {
        let staging = self
            .staging
            .take()
            .ok_or_else(|| io::Error::other("export already published"))?;
        staging.persist(&self.output).map_err(|e| e.error)
    }

    pub(crate) fn sync_parent(&self) -> io::Result<()> {
        Ok(())
    }
}
