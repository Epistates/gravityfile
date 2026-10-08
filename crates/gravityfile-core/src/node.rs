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
///
/// Debug formatting includes this node's metadata and direct child count, not
/// descendant contents, so logging a node does not walk the entire tree.
#[derive(Serialize, Deserialize)]
pub struct FileNode {
    /// Unique identifier for this node.
    pub id: NodeId,

    /// File/directory name (not full path).
    pub name: CompactString,

    /// Exact platform filename when the display name cannot represent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_name: Option<Box<std::ffi::OsString>>,

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

impl fmt::Debug for FileNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileNode")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("native_name", &self.native_name)
            .field("kind", &self.kind)
            .field("size", &self.size)
            .field("blocks", &self.blocks)
            .field("timestamps", &self.timestamps)
            .field("inode", &self.inode)
            .field("content_hash", &self.content_hash)
            .field("git_status", &self.git_status)
            .field("child_count", &self.children.len())
            .finish()
    }
}

impl Drop for FileNode {
    fn drop(&mut self) {
        if self.children.is_empty() {
            return;
        }
        // Each drained node reaches its destructor with no children. Keep the
        // remaining siblings in their original allocation, one iterator per depth.
        let mut stack = vec![std::mem::take(&mut self.children).into_iter()];
        while let Some(children) = stack.last_mut() {
            let Some(mut child) = children.next() else {
                stack.pop();
                continue;
            };
            if !child.children.is_empty() {
                stack.push(std::mem::take(&mut child.children).into_iter());
            }
        }
    }
}

impl Clone for FileNode {
    fn clone(&self) -> Self {
        fn shallow(node: &FileNode) -> FileNode {
            // Exhaustive destructuring makes added metadata fields require a
            // deliberate cloning decision instead of silently losing them.
            let FileNode {
                id,
                name,
                native_name,
                kind,
                size,
                blocks,
                timestamps,
                inode,
                content_hash,
                git_status,
                children: _,
            } = node;
            FileNode {
                id: *id,
                name: name.clone(),
                native_name: native_name.clone(),
                kind: kind.clone(),
                size: *size,
                blocks: *blocks,
                timestamps: *timestamps,
                inode: *inode,
                content_hash: *content_hash,
                git_status: *git_status,
                children: Vec::new(),
            }
        }
        let mut cloned = shallow(self);
        cloned.children = self.children.iter().map(shallow).collect();
        let mut stack = vec![self.children.iter().zip(cloned.children.iter_mut())];
        while let Some(children) = stack.last_mut() {
            let Some((source, destination)) = children.next() else {
                stack.pop();
                continue;
            };
            destination.children = source.children.iter().map(shallow).collect();
            stack.push(source.children.iter().zip(destination.children.iter_mut()));
        }
        cloned
    }
}

impl FileNode {
    /// Compare compatibility labels with exact native identity as a tiebreak.
    pub fn compare_name(&self, other: &Self) -> std::cmp::Ordering {
        self.name
            .cmp(&other.name)
            .then_with(|| self.file_name().cmp(other.file_name()))
    }
    /// Terminal-safe filename label that preserves visible byte distinctions.
    pub fn display_name(&self) -> std::borrow::Cow<'_, str> {
        crate::escape_filename(self.file_name())
    }
    /// Filename component for filesystem access; `name` is display text only.
    pub fn file_name(&self) -> &std::ffi::OsStr {
        self.native_name
            .as_ref()
            .map(|name| name.as_os_str())
            .unwrap_or_else(|| std::ffi::OsStr::new(self.name.as_str()))
    }

    /// Preserve an exact component while retaining a Unicode display label.
    pub fn set_file_name(&mut self, name: &std::ffi::OsStr) {
        self.name = CompactString::new(name.to_string_lossy());
        self.native_name = name
            .to_str()
            .is_none()
            .then(|| Box::new(name.to_os_string()));
    }
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
            native_name: None,
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
            native_name: None,
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

    /// Sort children by size descending, breaking ties by name and native name.
    ///
    /// Uses stored sizes without recomputing them. Traversal retains one child
    /// iterator per directory depth instead of using recursive calls.
    pub fn sort_children_by_size(&mut self) {
        self.children
            .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));
        let mut stack = vec![self.children.iter_mut()];
        while let Some(children) = stack.last_mut() {
            let Some(child) = children.next() else {
                stack.pop();
                continue;
            };
            if child.is_dir() {
                child
                    .children
                    .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));
                stack.push(child.children.iter_mut());
            }
        }
    }

    /// Update directory file/dir counts with an iterative post-order traversal.
    ///
    /// Retains one traversal frame per directory depth. Symlinks and Other
    /// entries are counted in the file count; non-directory children are leaves.
    pub fn update_counts(&mut self) {
        let NodeKind::Directory {
            file_count,
            dir_count,
        } = &mut self.kind
        else {
            return;
        };
        let mut stack = vec![(file_count, dir_count, self.children.iter_mut(), 0u64, 0u64)];
        while let Some((_, _, children, files, _)) = stack.last_mut() {
            if let Some(child) = children.next() {
                if let NodeKind::Directory {
                    file_count,
                    dir_count,
                } = &mut child.kind
                {
                    stack.push((file_count, dir_count, child.children.iter_mut(), 0, 0));
                } else {
                    *files += 1;
                }
                continue;
            }
            // End the child borrow before propagating its completed totals.
            let (file_count, dir_count, _, files, dirs) = stack.pop().expect("active frame");
            *file_count = files;
            *dir_count = dirs;
            if let Some((_, _, _, parent_files, parent_dirs)) = stack.last_mut() {
                *parent_files += files;
                *parent_dirs += dirs + 1;
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
    #[ignore = "manual optimized tree lifecycle scale benchmark"]
    fn tree_lifecycle_scale_benchmark() {
        use std::hint::black_box;
        use std::time::Instant;
        for files in [100_000u64, 1_000_000, 10_000_000] {
            let started = Instant::now();
            let stamp = Timestamps::with_modified(std::time::UNIX_EPOCH);
            let mut root = FileNode::new_directory(NodeId::new(0), "root", stamp);
            for group in 0..files / 1000 {
                let mut directory = FileNode::new_directory(
                    NodeId::new(files + group + 1),
                    format!("d{group:05}"),
                    stamp,
                );
                directory.children = (0..1000)
                    .map(|offset| {
                        let id = group * 1000 + offset + 1;
                        FileNode::new_file(
                            NodeId::new(id),
                            format!("f{id:09}"),
                            256,
                            1,
                            stamp,
                            false,
                        )
                    })
                    .collect();
                directory.size = 256_000;
                root.children.push(directory);
            }
            root.size = files * 256;
            let construction_ns = started.elapsed().as_nanos();
            let started = Instant::now();
            let mut cloned = black_box(&root).clone();
            let clone_ns = started.elapsed().as_nanos();
            let started = Instant::now();
            cloned.finalize();
            let finalize_ns = started.elapsed().as_nanos();
            assert_eq!(cloned.file_count(), files);
            assert_eq!(cloned.dir_count(), files / 1000);
            assert_eq!(cloned.size, files * 256);
            assert_eq!(
                cloned
                    .children
                    .first()
                    .unwrap()
                    .children
                    .first()
                    .unwrap()
                    .id,
                NodeId::new(1)
            );
            assert_eq!(
                cloned.children.last().unwrap().children.last().unwrap().id,
                NodeId::new(files)
            );
            assert_eq!(
                root.file_count(),
                0,
                "clone finalization must leave source counts untouched"
            );
            let started = Instant::now();
            drop(black_box(cloned));
            let clone_drop_ns = started.elapsed().as_nanos();
            let started = Instant::now();
            drop(black_box(root));
            let source_drop_ns = started.elapsed().as_nanos();
            println!(
                "TREE_BENCH {{\"files\":{files},\"construction_ns\":{construction_ns},\"clone_ns\":{clone_ns},\"finalize_ns\":{finalize_ns},\"clone_drop_ns\":{clone_drop_ns},\"source_drop_ns\":{source_drop_ns}}}"
            );
        }
    }

    #[test]
    fn deep_tree_drops_on_a_small_thread_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let depth = 50_000u64;
                let mut root = FileNode::new_directory(
                    NodeId::new(depth),
                    "leaf",
                    Timestamps::with_modified(std::time::UNIX_EPOCH),
                );
                for id in (0..depth).rev() {
                    let mut parent = FileNode::new_directory(
                        NodeId::new(id),
                        "d",
                        Timestamps::with_modified(std::time::UNIX_EPOCH),
                    );
                    parent.children.push(root);
                    parent.children.push(FileNode::new_file(
                        NodeId::new(depth + id + 1),
                        "sibling",
                        1,
                        1,
                        Timestamps::with_modified(std::time::UNIX_EPOCH),
                        false,
                    ));
                    root = parent;
                }
                // Both formatting modes must stay independent of descendant
                // count and depth, even on a stack too small for recursive Debug.
                for formatted in [format!("{root:?}"), format!("{root:#?}")] {
                    assert!(formatted.contains("child_count: 2"));
                    assert!(!formatted.contains("sibling"));
                    assert!(formatted.len() < 2048);
                }
                drop(root);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn deep_tree_clone_is_independent_without_recursive_traversal() {
        let depth = 20_000u64;
        let mut root = FileNode::new_directory(
            NodeId::new(depth),
            "leaf",
            Timestamps::with_modified(std::time::UNIX_EPOCH),
        );
        for id in (0..depth).rev() {
            let mut parent = FileNode::new_directory(
                NodeId::new(id),
                "d",
                Timestamps::with_modified(std::time::UNIX_EPOCH),
            );
            parent.children.push(root);
            root = parent;
        }
        let mut cloned = root.clone();
        let mut current = &mut cloned;
        let mut copied_depth = 0;
        while !current.children.is_empty() {
            copied_depth += 1;
            current = &mut current.children[0];
        }
        let copied_id = current.id;
        current.name = "changed".into();
        let mut original = &root;
        while let Some(child) = original.children.first() {
            original = child;
        }
        let original_name = original.name.clone();
        drop(root);
        drop(cloned);
        assert_eq!(copied_depth, depth);
        assert_eq!(copied_id, NodeId::new(depth));
        assert_eq!(original_name, "leaf");
    }

    #[test]
    fn deep_directory_finalization_avoids_recursive_traversal() {
        let depth = 20_000u64;
        let mut node = FileNode::new_directory(
            NodeId::new(depth),
            "empty",
            Timestamps::with_modified(std::time::UNIX_EPOCH),
        );
        for id in (0..depth).rev() {
            let mut parent = FileNode::new_directory(
                NodeId::new(id),
                "d",
                Timestamps::with_modified(std::time::UNIX_EPOCH),
            );
            parent.kind = NodeKind::Directory {
                file_count: u64::MAX,
                dir_count: u64::MAX,
            };
            parent.children.push(node);
            parent.children.push(FileNode::new_file(
                NodeId::new(depth + id + 1),
                "f",
                1,
                1,
                Timestamps::with_modified(std::time::UNIX_EPOCH),
                false,
            ));
            node = parent;
        }
        node.finalize();
        let actual = node.kind.clone();
        let mut sorted_directories = 0;
        let mut correctly_sorted = node.children[0].size == 1 && node.children[1].size == 0;
        // Inspect ordering while draining the fixture.
        let mut pending = std::mem::take(&mut node.children);
        while let Some(mut child) = pending.pop() {
            if child.children.len() == 2 {
                sorted_directories += 1;
                correctly_sorted &= child.children[0].size == 1 && child.children[1].size == 0;
            }
            pending.append(&mut child.children);
        }
        assert_eq!(sorted_directories, depth - 1);
        assert!(correctly_sorted);
        assert!(
            matches!(actual, NodeKind::Directory { file_count, dir_count } if file_count == depth && dir_count == depth)
        );
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
            native_name: None,
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
