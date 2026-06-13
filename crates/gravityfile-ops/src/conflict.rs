//! Conflict detection and resolution for file operations.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A conflict detected during a file operation.
#[derive(Debug, Clone)]
pub struct Conflict {
    /// The source path being operated on.
    pub source: PathBuf,
    /// The destination path where the conflict exists.
    pub destination: PathBuf,
    /// The kind of conflict.
    pub kind: ConflictKind,
}

impl Conflict {
    /// Create a new conflict.
    pub fn new(source: PathBuf, destination: PathBuf, kind: ConflictKind) -> Self {
        Self {
            source,
            destination,
            kind,
        }
    }

    /// Create a file exists conflict.
    pub fn file_exists(source: PathBuf, destination: PathBuf) -> Self {
        Self::new(source, destination, ConflictKind::FileExists)
    }

    /// Create a directory exists conflict.
    pub fn directory_exists(source: PathBuf, destination: PathBuf) -> Self {
        Self::new(source, destination, ConflictKind::DirectoryExists)
    }

    /// Create a source is ancestor conflict.
    pub fn source_is_ancestor(source: PathBuf, destination: PathBuf) -> Self {
        Self::new(source, destination, ConflictKind::SourceIsAncestor)
    }

    /// Create a permission denied conflict.
    pub fn permission_denied(source: PathBuf, destination: PathBuf) -> Self {
        Self::new(source, destination, ConflictKind::PermissionDenied)
    }
}

/// The kind of conflict encountered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictKind {
    /// A file already exists at the destination.
    FileExists,
    /// A directory already exists at the destination.
    DirectoryExists,
    /// Cannot move/copy a directory into itself.
    SourceIsAncestor,
    /// Permission denied.
    PermissionDenied,
    /// Source and destination are the same file.
    SameFile,
}

impl std::fmt::Display for ConflictKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileExists => write!(f, "File already exists"),
            Self::DirectoryExists => write!(f, "Directory already exists"),
            Self::SourceIsAncestor => write!(f, "Cannot copy/move a directory into itself"),
            Self::PermissionDenied => write!(f, "Permission denied"),
            Self::SameFile => write!(f, "Source and destination are the same file"),
        }
    }
}

/// How to resolve a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConflictResolution {
    /// Skip this item.
    #[default]
    Skip,
    /// Overwrite the existing item.
    Overwrite,
    /// Automatically rename the new item (e.g., "file (1).txt").
    AutoRename,
    /// Skip all remaining conflicts.
    SkipAll,
    /// Overwrite all remaining conflicts.
    OverwriteAll,
    /// Abort the entire operation.
    Abort,
}

impl ConflictResolution {
    /// Check if this resolution applies to all remaining conflicts.
    pub fn is_global(&self) -> bool {
        matches!(self, Self::SkipAll | Self::OverwriteAll | Self::Abort)
    }

    /// Convert a global resolution to its single-item equivalent.
    pub fn to_single(&self) -> Self {
        match self {
            Self::SkipAll => Self::Skip,
            Self::OverwriteAll => Self::Overwrite,
            _ => *self,
        }
    }
}

/// Returns true if nothing exists at `path` (including a dangling symlink).
///
/// Uses `symlink_metadata` rather than `exists()` so a broken symlink at the
/// candidate name is treated as occupied — `exists()` follows the link and
/// would report it free, leading us to clobber the link's would-be target.
fn path_is_free(path: &Path) -> bool {
    path.symlink_metadata().is_err()
}

/// Generate an auto-renamed path to avoid conflicts.
///
/// For "file.txt", tries "file (1).txt", "file (2).txt", etc.
///
/// This is a best-effort hint: callers still create the destination with
/// `create_new`/exclusive semantics, since any existence check is inherently
/// racy (TOCTOU).
pub fn auto_rename_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or(std::path::Path::new(""));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let extension = path.extension().and_then(|e| e.to_str());

    for i in 1..1000 {
        let new_name = if let Some(ext) = extension {
            format!("{} ({}).{}", stem, i, ext)
        } else {
            format!("{} ({})", stem, i)
        };

        let new_path = parent.join(&new_name);
        if path_is_free(&new_path) {
            return new_path;
        }
    }

    // Fallback: use a timestamp, still probing for a free name.
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for suffix in 0.. {
        let base = if suffix == 0 {
            format!("{}_{}", stem, timestamp)
        } else {
            format!("{}_{}_{}", stem, timestamp, suffix)
        };
        let new_name = match extension {
            Some(ext) => format!("{}.{}", base, ext),
            None => base,
        };
        let new_path = parent.join(&new_name);
        if path_is_free(&new_path) {
            return new_path;
        }
    }

    // Unreachable in practice (the loop above is effectively unbounded).
    parent.join(format!("{}_{}", stem, timestamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_rename_path() {
        let path = PathBuf::from("/tmp/test.txt");
        let renamed = auto_rename_path(&path);
        assert!(renamed.to_string_lossy().contains("test (1).txt"));
    }

    #[test]
    fn test_auto_rename_no_extension() {
        let path = PathBuf::from("/tmp/testfile");
        let renamed = auto_rename_path(&path);
        assert!(renamed.to_string_lossy().contains("testfile (1)"));
    }
}
