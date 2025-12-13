//! Conflict detection and resolution for file operations.

use std::path::PathBuf;

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

/// Generate an auto-renamed path to avoid conflicts.
///
/// For "file.txt", tries "file (1).txt", "file (2).txt", etc.
pub fn auto_rename_path(path: &PathBuf) -> PathBuf {
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
        if !new_path.exists() {
            return new_path;
        }
    }

    // Fallback: use timestamp
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let new_name = if let Some(ext) = extension {
        format!("{}_{}.{}", stem, timestamp, ext)
    } else {
        format!("{}_{}", stem, timestamp)
    };

    parent.join(&new_name)
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
