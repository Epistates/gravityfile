//! JWalk-based parallel directory scanner.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use compact_str::CompactString;
use jwalk::{DirEntry, Parallelism, WalkDirGeneric};
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
        let root_path = config
            .root
            .canonicalize()
            .map_err(|e| ScanError::io(&config.root, e))?;

        // Verify root is a directory
        if !root_path.is_dir() {
            return Err(ScanError::NotADirectory { path: root_path });
        }

        // Get root device for cross-filesystem detection
        let root_metadata =
            std::fs::metadata(&root_path).map_err(|e| ScanError::io(&root_path, e))?;
        let root_device = get_dev(&root_metadata);

        // Set up tracking
        let mut inode_tracker = InodeTracker::new();
        let node_id_counter = AtomicU64::new(0);
        let mut stats = TreeStats::new();
        let mut warnings = Vec::new();

        // Collect all entries first
        let entries = self.collect_entries(
            config,
            &root_path,
            root_device,
            &mut inode_tracker,
            &mut stats,
            &mut warnings,
        )?;

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
        inode_tracker: &mut InodeTracker,
        stats: &mut TreeStats,
        warnings: &mut Vec<ScanWarning>,
    ) -> Result<HashMap<PathBuf, Vec<EntryInfo>>, ScanError> {
        // Platform-specific thread default: 4 on macOS, rayon default everywhere else.
        let parallelism = match config.threads {
            0 => {
                #[cfg(target_os = "macos")]
                {
                    Parallelism::RayonNewPool(4)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Parallelism::RayonDefaultPool {
                        busy_timeout: std::time::Duration::from_millis(100),
                    }
                }
            }
            n => Parallelism::RayonNewPool(n),
        };

        // Capture config fields needed in the closure.
        let cross_filesystems = config.cross_filesystems;
        let include_hidden = config.include_hidden;

        // Re-use the GlobSet already compiled by ScanConfig when available.
        // Fall back to compiling on-the-fly if patterns exist but weren't compiled
        // (e.g. config built via the derive_builder path without calling compile_patterns).
        let ignore_globset: Option<Arc<globset::GlobSet>> = config
            .compiled_ignore_set()
            .cloned()
            .map(Arc::new)
            .or_else(|| {
                if config.ignore_patterns.is_empty() {
                    return None;
                }
                let mut builder = globset::GlobSetBuilder::new();
                for pattern in &config.ignore_patterns {
                    if let Ok(glob) = globset::Glob::new(pattern) {
                        builder.add(glob);
                    }
                }
                builder.build().ok().map(Arc::new)
            });

        let walker = WalkDirGeneric::<((), ())>::new(root_path)
            .parallelism(parallelism)
            .skip_hidden(!include_hidden)
            .follow_links(config.follow_symlinks)
            .min_depth(0)
            .max_depth(config.max_depth.map(|d| d as usize).unwrap_or(usize::MAX))
            .process_read_dir(move |_depth, _dir_path, _state, children| {
                // Prune and filter early — before jwalk recurses.
                children.retain_mut(|entry_result| {
                    let entry = match entry_result {
                        Ok(e) => e,
                        Err(_) => return true, // keep errors so they surface as warnings
                    };

                    let name = entry.file_name.to_string_lossy();

                    // Apply ignore-pattern filter.
                    if let Some(ref gs) = ignore_globset
                        && gs.is_match(name.as_ref())
                    {
                        return false;
                    }

                    // Prune cross-filesystem subtrees for directories.
                    if !cross_filesystems
                        && entry.file_type.is_dir()
                        && let Ok(meta) = entry.metadata()
                        && get_dev(&meta) != root_device
                    {
                        // Setting read_children_path to None stops jwalk
                        // from descending into this directory.
                        entry.read_children_path = None;
                        return false; // drop the dir entry itself too
                    }

                    true
                });
            });

        // Map from parent path to children
        let mut entries_by_parent: HashMap<PathBuf, Vec<EntryInfo>> = HashMap::new();
        let mut progress_counter: u64 = 0;

        for entry_result in walker {
            let entry: DirEntry<((), ())> = match entry_result {
                Ok(e) => e,
                Err(err) => {
                    let path = err.path().map(|p| p.to_path_buf()).unwrap_or_default();
                    warnings.push(ScanWarning::new(
                        path,
                        WarningKind::ReadError,
                        err.to_string(),
                    ));
                    continue;
                }
            };

            let path = entry.path();
            // Use CompactString directly to avoid an extra heap allocation.
            let file_name = CompactString::new(entry.file_name().to_string_lossy());

            // Get metadata
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(err) => {
                    warnings.push(ScanWarning::new(
                        &path,
                        WarningKind::MetadataError,
                        err.to_string(),
                    ));
                    continue;
                }
            };

            // Handle different file types
            let file_type = entry.file_type();
            let depth = entry.depth() as u32;

            if file_type.is_dir() {
                stats.record_dir(depth);

                // For directories, track them but size will be aggregated later
                if let Some(parent) = path.parent() {
                    let entry_info = EntryInfo {
                        name: file_name,
                        path: path.clone(),
                        size: 0,
                        blocks: 0,
                        is_dir: true,
                        is_symlink: false,
                        symlink_target: None,
                        symlink_broken: false,
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
                // Filter non-directory entries on different filesystems at
                // depth 1 (directly under scan root). Deeper entries are
                // already excluded by the directory-prune in process_read_dir.
                if !cross_filesystems && get_dev(&metadata) != root_device {
                    continue;
                }

                let nlink = get_nlink(&metadata);
                let inode_info = InodeInfo::new(get_ino(&metadata), get_dev(&metadata));

                let size = if config.apparent_size {
                    metadata.len()
                } else {
                    // Only count size for first hardlink encounter.
                    if nlink > 1 && !inode_tracker.track(inode_info, nlink) {
                        0 // Already counted this inode
                    } else {
                        // Use disk blocks for physical size when apparent_size == false.
                        disk_size(&metadata)
                    }
                };

                let blocks = get_blocks(&metadata);

                stats.record_file(
                    &path,
                    size,
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    depth,
                );

                if let Some(parent) = path.parent() {
                    let executable = is_executable(&metadata);
                    let entry_info = EntryInfo {
                        name: file_name,
                        path: path.clone(),
                        size,
                        blocks,
                        is_dir: false,
                        is_symlink: false,
                        symlink_target: None,
                        symlink_broken: false,
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
                progress_counter += 1;
                if progress_counter.is_multiple_of(1000) {
                    let _ = self.progress_tx.send(ScanProgress {
                        files_scanned: stats.total_files,
                        dirs_scanned: stats.total_dirs,
                        bytes_scanned: stats.total_size,
                        current_path: path.clone(),
                        errors_count: warnings.len() as u64,
                        elapsed: std::time::Duration::ZERO,
                    });
                }
            } else if file_type.is_symlink() {
                // Filter symlinks on different filesystems at depth 1.
                if !cross_filesystems && get_dev(&metadata) != root_device {
                    continue;
                }

                stats.record_symlink();

                if let Some(parent) = path.parent() {
                    // Read symlink target once; re-use for both the warning and
                    // the EntryInfo so we never call read_link twice.
                    let (symlink_target, symlink_broken) = match std::fs::read_link(&path) {
                        Ok(target) => {
                            // path.exists() follows the link; broken if it fails.
                            let broken = !path.exists();
                            let target_str = CompactString::new(target.to_string_lossy());
                            (target_str, broken)
                        }
                        Err(_) => (CompactString::default(), true),
                    };

                    if symlink_broken {
                        warnings.push(ScanWarning::broken_symlink(&path, symlink_target.as_str()));
                    }

                    let entry_info = EntryInfo {
                        name: file_name,
                        path: path.clone(),
                        size: 0,
                        blocks: 0,
                        is_dir: false,
                        is_symlink: true,
                        symlink_target: Some(symlink_target),
                        symlink_broken,
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
        _stats: &mut TreeStats,
    ) -> FileNode {
        self.build_node(root_path, &mut entries_by_parent, node_id_counter)
    }

    /// Recursively build a node and its children.
    fn build_node(
        &self,
        path: &Path,
        entries_by_parent: &mut HashMap<PathBuf, Vec<EntryInfo>>,
        node_id_counter: &AtomicU64,
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
                let child_node = self.build_node(&entry.path, entries_by_parent, node_id_counter);
                total_size += child_node.size;
                file_count += child_node.file_count();
                dir_count += child_node.dir_count() + 1;
                node.children.push(child_node);
            } else if entry.is_symlink {
                // Re-use the already-resolved target and broken status from EntryInfo —
                // no second syscall needed here.
                let child_id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));
                let target = entry.symlink_target.unwrap_or_default();
                let broken = entry.symlink_broken;

                let child_node = FileNode {
                    id: child_id,
                    name: entry.name,
                    kind: NodeKind::Symlink { target, broken },
                    size: 0,
                    blocks: 0,
                    timestamps: entry.timestamps,
                    inode: None,
                    content_hash: None,
                    git_status: None,
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

/// Create a quick, non-recursive directory listing for immediate display.
/// This function reads only the immediate children of a directory without
/// recursing into subdirectories. Directory sizes will be 0 (unknown).
///
/// Warnings encountered during the listing are included in the returned
/// `FileTree` rather than being silently dropped.
///
/// The `config` parameter controls whether hidden files are shown. Pass
/// `None` to use a default config (hidden files included).
pub fn quick_list(path: &Path, config: Option<&ScanConfig>) -> Result<FileTree, ScanError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    let start = Instant::now();
    let root_path = path.canonicalize().map_err(|e| ScanError::io(path, e))?;

    if !root_path.is_dir() {
        return Err(ScanError::NotADirectory {
            path: root_path.clone(),
        });
    }

    // Build an owned config for the case where none was supplied.
    let owned_config;
    let cfg: &ScanConfig = match config {
        Some(c) => c,
        None => {
            owned_config = ScanConfig::new(&root_path);
            &owned_config
        }
    };

    let node_id_counter = AtomicU64::new(0);
    let mut stats = TreeStats::new();
    let mut warnings: Vec<ScanWarning> = Vec::new();

    // Get root directory metadata
    let root_metadata = std::fs::metadata(&root_path).map_err(|e| ScanError::io(&root_path, e))?;
    let root_timestamps = Timestamps::new(
        root_metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        root_metadata.accessed().ok(),
        root_metadata.created().ok(),
    );

    let root_name = root_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root_path.to_string_lossy().to_string());

    let root_id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));
    let mut root_node = FileNode::new_directory(root_id, root_name, root_timestamps);

    // Read immediate children
    let read_dir = match std::fs::read_dir(&root_path) {
        Ok(rd) => rd,
        Err(e) => return Err(ScanError::io(&root_path, e)),
    };

    let mut total_size: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;

    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warnings.push(ScanWarning::new(
                    root_path.clone(),
                    WarningKind::ReadError,
                    e.to_string(),
                ));
                continue;
            }
        };

        let entry_path = entry.path();
        let entry_name = entry.file_name().to_string_lossy().to_string();

        // Respect include_hidden from config.
        if !cfg.include_hidden && entry_name.starts_with('.') {
            continue;
        }

        // Respect ignore patterns from config.
        if cfg.should_ignore(&entry_name) {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                warnings.push(ScanWarning::new(
                    entry_path,
                    WarningKind::MetadataError,
                    e.to_string(),
                ));
                continue;
            }
        };

        let timestamps = Timestamps::new(
            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
            metadata.accessed().ok(),
            metadata.created().ok(),
        );

        let child_id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));

        if metadata.is_dir() {
            // Directory - size is unknown (0) until full scan
            let child_node =
                FileNode::new_directory(child_id, CompactString::new(&entry_name), timestamps);
            root_node.children.push(child_node);
            dir_count += 1;
            stats.record_dir(1);
        } else if metadata.is_file() {
            let size = if cfg.apparent_size {
                metadata.len()
            } else {
                disk_size(&metadata)
            };
            let blocks = get_blocks(&metadata);
            let executable = is_executable(&metadata);

            let mut child_node = FileNode::new_file(
                child_id,
                CompactString::new(&entry_name),
                size,
                blocks,
                timestamps,
                executable,
            );

            // Set inode info for potential hardlink detection
            let inode = InodeInfo::new(get_ino(&metadata), get_dev(&metadata));
            child_node.inode = Some(inode);

            total_size += size;
            file_count += 1;
            root_node.children.push(child_node);
            stats.record_file(&entry_path, size, timestamps.modified, 1);
        } else if metadata.file_type().is_symlink() {
            // read_link + exists() in a single pass to avoid the double-syscall.
            let (target, broken) = match std::fs::read_link(&entry_path) {
                Ok(t) => {
                    let broken = !entry_path.exists();
                    (CompactString::new(t.to_string_lossy()), broken)
                }
                Err(_) => (CompactString::default(), true),
            };

            if broken {
                warnings.push(ScanWarning::broken_symlink(&entry_path, target.as_str()));
            }

            let child_node = FileNode {
                id: child_id,
                name: CompactString::new(&entry_name),
                kind: NodeKind::Symlink { target, broken },
                size: 0,
                blocks: 0,
                timestamps,
                inode: None,
                content_hash: None,
                git_status: None,
                children: Vec::new(),
            };
            root_node.children.push(child_node);
            stats.record_symlink();
        }
    }

    // Update root node with aggregated values
    root_node.size = total_size;
    root_node.kind = NodeKind::Directory {
        file_count,
        dir_count,
    };

    // Sort children by name for initial display (scan will re-sort by size later)
    root_node.children.sort_by(|a, b| a.name.cmp(&b.name));

    stats.record_dir(0);

    let scan_config = cfg.clone();
    let scan_duration = start.elapsed();

    Ok(FileTree::new(
        root_node,
        root_path,
        scan_config,
        stats,
        scan_duration,
        warnings,
    ))
}

/// Temporary struct for collecting entry information.
struct EntryInfo {
    name: CompactString,
    path: PathBuf,
    size: u64,
    blocks: u64,
    is_dir: bool,
    is_symlink: bool,
    /// Pre-resolved symlink target (avoids re-reading in build_node).
    symlink_target: Option<CompactString>,
    /// Pre-computed broken status (avoids re-calling path.exists() in build_node).
    symlink_broken: bool,
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

#[cfg(windows)]
fn get_dev(_metadata: &std::fs::Metadata) -> u64 {
    0 // Windows doesn't expose a simple numeric device ID via MetadataExt
}

#[cfg(not(any(unix, windows)))]
fn get_dev(_metadata: &std::fs::Metadata) -> u64 {
    0
}

/// Get the inode number from metadata.
#[cfg(unix)]
fn get_ino(metadata: &std::fs::Metadata) -> u64 {
    metadata.ino()
}

#[cfg(windows)]
fn get_ino(_metadata: &std::fs::Metadata) -> u64 {
    // file_index() requires unstable `windows_by_handle` feature.
    // Hardlink dedup is not supported on Windows; return 0 to treat every
    // file as unique.
    0
}

#[cfg(not(any(unix, windows)))]
fn get_ino(_metadata: &std::fs::Metadata) -> u64 {
    0
}

/// Get the number of hard links from metadata.
#[cfg(unix)]
fn get_nlink(metadata: &std::fs::Metadata) -> u64 {
    metadata.nlink()
}

#[cfg(windows)]
fn get_nlink(_metadata: &std::fs::Metadata) -> u64 {
    // number_of_links() requires unstable `windows_by_handle` feature.
    // Return 1 to skip hardlink dedup on Windows.
    1
}

#[cfg(not(any(unix, windows)))]
fn get_nlink(_metadata: &std::fs::Metadata) -> u64 {
    1 // Assume single link on other platforms
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

/// Compute the physical (disk) size of a file.
///
/// On Unix this is `blocks * 512` (the kernel-reported allocation unit).
/// On other platforms we fall back to the apparent size since there is no
/// portable way to query the on-disk allocation.
#[cfg(unix)]
fn disk_size(metadata: &std::fs::Metadata) -> u64 {
    get_blocks(metadata) * 512
}

#[cfg(not(unix))]
fn disk_size(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
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
        assert!(!tree.root.children.iter().any(|c| c.name.as_str() == "dir2"));
    }

    #[test]
    fn test_quick_list_respects_hidden() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::write(root.join(".hidden"), "secret").unwrap();
        fs::write(root.join("visible"), "public").unwrap();

        // Default config hides hidden files.
        let config = ScanConfig::builder()
            .root(root)
            .include_hidden(false)
            .build()
            .unwrap();

        let tree = quick_list(root, Some(&config)).unwrap();
        let names: Vec<_> = tree.root.children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"visible"));
        assert!(!names.contains(&".hidden"));
    }

    #[test]
    fn test_quick_list_includes_hidden_when_configured() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::write(root.join(".hidden"), "secret").unwrap();
        fs::write(root.join("visible"), "public").unwrap();

        let config = ScanConfig::builder()
            .root(root)
            .include_hidden(true)
            .build()
            .unwrap();

        let tree = quick_list(root, Some(&config)).unwrap();
        let names: Vec<_> = tree.root.children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"visible"));
        assert!(names.contains(&".hidden"));
    }
}
