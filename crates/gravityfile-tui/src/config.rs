//! Reading the files in the config directory.

use std::io::{Error, ErrorKind, Read};
use std::path::Path;

/// The largest config file read or written.
pub(crate) const MAX_BYTES: u64 = 1024 * 1024;

/// Read a config file as text. A missing file is `Ok(None)`. Symlinks are
/// followed, as dotfile managers use them, but anything that is not a
/// regular UTF-8 file of at most 1 MiB is an error, and a FIFO is rejected
/// without waiting for a writer.
pub(crate) fn read(path: &Path) -> std::io::Result<Option<String>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "config files must be regular UTF-8 files of at most 1 MiB",
        ));
    }
    let mut content = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut content)?;
    if content.len() as u64 > MAX_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "config file grew beyond 1 MiB while reading",
        ));
    }
    Ok(Some(content))
}
