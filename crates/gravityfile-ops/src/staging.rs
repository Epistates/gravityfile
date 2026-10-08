//! Temporary directories that protect staged user data from other local users.
use std::io;
use std::path::Path;

/// Reserve a sibling directory, with owner-only permissions at creation on Unix.
/// Windows uses inherited ACLs; this helper does not establish a custom DACL.
pub(crate) fn private_directory(parent: &Path, prefix: &str) -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn staged_payload_is_behind_an_owner_only_directory() {
        let parent = tempfile::tempdir().unwrap();
        let directory = private_directory(parent.path(), ".staging-test-").unwrap();
        let payload = directory.path().join("payload");
        std::fs::write(&payload, b"private user data").unwrap();
        std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            std::fs::metadata(directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(std::fs::read(payload).unwrap(), b"private user data");
    }
}
