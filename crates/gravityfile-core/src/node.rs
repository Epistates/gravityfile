//! File and directory node types.

use std::fmt;
use std::time::SystemTime;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

/// Git status for a file or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum GitStatus {
    /// File/directory has been modified.
    Modified,
    /// File/directory is staged for commit.
    Staged,
    /// File/directory is not tracked by git.
    Untracked,
    /// File/directory is ignored by git.
    Ignored,
    /// File/directory has merge conflicts.
    Conflict,
    /// File/directory is clean (no changes).
    #[default]
    Clean,
}

impl GitStatus {
    /// Get a single character indicator for display.
    #[inline]
    pub fn indicator(&self) -> &'static str {
        match self {
            GitStatus::Modified => "M",
            GitStatus::Staged => "A",
            GitStatus::Untracked => "?",
            GitStatus::Ignored => "!",
            GitStatus::Conflict => "C",
            GitStatus::Clean => " ",
        }
    }

    /// Check if this status should be displayed (not clean).
    #[inline]
    pub fn is_displayable(&self) -> bool {
        !matches!(self, GitStatus::Clean)
    }
}

impl fmt::Display for GitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GitStatus::Modified => "Modified",
            GitStatus::Staged => "Staged",
            GitStatus::Untracked => "Untracked",
            GitStatus::Ignored => "Ignored",
            GitStatus::Conflict => "Conflict",
            GitStatus::Clean => "Clean",
        })
    }
}

/// Unique identifier for a node within a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(u64);

impl NodeId {
    /// Create a new NodeId from a u64.
    #[inline]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the inner u64 value.
    #[inline]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// BLAKE3 content hash for duplicate detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Create a new ContentHash from raw bytes.
    #[inline]
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Get the raw hash bytes.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Get the hash as a hex string.
    pub fn to_hex(&self) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(64);
        for byte in &self.0 {
            write!(out, "{byte:02x}").unwrap();
        }
        out
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
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
    #[inline]
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
    #[inline]
    pub fn with_modified(modified: SystemTime) -> Self {
        Self {
            modified,
            accessed: None,
            created: None,
        }
    }

    /// Create timestamps with all available times.
    #[inline]
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
    #[inline]
    pub fn is_dir(&self) -> bool {
        matches!(self, NodeKind::Directory { .. })
    }

    /// Check if this is a regular file.
    #[inline]
    pub fn is_file(&self) -> bool {
        matches!(self, NodeKind::File { .. })
    }

    /// Check if this is a symlink.
    #[inline]
    pub fn is_symlink(&self) -> bool {
        matches!(self, NodeKind::Symlink { .. })
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeKind::File { executable: true } => f.write_str("Executable"),
            NodeKind::File { executable: false } => f.write_str("File"),
            NodeKind::Directory { .. } => f.write_str("Directory"),
            NodeKind::Symlink { broken: true, .. } => f.write_str("Broken Symlink"),
            NodeKind::Symlink { .. } => f.write_str("Symlink"),
            NodeKind::Other => f.write_str("Other"),
        }
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

    /// Content hash (computed on demand). Stored inline — 32 bytes, no heap allocation.
    pub content_hash: Option<ContentHash>,

    /// Git status for this file/directory.
    #[serde(default)]
    pub git_status: Option<GitStatus>,

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
            git_status: None,
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
            git_status: None,
            children: Vec::new(),
        }
    }

    /// Check if this node is a directory.
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.kind.is_dir()
    }

    /// Check if this node is a file.
    #[inline]
    pub fn is_file(&self) -> bool {
        self.kind.is_file()
    }

    /// Get the number of direct children.
    #[inline]
    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    /// Get file count for directories (includes files, symlinks, and other entries),
    /// 1 for files/symlinks/other.
    #[inline]
    pub fn file_count(&self) -> u64 {
        match &self.kind {
            NodeKind::Directory { file_count, .. } => *file_count,
            NodeKind::File { .. } | NodeKind::Symlink { .. } | NodeKind::Other => 1,
        }
    }

    /// Get directory count for directories.
    #[inline]
    pub fn dir_count(&self) -> u64 {
        match &self.kind {
            NodeKind::Directory { dir_count, .. } => *dir_count,
            _ => 0,
        }
    }

    /// Sort children by size in descending order, with deterministic secondary sort by name.
    ///
    /// Must be called after [`update_counts`](Self::update_counts) for correct directory sizes.
    #[inline]
    pub fn sort_children_by_size(&mut self) {
        self.children
            .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        for child in &mut self.children {
            if child.is_dir() {
                child.sort_children_by_size();
            }
        }
    }

    /// Recursively update directory file/dir counts based on children (post-order traversal).
    ///
    /// This recurses into all children first, then aggregates counts upward.
    /// Symlinks and Other entries are counted in the file count.
    pub fn update_counts(&mut self) {
        if let NodeKind::Directory {
            ref mut file_count,
            ref mut dir_count,
        } = self.kind
        {
            // Recurse into children first (post-order)
            for child in &mut self.children {
                child.update_counts();
            }

            *file_count = 0;
            *dir_count = 0;

            for child in &self.children {
                match &child.kind {
                    NodeKind::File { .. } | NodeKind::Symlink { .. } | NodeKind::Other => {
                        *file_count += 1;
                    }
                    NodeKind::Directory {
                        file_count: fc,
                        dir_count: dc,
                    } => {
                        *file_count += fc;
                        *dir_count += dc + 1;
                    }
                }
            }
        }
    }

    /// Update counts and sort children in the correct order.
    pub fn finalize(&mut self) {
        self.update_counts();
        self.sort_children_by_size();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_id() {
        let id = NodeId::new(42);
        assert_eq!(id.get(), 42);
        assert_eq!(format!("{id}"), "42");
    }

    #[test]
    fn test_content_hash_hex() {
        let hash = ContentHash::new([0xab; 32]);
        assert_eq!(hash.to_hex().len(), 64);
        assert!(hash.to_hex().starts_with("abab"));
        // Test Display impl
        assert_eq!(format!("{hash}"), hash.to_hex());
    }

    #[test]
    fn test_content_hash_as_bytes() {
        let bytes = [0xcd; 32];
        let hash = ContentHash::new(bytes);
        assert_eq!(hash.as_bytes(), &bytes);
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

    #[test]
    fn test_update_counts_recursive() {
        let now = SystemTime::now();
        let mut root =
            FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));
        let mut dir1 =
            FileNode::new_directory(NodeId::new(2), "dir1", Timestamps::with_modified(now));
        let mut dir2 =
            FileNode::new_directory(NodeId::new(3), "dir2", Timestamps::with_modified(now));
        let file1 = FileNode::new_file(
            NodeId::new(4),
            "f1",
            100,
            1,
            Timestamps::with_modified(now),
            false,
        );
        let file2 = FileNode::new_file(
            NodeId::new(5),
            "f2",
            200,
            1,
            Timestamps::with_modified(now),
            false,
        );

        dir2.children.push(file2);
        dir1.children.push(dir2);
        dir1.children.push(file1);
        root.children.push(dir1);

        // Single call should recursively update everything
        root.update_counts();

        assert_eq!(root.file_count(), 2); // f1 + f2
        assert_eq!(root.dir_count(), 2); // dir1 + dir2
    }

    #[test]
    fn test_update_counts_includes_symlinks() {
        let now = SystemTime::now();
        let mut root =
            FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));
        let file = FileNode::new_file(
            NodeId::new(2),
            "f1",
            100,
            1,
            Timestamps::with_modified(now),
            false,
        );
        let symlink = FileNode {
            id: NodeId::new(3),
            name: "link".into(),
            kind: NodeKind::Symlink {
                target: "target".into(),
                broken: false,
            },
            size: 0,
            blocks: 0,
            timestamps: Timestamps::with_modified(now),
            inode: None,
            content_hash: None,
            git_status: None,
            children: Vec::new(),
        };
        root.children.push(file);
        root.children.push(symlink);
        root.update_counts();
        assert_eq!(root.file_count(), 2); // file + symlink
    }

    #[test]
    fn test_sort_deterministic() {
        let now = SystemTime::now();
        let mut root =
            FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));
        let f1 = FileNode::new_file(
            NodeId::new(2),
            "bbb",
            100,
            1,
            Timestamps::with_modified(now),
            false,
        );
        let f2 = FileNode::new_file(
            NodeId::new(3),
            "aaa",
            100,
            1,
            Timestamps::with_modified(now),
            false,
        );
        root.children.push(f1);
        root.children.push(f2);
        root.sort_children_by_size();
        // Same size => sorted by name ascending
        assert_eq!(root.children[0].name.as_str(), "aaa");
        assert_eq!(root.children[1].name.as_str(), "bbb");
    }

    #[test]
    fn test_git_status_display() {
        assert_eq!(format!("{}", GitStatus::Modified), "Modified");
        assert_eq!(format!("{}", GitStatus::Clean), "Clean");
    }

    #[test]
    fn test_node_kind_display() {
        assert_eq!(format!("{}", NodeKind::File { executable: false }), "File");
        assert_eq!(
            format!("{}", NodeKind::File { executable: true }),
            "Executable"
        );
        assert_eq!(
            format!(
                "{}",
                NodeKind::Directory {
                    file_count: 0,
                    dir_count: 0
                }
            ),
            "Directory"
        );
    }
}
