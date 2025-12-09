//! File and directory node types.

use std::time::SystemTime;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// Unique identifier for a node within a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);

impl NodeId {
    /// Create a new NodeId from a u64.
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

/// BLAKE3 content hash for duplicate detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    /// Create a new ContentHash from raw bytes.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Get the hash as a hex string.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Inode information for hardlink detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InodeInfo {
    /// Inode number.
    pub inode: u64,
    /// Device ID.
    pub device: u64,
}

impl InodeInfo {
    /// Create new inode info.
    pub fn new(inode: u64, device: u64) -> Self {
        Self { inode, device }
    }
}

/// File metadata timestamps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Timestamps {
    /// Last modification time.
    pub modified: SystemTime,
    /// Last access time (if available).
    pub accessed: Option<SystemTime>,
    /// Creation time (if available, platform-dependent).
    pub created: Option<SystemTime>,
}

impl Timestamps {
    /// Create timestamps with only modified time.
    pub fn with_modified(modified: SystemTime) -> Self {
        Self {
            modified,
            accessed: None,
            created: None,
        }
    }

    /// Create timestamps with all available times.
    pub fn new(
        modified: SystemTime,
        accessed: Option<SystemTime>,
        created: Option<SystemTime>,
    ) -> Self {
        Self {
            modified,
            accessed,
            created,
        }
    }
}

/// Type of file system node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeKind {
    /// Regular file.
    File {
        /// Whether the file is executable.
        executable: bool,
    },
    /// Directory.
    Directory {
        /// Total number of files in this subtree.
        file_count: u64,
        /// Total number of directories in this subtree.
        dir_count: u64,
    },
    /// Symbolic link.
    Symlink {
        /// Link target path.
        target: CompactString,
        /// Whether the link target exists.
        broken: bool,
    },
    /// Other file types (sockets, devices, etc.).
    Other,
}

impl NodeKind {
    /// Check if this is a directory.
    pub fn is_dir(&self) -> bool {
        matches!(self, NodeKind::Directory { .. })
    }

    /// Check if this is a regular file.
    pub fn is_file(&self) -> bool {
        matches!(self, NodeKind::File { .. })
    }

    /// Check if this is a symlink.
    pub fn is_symlink(&self) -> bool {
        matches!(self, NodeKind::Symlink { .. })
    }
}

/// A single file or directory in the tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    /// Unique identifier for this node.
    pub id: NodeId,

    /// File/directory name (not full path).
    pub name: CompactString,

    /// Node type and associated metadata.
    pub kind: NodeKind,

    /// Size in bytes (aggregate for directories).
    pub size: u64,

    /// Disk blocks actually used.
    pub blocks: u64,

    /// File metadata timestamps.
    pub timestamps: Timestamps,

    /// Inode info for hardlink detection.
    pub inode: Option<InodeInfo>,

    /// Content hash (computed on demand).
    pub content_hash: Option<ContentHash>,

    /// Children nodes (directories only), sorted by size descending.
    pub children: Vec<FileNode>,
}

impl FileNode {
    /// Create a new file node.
    pub fn new_file(
        id: NodeId,
        name: impl Into<CompactString>,
        size: u64,
        blocks: u64,
        timestamps: Timestamps,
        executable: bool,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind: NodeKind::File { executable },
            size,
            blocks,
            timestamps,
            inode: None,
            content_hash: None,
            children: Vec::new(),
        }
    }

    /// Create a new directory node.
    pub fn new_directory(
        id: NodeId,
        name: impl Into<CompactString>,
        timestamps: Timestamps,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind: NodeKind::Directory {
                file_count: 0,
                dir_count: 0,
            },
            size: 0,
            blocks: 0,
            timestamps,
            inode: None,
            content_hash: None,
            children: Vec::new(),
        }
    }

    /// Check if this node is a directory.
    pub fn is_dir(&self) -> bool {
        self.kind.is_dir()
    }

    /// Check if this node is a file.
    pub fn is_file(&self) -> bool {
        self.kind.is_file()
    }

    /// Get the number of direct children.
    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    /// Get file count for directories, 1 for files.
    pub fn file_count(&self) -> u64 {
        match &self.kind {
            NodeKind::Directory { file_count, .. } => *file_count,
            NodeKind::File { .. } => 1,
            _ => 0,
        }
    }

    /// Get directory count for directories.
    pub fn dir_count(&self) -> u64 {
        match &self.kind {
            NodeKind::Directory { dir_count, .. } => *dir_count,
            _ => 0,
        }
    }

    /// Sort children by size in descending order.
    pub fn sort_children_by_size(&mut self) {
        self.children.sort_by(|a, b| b.size.cmp(&a.size));
        for child in &mut self.children {
            child.sort_children_by_size();
        }
    }

    /// Update directory counts based on children.
    pub fn update_counts(&mut self) {
        if let NodeKind::Directory {
            ref mut file_count,
            ref mut dir_count,
        } = self.kind
        {
            *file_count = 0;
            *dir_count = 0;

            for child in &self.children {
                match &child.kind {
                    NodeKind::File { .. } => *file_count += 1,
                    NodeKind::Directory {
                        file_count: fc,
                        dir_count: dc,
                    } => {
                        *file_count += fc;
                        *dir_count += dc + 1;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_id() {
        let id = NodeId::new(42);
        assert_eq!(id.0, 42);
    }

    #[test]
    fn test_content_hash_hex() {
        let hash = ContentHash::new([0xab; 32]);
        assert_eq!(hash.to_hex().len(), 64);
        assert!(hash.to_hex().starts_with("abab"));
    }

    #[test]
    fn test_file_node_creation() {
        let node = FileNode::new_file(
            NodeId::new(1),
            "test.txt",
            1024,
            2,
            Timestamps::with_modified(SystemTime::now()),
            false,
        );
        assert!(node.is_file());
        assert!(!node.is_dir());
        assert_eq!(node.size, 1024);
    }

    #[test]
    fn test_directory_node_creation() {
        let node = FileNode::new_directory(
            NodeId::new(1),
            "test_dir",
            Timestamps::with_modified(SystemTime::now()),
        );
        assert!(node.is_dir());
        assert!(!node.is_file());
    }
}
