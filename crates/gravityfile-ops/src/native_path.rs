//! Lossless, inspection-only path serialization shared by exported reports.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::path::Path;

pub(crate) struct NativePath<'a>(pub(crate) &'a Path);
impl Serialize for NativePath<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("NativePath", 3)?;
        record.serialize_field("display", &self.0.to_string_lossy())?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            record.serialize_field("platform", "unix")?;
            record.serialize_field("bytes", self.0.as_os_str().as_bytes())?;
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            record.serialize_field("platform", "windows")?;
            record.serialize_field(
                "wide",
                &self.0.as_os_str().encode_wide().collect::<Vec<_>>(),
            )?;
        }
        record.end()
    }
}
