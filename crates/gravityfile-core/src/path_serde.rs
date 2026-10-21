//! JSON-compatible native paths. Unicode paths retain their legacy string shape.
use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::{Path, PathBuf};

pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    if !serializer.is_human_readable() || path.to_str().is_some() {
        return path.serialize(serializer);
    }
    let mut record = serializer.serialize_struct("NativePath", 2)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        record.serialize_field("platform", "unix")?;
        record.serialize_field("bytes", path.as_os_str().as_bytes())?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        record.serialize_field("platform", "windows")?;
        record.serialize_field("wide", &path.as_os_str().encode_wide().collect::<Vec<_>>())?;
    }
    #[cfg(not(any(unix, windows)))]
    return Err(serde::ser::Error::custom(
        "native path encoding unavailable on this platform",
    ));
    record.end()
}

#[derive(Deserialize)]
#[serde(tag = "platform", rename_all = "lowercase", deny_unknown_fields)]
enum NativePath {
    Unix { bytes: Vec<u8> },
    Windows { wide: Vec<u16> },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HumanPath {
    Text(String),
    Native(NativePath),
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    if !deserializer.is_human_readable() {
        return PathBuf::deserialize(deserializer);
    }
    match HumanPath::deserialize(deserializer)? {
        HumanPath::Text(text) => Ok(PathBuf::from(text)),
        HumanPath::Native(NativePath::Unix { bytes }) => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                Ok(std::ffi::OsString::from_vec(bytes).into())
            }
            #[cfg(not(unix))]
            {
                let _ = bytes;
                Err(serde::de::Error::custom(
                    "Unix native path cannot be decoded on this platform",
                ))
            }
        }
        HumanPath::Native(NativePath::Windows { wide }) => {
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;
                Ok(std::ffi::OsString::from_wide(&wide).into())
            }
            #[cfg(not(windows))]
            {
                let _ = wide;
                Err(serde::de::Error::custom(
                    "Windows native path cannot be decoded on this platform",
                ))
            }
        }
    }
}

struct PathRef<'a>(&'a Path);
impl Serialize for PathRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize(self.0, serializer)
    }
}
#[derive(Deserialize)]
struct OwnedPath(#[serde(deserialize_with = "deserialize")] PathBuf);

/// Adapter for lists of paths.
pub mod list {
    use super::*;
    pub fn serialize<S: Serializer>(paths: &[PathBuf], serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(paths.len()))?;
        for path in paths {
            sequence.serialize_element(&PathRef(path))?;
        }
        sequence.end()
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<PathBuf>, D::Error> {
        Ok(Vec::<OwnedPath>::deserialize(deserializer)?
            .into_iter()
            .map(|path| path.0)
            .collect())
    }
}

/// Adapter for an optional path/value tuple, preserving its array shape.
pub mod optional_pair {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(
        pair: &Option<(PathBuf, T)>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match pair {
            Some((path, value)) => serializer.serialize_some(&(PathRef(path), value)),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<(PathBuf, T)>, D::Error> {
        Ok(Option::<(OwnedPath, T)>::deserialize(deserializer)?
            .map(|(path, value)| (path.0, value)))
    }
}

/// Adapter for age-report path/size/time tuples.
pub mod triples {
    use super::*;
    pub fn serialize<S: Serializer, A: Serialize, B: Serialize>(
        values: &[(PathBuf, A, B)],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(values.len()))?;
        for (path, a, b) in values {
            sequence.serialize_element(&(PathRef(path), a, b))?;
        }
        sequence.end()
    }
    pub fn deserialize<'de, D: Deserializer<'de>, A: Deserialize<'de>, B: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Vec<(PathBuf, A, B)>, D::Error> {
        Ok(Vec::<(OwnedPath, A, B)>::deserialize(deserializer)?
            .into_iter()
            .map(|(path, a, b)| (path.0, a, b))
            .collect())
    }
}
