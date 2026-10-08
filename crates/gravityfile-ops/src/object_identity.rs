//! Native object identity, without following the final symlink/reparse point.
//! A path check is evidence, not an atomic conditional rename or content snapshot.
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ObjectKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Native identity recorded before mutation. Does not identify a content version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum ObjectIdentity {
    /// Device/inode identity on Unix; inode reuse is not excluded.
    Unix {
        device: u64,
        inode: u64,
        kind: ObjectKind,
    },
    /// Windows volume serial and 128-bit file ID obtained from a no-follow handle.
    Windows {
        volume: u64,
        file_id: [u8; 16],
        kind: ObjectKind,
    },
}

impl ObjectIdentity {
    /// Read native object identity without following the final link/reparse point.
    /// Unsupported filesystems fail rather than providing a fabricated identity.
    pub fn capture(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(path)?;
            let kind = if metadata.is_symlink() {
                ObjectKind::Symlink
            } else if metadata.is_dir() {
                ObjectKind::Directory
            } else if metadata.is_file() {
                ObjectKind::File
            } else {
                ObjectKind::Other
            };
            Ok(Self::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
                kind,
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
            use windows_sys::Win32::Storage::FileSystem::*;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .access_mode(0)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            let mut id: FILE_ID_INFO = unsafe { std::mem::zeroed() };
            let mut attributes: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
            // SAFETY: both buffers are initialized, have the documented size,
            // and the borrowed handle remains alive throughout these calls.
            let result = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle().cast(),
                    FileIdInfo,
                    (&mut id as *mut FILE_ID_INFO).cast(),
                    std::mem::size_of::<FILE_ID_INFO>() as u32,
                )
            };
            if result == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle().cast(),
                    FileAttributeTagInfo,
                    (&mut attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                    std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
                )
            };
            if result == 0 {
                return Err(io::Error::last_os_error());
            }
            let kind = if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                ObjectKind::Symlink
            } else if attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                ObjectKind::Directory
            } else {
                ObjectKind::File
            };
            Ok(Self::Windows {
                volume: id.VolumeSerialNumber,
                file_id: id.FileId.Identifier,
                kind,
            })
        }
    }

    /// Compare a path with this observation. The check is not atomic with later I/O.
    pub fn verify(&self, path: &Path) -> io::Result<()> {
        if *self != Self::capture(path)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "filesystem object identity changed; refusing to move replacement data",
            ));
        }
        Ok(())
    }
}
