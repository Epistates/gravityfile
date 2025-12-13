//! JWalk-based parallel directory scanner.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use compact_str::CompactString;
use jwalk::{Parallelism, WalkDir};
use tokio::sync::broadcast;

use gravityfile_core::{
    FileNode, FileTree, InodeInfo, NodeId, NodeKind, ScanConfig, ScanError, ScanWarning,
    Timestamps, TreeStats, WarningKind,
};

use crate::inode::InodeTracker;
use crate::progress::ScanProgress;

/// High-performance scanner using jwalk for parallel traversal.
pub struct JwalkScanner {
    progress_tx: broadcast::Sender<ScanProgress>,
}

impl JwalkScanner {
    /// Create a new scanner.
    pub fn new() -> Self {
        let (progress_tx, _) = broadcast::channel(100);
        Self { progress_tx }
    }

    /// Subscribe to scan progress updates.
    pub fn subscribe(&self) -> broadcast::Receiver<ScanProgress> {
        self.progress_tx.subscribe()
    }

    /// Perform a scan of the given path.
    pub fn scan(&self, config: &ScanConfig) -> Result<FileTree, ScanError> {
        let start = Instant::now();
        let root_path = config.root.canonicalize().map_err(|e| ScanError::io(&config.root, e))?;

        // Verify root is a directory
        if !root_path.is_dir() {
            return Err(ScanError::NotADirectory { path: root_path });
        }

        // Get root device for cross-filesystem detection
        let root_metadata = std::fs::metadata(&root_path).map_err(|e| ScanError::io(&root_path, e))?;
        let root_device = get_dev(&root_metadata);

        // Set up tracking
        let inode_tracker = InodeTracker::new();
        let node_id_counter = AtomicU64::new(0);
        let mut stats = TreeStats::new();
        let mut warnings = Vec::new();

        // Collect all entries first
        let entries = self.collect_entries(config, &root_path, root_device, &inode_tracker, &mut stats, &mut warnings)?;

        // Build tree from collected entries
        let root_node = self.build_tree(&root_path, entries, &node_id_counter, &mut stats);

        let scan_duration = start.elapsed();

        Ok(FileTree::new(
            root_node,
            root_path,
            config.clone(),
            stats,
            scan_duration,
            warnings,
        ))
    }

    /// Collect all entries using jwalk.
    fn collect_entries(
        &self,
        config: &ScanConfig,
        root_path: &Path,
        root_device: u64,
        inode_tracker: &InodeTracker,
        stats: &mut TreeStats,
        warnings: &mut Vec<ScanWarning>,
    ) -> Result<HashMap<PathBuf, Vec<EntryInfo>>, ScanError> {
        let parallelism = match config.threads {
            0 => Parallelism::RayonDefaultPool { busy_timeout: std::time::Duration::from_millis(100) },
            n => Parallelism::RayonNewPool(n),
        };

        let walker = WalkDir::new(root_path)
            .parallelism(parallelism)
            .skip_hidden(!config.include_hidden)
            .follow_links(config.follow_symlinks)
            .min_depth(0)
            .max_depth(config.max_depth.map(|d| d as usize).unwrap_or(usize::MAX));

        // Map from parent path to children
        let mut entries_by_parent: HashMap<PathBuf, Vec<EntryInfo>> = HashMap::new();
        let progress_counter = Arc::new(AtomicU64::new(0));

        for entry_result in walker {
            let entry = match entry_result {
                Ok(e) => e,
                Err(err) => {
                    let path = err.path().map(|p| p.to_path_buf()).unwrap_or_default();
                    warnings.push(ScanWarning::new(
                        path,
                        err.to_string(),
                        WarningKind::ReadError,
                    ));
                    continue;
                }
            };

            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Check ignore patterns
            if config.should_ignore(&file_name) {
                continue;
            }

            // Get metadata
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(err) => {
                    warnings.push(ScanWarning::new(
                        &path,
                        err.to_string(),
                        WarningKind::MetadataError,
                    ));
                    continue;
                }
            };

            // Check cross-filesystem
            if !config.cross_filesystems && get_dev(&metadata) != root_device {
                continue;
            }

            // Handle different file types
            let file_type = entry.file_type();
            let depth = entry.depth() as u32;

            if file_type.is_dir() {
                stats.record_dir(depth);

                // For directories, track them but size will be aggregated later
                if let Some(parent) = path.parent() {
                    let entry_info = EntryInfo {
                        name: file_name.into(),
                        path: path.clone(),
                        size: 0,
                        blocks: 0,
                        is_dir: true,
                        is_symlink: false,
                        executable: false,
                        timestamps: Timestamps::new(
                            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                            metadata.accessed().ok(),
                            metadata.created().ok(),
                        ),
                        inode: Some(InodeInfo::new(get_ino(&metadata), get_dev(&metadata))),
                    };

                    entries_by_parent
                        .entry(parent.to_path_buf())
                        .or_default()
                        .push(entry_info);
                }
            } else if file_type.is_file() {
                // Check for hardlinks
                let inode_info = InodeInfo::new(get_ino(&metadata), get_dev(&metadata));
                let size = if config.apparent_size {
                    metadata.len()
                } else {
                    // Only count size for first hardlink
                    if get_nlink(&metadata) > 1 && !inode_tracker.track(inode_info) {
                        0 // Already counted this inode
                    } else {
                        metadata.len()
                    }
                };

                let blocks = get_blocks(&metadata);

                stats.record_file(
                    path.clone(),
                    size,
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    depth,
                );

                if let Some(parent) = path.parent() {
                    let executable = is_executable(&metadata);
                    let entry_info = EntryInfo {
                        name: file_name.into(),
                        path: path.clone(),
                        size,
                        blocks,
                        is_dir: false,
                        is_symlink: false,
                        executable,
                        timestamps: Timestamps::new(
                            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                            metadata.accessed().ok(),
                            metadata.created().ok(),
                        ),
                        inode: Some(inode_info),
                    };

                    entries_by_parent
                        .entry(parent.to_path_buf())
                        .or_default()
                        .push(entry_info);
                }

                // Update progress periodically
                let count = progress_counter.fetch_add(1, Ordering::Relaxed);
                if count % 1000 == 0 {
                    let _ = self.progress_tx.send(ScanProgress {
                        files_scanned: stats.total_files,
                        dirs_scanned: stats.total_dirs,
                        bytes_scanned: stats.total_size,
                        current_path: path.clone(),
                        errors_count: warnings.len() as u64,
                        elapsed: std::time::Duration::ZERO, // Will be set properly at end
                    });
                }
            } else if file_type.is_symlink() {
                stats.record_symlink();

                if let Some(parent) = path.parent() {
                    let target = std::fs::read_link(&path)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();

                    let broken = !path.exists();
                    if broken {
                        warnings.push(ScanWarning::broken_symlink(&path, &target));
                    }

                    let entry_info = EntryInfo {
                        name: file_name.into(),
                        path: path.clone(),
                        size: 0,
                        blocks: 0,
                        is_dir: false,
                        is_symlink: true,
                        executable: false,
                        timestamps: Timestamps::new(
                            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                            metadata.accessed().ok(),
                            metadata.created().ok(),
                        ),
                        inode: None,
                    };

                    entries_by_parent
                        .entry(parent.to_path_buf())
                        .or_default()
                        .push(entry_info);
                }
            }
        }

        Ok(entries_by_parent)
    }

    /// Build tree structure from collected entries.
    fn build_tree(
        &self,
        root_path: &Path,
        mut entries_by_parent: HashMap<PathBuf, Vec<EntryInfo>>,
        node_id_counter: &AtomicU64,
        stats: &mut TreeStats,
    ) -> FileNode {
        self.build_node(root_path, &mut entries_by_parent, node_id_counter, stats)
    }

    /// Recursively build a node and its children.
    fn build_node(
        &self,
        path: &Path,
        entries_by_parent: &mut HashMap<PathBuf, Vec<EntryInfo>>,
        node_id_counter: &AtomicU64,
        stats: &mut TreeStats,
    ) -> FileNode {
        let id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        let metadata = std::fs::metadata(path).ok();
        let timestamps = metadata
            .as_ref()
            .map(|m| {
                Timestamps::new(
                    m.modified().unwrap_or(std::time::UNIX_EPOCH),
                    m.accessed().ok(),
                    m.created().ok(),
                )
            })
            .unwrap_or_else(|| Timestamps::with_modified(std::time::UNIX_EPOCH));

        let mut node = FileNode::new_directory(id, name, timestamps);

        // Get children for this path
        let children_entries = entries_by_parent.remove(path).unwrap_or_default();

        let mut total_size: u64 = 0;
        let mut file_count: u64 = 0;
        let mut dir_count: u64 = 0;

        for entry in children_entries {
            if entry.is_dir {
                // Recursively build directory
                let child_node = self.build_node(&entry.path, entries_by_parent, node_id_counter, stats);
                total_size += child_node.size;
                file_count += child_node.file_count();
                dir_count += child_node.dir_count() + 1;
                node.children.push(child_node);
            } else if entry.is_symlink {
                // Create symlink node
                let child_id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));
                let target = std::fs::read_link(&entry.path)
                    .map(|p| CompactString::new(p.to_string_lossy()))
                    .unwrap_or_default();
                let broken = !entry.path.exists();

                let child_node = FileNode {
                    id: child_id,
                    name: entry.name,
                    kind: NodeKind::Symlink { target, broken },
                    size: 0,
                    blocks: 0,
                    timestamps: entry.timestamps,
                    inode: None,
                    content_hash: None,
                    children: Vec::new(),
                };
                node.children.push(child_node);
            } else {
                // Create file node
                let child_id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));
                let mut child_node = FileNode::new_file(
                    child_id,
                    entry.name,
                    entry.size,
                    entry.blocks,
                    entry.timestamps,
                    entry.executable,
                );
                child_node.inode = entry.inode;

                total_size += entry.size;
                file_count += 1;
                node.children.push(child_node);
            }
        }

        // Update node with aggregated values
        node.size = total_size;
        node.kind = NodeKind::Directory {
            file_count,
            dir_count,
        };

        // Sort children by size (descending)
        node.children.sort_by(|a, b| b.size.cmp(&a.size));

        node
    }
}

impl Default for JwalkScanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Temporary struct for collecting entry information.
struct EntryInfo {
    name: CompactString,
    path: PathBuf,
    size: u64,
    blocks: u64,
    is_dir: bool,
    is_symlink: bool,
    executable: bool,
    timestamps: Timestamps,
    inode: Option<InodeInfo>,
}

/// Check if a file is executable (Unix).
#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

// Cross-platform metadata helpers

/// Get the device ID from metadata.
#[cfg(unix)]
fn get_dev(metadata: &std::fs::Metadata) -> u64 {
    metadata.dev()
}

#[cfg(not(unix))]
fn get_dev(_metadata: &std::fs::Metadata) -> u64 {
    0 // Windows doesn't have device IDs in the same way
}

/// Get the inode number from metadata.
#[cfg(unix)]
fn get_ino(metadata: &std::fs::Metadata) -> u64 {
    metadata.ino()
}

#[cfg(not(unix))]
fn get_ino(_metadata: &std::fs::Metadata) -> u64 {
    0 // Windows doesn't have inodes
}

/// Get the number of hard links from metadata.
#[cfg(unix)]
fn get_nlink(metadata: &std::fs::Metadata) -> u64 {
    metadata.nlink()
}

#[cfg(not(unix))]
fn get_nlink(_metadata: &std::fs::Metadata) -> u64 {
    1 // Assume single link on Windows
}

/// Get the number of 512-byte blocks from metadata.
#[cfg(unix)]
fn get_blocks(metadata: &std::fs::Metadata) -> u64 {
    metadata.blocks()
}

#[cfg(not(unix))]
fn get_blocks(metadata: &std::fs::Metadata) -> u64 {
    // Estimate blocks from file size (512-byte blocks, rounded up)
    (metadata.len() + 511) / 512
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_tree() -> TempDir {
        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create directory structure
        fs::create_dir(root.join("dir1")).unwrap();
        fs::create_dir(root.join("dir2")).unwrap();
        fs::create_dir(root.join("dir1/subdir")).unwrap();

        // Create files
        fs::write(root.join("file1.txt"), "hello").unwrap();
        fs::write(root.join("dir1/file2.txt"), "world world world").unwrap();
        fs::write(root.join("dir1/subdir/file3.txt"), "test").unwrap();
        fs::write(root.join("dir2/file4.txt"), "another file here").unwrap();

        temp
    }

    #[test]
    fn test_basic_scan() {
        let temp = create_test_tree();
        let config = ScanConfig::new(temp.path());

        let scanner = JwalkScanner::new();
        let tree = scanner.scan(&config).unwrap();

        assert_eq!(tree.stats.total_files, 4);
        // dir1, dir2, subdir + root = 4, but root not counted in walker
        assert!(tree.stats.total_dirs >= 3);
        assert!(tree.root.size > 0);
    }

    #[test]
    fn test_children_sorted_by_size() {
        let temp = create_test_tree();
        let config = ScanConfig::new(temp.path());

        let scanner = JwalkScanner::new();
        let tree = scanner.scan(&config).unwrap();

        // Children should be sorted by size descending
        for i in 0..tree.root.children.len().saturating_sub(1) {
            assert!(tree.root.children[i].size >= tree.root.children[i + 1].size);
        }
    }

    #[test]
    fn test_ignore_patterns() {
        let temp = create_test_tree();
        let config = ScanConfig::builder()
            .root(temp.path())
            .ignore_patterns(vec!["dir2".to_string()])
            .build()
            .unwrap();

        let scanner = JwalkScanner::new();
        let tree = scanner.scan(&config).unwrap();

        // dir2 should be ignored
        assert!(!tree
            .root
            .children
            .iter()
            .any(|c| c.name.as_str() == "dir2"));
    }
}
