//! Private, bounded storage for the line-oriented bulk rename editor handoff.
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 1024 * 1024;

pub(super) struct BulkRenameEdit {
    _directory: tempfile::TempDir,
    path: PathBuf,
    pub paths: Vec<PathBuf>,
}

impl BulkRenameEdit {
    pub fn new(mut paths: Vec<PathBuf>) -> io::Result<Self> {
        paths.sort();
        let mut content = String::new();
        for path in &paths {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Bulk rename editor requires UTF-8 filenames",
                    )
                })?;
            if gravityfile_core::escape_terminal_text(name) != name {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Bulk rename editor cannot represent filenames containing controls or directional formatting",
                ));
            }
            if content.len() as u64 + name.len() as u64 + 1 > MAX_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Rename list exceeds 1 MiB",
                ));
            }
            content.push_str(name);
            content.push('\n');
        }
        let mut builder = tempfile::Builder::new();
        builder.prefix("gravityfile-rename-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir()?;
        let path = directory.path().join("names.txt");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&path)?.write_all(content.as_bytes())?;
        Ok(Self {
            _directory: directory,
            path,
            paths,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_names(&self) -> io::Result<Vec<String>> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&self.path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Edited rename list must be a regular UTF-8 file of at most 1 MiB",
            ));
        }
        let mut content = String::new();
        file.take(MAX_BYTES + 1).read_to_string(&mut content)?;
        if content.len() as u64 > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Edited rename list exceeds 1 MiB",
            ));
        }
        let names: Vec<_> = content.lines().map(str::to_owned).collect();
        if names.len() != self.paths.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Line count mismatch: expected {}, got {}",
                    self.paths.len(),
                    names.len()
                ),
            ));
        }
        for name in &names {
            if gravityfile_core::escape_terminal_text(name) != name.as_str() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Edited filenames cannot contain controls or directional formatting",
                ));
            }
            gravityfile_ops::validate_filename(name)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_rejects_directional_formatting_in_original_and_edited_names() {
        for name in ["safe\u{202e}txt", "safe\u{2067}txt", "safe\u{2028}txt"] {
            assert!(BulkRenameEdit::new(vec![name.into()]).is_err());
            let edit = BulkRenameEdit::new(vec!["safe.txt".into()]).unwrap();
            std::fs::write(edit.path(), format!("{name}\n")).unwrap();
            assert!(edit.read_names().is_err());
            assert_eq!(edit.paths, vec![PathBuf::from("safe.txt")]);
        }
    }

    #[test]
    fn edit_is_sorted_private_and_cleaned_on_drop() {
        let edit = BulkRenameEdit::new(vec!["z".into(), "a".into()]).unwrap();
        let path = edit.path().to_owned();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nz\n");
        std::fs::write(&path, "renamed-a\nrenamed-z\n").unwrap();
        assert_eq!(edit.read_names().unwrap(), ["renamed-a", "renamed-z"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        drop(edit);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn edit_rejects_malformed_and_oversized_results() {
        let edit = BulkRenameEdit::new(vec!["one".into(), "two".into()]).unwrap();
        for text in ["one\n", "one\n\n", "one\n../two\n"] {
            std::fs::write(edit.path(), text).unwrap();
            assert!(edit.read_names().is_err());
        }
        std::fs::File::create(edit.path())
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(edit.read_names().is_err());
        assert!(BulkRenameEdit::new(vec!["bad\nname".into()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn edit_rejects_non_utf8_names_and_replaced_symlink() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        assert!(
            BulkRenameEdit::new(vec![std::ffi::OsString::from_vec(vec![0xff]).into()]).is_err()
        );
        let edit = BulkRenameEdit::new(vec!["one".into()]).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "outside\n").unwrap();
        std::fs::remove_file(edit.path()).unwrap();
        symlink(outside.path(), edit.path()).unwrap();
        assert!(edit.read_names().is_err());
        drop(edit);
        assert_eq!(
            std::fs::read_to_string(outside.path()).unwrap(),
            "outside\n"
        );
    }
}
