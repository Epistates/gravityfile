//! JWalk-based parallel directory scanner.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use compact_str::CompactString;
use jwalk::{DirEntry, Parallelism, WalkDirGeneric};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use gravityfile_core::{
    ContentHash, FileNode, FileTree, InodeInfo, NodeId, NodeKind, ScanConfig, ScanError,
    ScanWarning, Timestamps, TreeStats, WarningKind,
};

use crate::inode::InodeTracker;
use crate::progress::ScanProgress;

/// Charge yielded descendants before metadata, hashing, or retention.
struct EntryBudget {
    limit: Option<u64>,
    consumed: u64,
}

impl EntryBudget {
    fn new(limit: Option<u64>) -> Self {
        Self { limit, consumed: 0 }
    }

    fn consume(&mut self) -> Result<(), ScanError> {
        if let Some(limit) = self.limit
            && self.consumed >= limit
        {
            return Err(ScanError::EntryLimitExceeded { limit });
        }
        self.consumed = self.consumed.saturating_add(1);
        Ok(())
    }
}

/// Retain bounded warning detail without misreporting scan completeness.
#[derive(Default)]
struct WarningAccumulator {
    details: Vec<ScanWarning>,
    count: u64,
    omitted: u64,
}

impl WarningAccumulator {
    fn push(&mut self, warning: ScanWarning) {
        let represented = warning.represented_count();
        self.count = self.count.saturating_add(represented);
        let bytes = warning
            .path
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .saturating_add(warning.message.len());
        if self.details.len() < 999
            && bytes <= 8 * 1024
            && !matches!(warning.kind, WarningKind::Omitted { .. })
        {
            self.details.push(warning);
        } else {
            self.omitted = self.omitted.saturating_add(represented);
        }
    }

    fn into_vec(mut self) -> Vec<ScanWarning> {
        if self.omitted > 0 {
            self.details.push(ScanWarning::new(PathBuf::new(),
                WarningKind::Omitted { count: self.omitted },
                format!("{} scan warnings omitted from retained detail (999-record / 8 KiB payload limits)", self.omitted)));
        }
        self.details
    }
}

/// Append enrichment diagnostics without bypassing scanner warning limits.
pub(crate) fn append_scan_warning(tree: &mut FileTree, warning: ScanWarning) {
    let mut warnings = WarningAccumulator::default();
    for retained in std::mem::take(&mut tree.warnings) {
        warnings.push(retained);
    }
    warnings.push(warning);
    tree.warnings = warnings.into_vec();
}

/// High-performance scanner using jwalk for parallel traversal.
pub struct JwalkScanner {
    progress_tx: broadcast::Sender<ScanProgress>,
}

struct EntryCollection<'a> {
    inode_tracker: &'a mut InodeTracker,
    stats: &'a mut TreeStats,
    warnings: &'a mut WarningAccumulator,
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
    pub fn scan(
        &self,
        config: &ScanConfig,
        token: CancellationToken,
    ) -> Result<FileTree, ScanError> {
        if token.is_cancelled() {
            return Err(ScanError::Interrupted);
        }
        config
            .validate()
            .map_err(|message| ScanError::InvalidConfig { message })?;
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
        let mut inode_tracker = if config.follow_symlinks {
            InodeTracker::with_alias_tracking()
        } else {
            InodeTracker::new()
        };
        let node_id_counter = AtomicU64::new(0);
        let mut stats = TreeStats::new();
        let mut warnings = WarningAccumulator::default();

        // Collect all entries first
        let entries = {
            let mut collection = EntryCollection {
                inode_tracker: &mut inode_tracker,
                stats: &mut stats,
                warnings: &mut warnings,
            };
            self.collect_entries(
                config,
                &root_path,
                root_device,
                token.clone(),
                start,
                &mut collection,
            )?
        };

        // Build tree from collected entries
        let root_node = self.build_tree(&root_path, entries, &node_id_counter, &|| {
            token.is_cancelled()
        })?;
        if token.is_cancelled() {
            return Err(ScanError::Interrupted);
        }

        let scan_duration = start.elapsed();

        let mut tree = FileTree::new(
            root_node,
            root_path,
            config.clone(),
            stats,
            scan_duration,
            warnings.into_vec(),
        );

        #[cfg(feature = "git")]
        crate::git::apply_git_status_cancellable(&mut tree, &token);

        if token.is_cancelled() {
            return Err(ScanError::Interrupted);
        }
        // Git enrichment is part of completing the scan, including error
        // reporting. Do not publish pre-enrichment timing as completion time.
        tree.scan_duration = start.elapsed();
        tree.scanned_at = std::time::SystemTime::now();
        Ok(tree)
    }

    /// Collect all entries using jwalk.
    fn collect_entries(
        &self,
        config: &ScanConfig,
        root_path: &Path,
        root_device: u64,
        token: CancellationToken,
        start: Instant,
        collection: &mut EntryCollection<'_>,
    ) -> Result<HashMap<PathBuf, Vec<EntryInfo>>, ScanError> {
        // Use global rayon pool to avoid lingering threads from new pools.
        let parallelism = match config.threads {
            0 => Parallelism::RayonDefaultPool {
                busy_timeout: std::time::Duration::from_millis(100),
            },
            n => Parallelism::RayonNewPool(n), // Explicit thread count still gets its own pool
        };

        // Capture config fields needed in the closure.
        let cross_filesystems = config.cross_filesystems;
        let include_hidden = config.include_hidden;
        let token_clone = token.clone();
        let filter_root = root_path.to_path_buf();

        // Re-use the GlobSet already compiled by ScanConfig when available,
        // compiling on-the-fly otherwise (e.g. config built via the
        // derive_builder path without calling compile_patterns).
        let ignore_globset: Option<Arc<globset::GlobSet>> = config
            .effective_ignore_globset()
            .map_err(|message| ScanError::InvalidConfig { message })?
            .map(Arc::new);

        let walker = WalkDirGeneric::<((), ())>::new(root_path)
            .parallelism(parallelism)
            .skip_hidden(!include_hidden)
            .follow_links(config.follow_symlinks)
            .min_depth(0)
            .max_depth(config.max_depth.map(|d| d as usize).unwrap_or(usize::MAX))
            .process_read_dir(move |_depth, _dir_path, _state, children| {
                // Check for cancellation
                if token_clone.is_cancelled() {
                    children.clear();
                    return;
                }

                // Prune and filter early — before jwalk recurses.
                children.retain_mut(|entry_result| {
                    let entry = match entry_result {
                        Ok(e) => e,
                        Err(_) => return true, // keep errors so they surface as warnings
                    };

                    let name = entry.file_name.to_string_lossy();
                    let entry_path = entry.path();

                    // Apply ignore-pattern filter.
                    if let Some(ref gs) = ignore_globset
                        && (gs.is_match(name.as_ref())
                            || entry_path
                                .strip_prefix(&filter_root)
                                .is_ok_and(|relative| gs.is_match(relative)))
                    {
                        return false;
                    }

                    // Prune cross-filesystem subtrees for directories.
                    if !cross_filesystems
                        && entry.file_type.is_dir()
                        && let Ok(meta) = entry.metadata()
                        && get_dev(&meta) != root_device
                    {
                        // Setting read_children to None stops jwalk
                        // from descending into this directory.
                        entry.read_children = None;
                        return false; // drop the dir entry itself too
                    }

                    true
                });
            });

        // Map from parent path to children
        let mut entries_by_parent: HashMap<PathBuf, Vec<EntryInfo>> = HashMap::new();
        let mut progress_counter: u64 = 0;
        let mut entry_budget = EntryBudget::new(config.max_entries);

        for entry_result in walker {
            // Check for cancellation
            if token.is_cancelled() {
                // Use the dedicated variant so callers can distinguish a user
                // cancellation from a genuine scan failure.
                return Err(ScanError::Interrupted);
            }

            let entry: DirEntry<((), ())> = match entry_result {
                Ok(e) => e,
                Err(err) => {
                    let path = err.path().map(|p| p.to_path_buf()).unwrap_or_default();
                    collection.warnings.push(ScanWarning::new(
                        path,
                        WarningKind::ReadError,
                        err.to_string(),
                    ));
                    continue;
                }
            };

            if entry.depth() > 0 {
                entry_budget.consume()?;
            }
            let path = entry.path();
            // Use CompactString directly to avoid an extra heap allocation.
            let file_name = CompactString::new(entry.file_name().to_string_lossy());

            // Get metadata
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(err) => {
                    collection.warnings.push(ScanWarning::new(
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
                collection.stats.record_dir(depth);

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
                        timestamps: timestamps_from(&metadata),
                        inode: inode_info(&metadata),
                        content_hash: None,
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
                let inode = inode_info(&metadata);

                let size = if config.apparent_size {
                    metadata.len()
                } else {
                    // Only count size for first hardlink encounter.
                    if inode.is_some_and(|info| !collection.inode_tracker.track(info, nlink)) {
                        0 // Already counted this inode
                    } else {
                        // Use disk blocks for physical size when apparent_size == false.
                        disk_size(&metadata)
                    }
                };

                let blocks = get_blocks(&metadata);
                let content_hash =
                    if config.compute_hashes && metadata.len() >= config.min_hash_size {
                        match compute_content_hash(&path, &metadata, &token) {
                            Ok(hash) => Some(hash),
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                return Err(ScanError::Interrupted);
                            }
                            Err(error) => {
                                collection
                                    .warnings
                                    .push(ScanWarning::read_error(&path, &error));
                                None
                            }
                        }
                    } else {
                        None
                    };

                collection
                    .stats
                    .record_file(&path, size, metadata.modified().ok(), depth);

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
                        timestamps: timestamps_from(&metadata),
                        inode,
                        content_hash,
                    };

                    entries_by_parent
                        .entry(parent.to_path_buf())
                        .or_default()
                        .push(entry_info);
                }
            } else if file_type.is_symlink() {
                // Filter symlinks on different filesystems at depth 1.
                if !cross_filesystems && get_dev(&metadata) != root_device {
                    continue;
                }

                collection.stats.record_symlink();

                if let Some(parent) = path.parent() {
                    // Read symlink target once; re-use for both the warning and
                    // the EntryInfo so we never call read_link twice.
                    let (symlink_target, symlink_broken) = resolve_symlink(&path);

                    if symlink_broken {
                        collection
                            .warnings
                            .push(ScanWarning::broken_symlink(&path, symlink_target.as_str()));
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
                        timestamps: timestamps_from(&metadata),
                        inode: None,
                        content_hash: None,
                    };

                    entries_by_parent
                        .entry(parent.to_path_buf())
                        .or_default()
                        .push(entry_info);
                }
            }

            // Update progress periodically across all accepted entries, not only files.
            progress_counter += 1;
            if progress_counter.is_multiple_of(1000) {
                let _ = self.progress_tx.send(ScanProgress {
                    files_scanned: collection.stats.total_files,
                    dirs_scanned: collection.stats.total_dirs,
                    bytes_scanned: collection.stats.total_size,
                    current_path: path.clone(),
                    errors_count: collection.warnings.count,
                    elapsed: start.elapsed(),
                });
            }
        }

        // Cancellation may clear the walker's final batch without yielding
        // another entry, so the loop's per-entry check alone is insufficient.
        if token.is_cancelled() {
            return Err(ScanError::Interrupted);
        }
        Ok(entries_by_parent)
    }

    /// Build tree structure from collected entries.
    fn build_tree(
        &self,
        root_path: &Path,
        mut entries_by_parent: HashMap<PathBuf, Vec<EntryInfo>>,
        node_id_counter: &AtomicU64,
        cancelled: &impl Fn() -> bool,
    ) -> Result<FileNode, ScanError> {
        self.build_node(
            root_path,
            None,
            &mut entries_by_parent,
            node_id_counter,
            cancelled,
        )
    }

    /// Recursively build a node and its children.
    fn build_node(
        &self,
        path: &Path,
        timestamps: Option<Timestamps>,
        entries_by_parent: &mut HashMap<PathBuf, Vec<EntryInfo>>,
        node_id_counter: &AtomicU64,
        cancelled: &impl Fn() -> bool,
    ) -> Result<FileNode, ScanError> {
        if cancelled() {
            return Err(ScanError::Interrupted);
        }
        let id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        // Timestamps were already captured during the walk; only the scan root
        // (which has no EntryInfo) needs a stat here.
        let timestamps = timestamps.unwrap_or_else(|| {
            std::fs::metadata(path)
                .map(|m| timestamps_from(&m))
                .unwrap_or_else(|_| Timestamps::with_modified(std::time::UNIX_EPOCH))
        });

        let mut node = FileNode::new_directory(id, name, timestamps);
        node.set_file_name(path.file_name().unwrap_or(path.as_os_str()));

        // Get children for this path
        let children_entries = entries_by_parent.remove(path).unwrap_or_default();

        let mut total_size: u64 = 0;
        let mut file_count: u64 = 0;
        let mut dir_count: u64 = 0;

        for entry in children_entries {
            if cancelled() {
                return Err(ScanError::Interrupted);
            }
            if entry.is_dir {
                // Recursively build directory
                let child_node = self.build_node(
                    &entry.path,
                    Some(entry.timestamps),
                    entries_by_parent,
                    node_id_counter,
                    cancelled,
                )?;
                total_size = total_size.saturating_add(child_node.size);
                file_count = file_count.saturating_add(child_node.file_count());
                dir_count = dir_count.saturating_add(child_node.dir_count().saturating_add(1));
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
                    native_name: entry
                        .path
                        .file_name()
                        .filter(|n| n.to_str().is_none())
                        .map(|n| Box::new(n.to_os_string())),
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
                child_node.set_file_name(entry.path.file_name().unwrap());
                child_node.inode = entry.inode;
                child_node.content_hash = entry.content_hash;

                total_size = total_size.saturating_add(entry.size);
                file_count = file_count.saturating_add(1);
                node.children.push(child_node);
            }
        }

        // Update node with aggregated values
        node.size = total_size;
        node.kind = NodeKind::Directory {
            file_count,
            dir_count,
        };

        // Sort children by size (descending) with a name tiebreak so equal-size
        // siblings have a deterministic order regardless of the parallel scan's
        // collection order. Descendants were sorted when built; recursively
        // sorting them here repeats work at every ancestor and delays cancellation.
        if cancelled() {
            return Err(ScanError::Interrupted);
        }
        node.children
            .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));
        if cancelled() {
            return Err(ScanError::Interrupted);
        }

        Ok(node)
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

    cfg.validate()
        .map_err(|message| ScanError::InvalidConfig { message })?;
    let ignore = cfg
        .effective_ignore_globset()
        .map_err(|message| ScanError::InvalidConfig { message })?;

    let node_id_counter = AtomicU64::new(0);
    let mut stats = TreeStats::new();
    let mut warnings = WarningAccumulator::default();

    // Get root directory metadata
    let root_metadata = std::fs::metadata(&root_path).map_err(|e| ScanError::io(&root_path, e))?;
    let root_timestamps = timestamps_from(&root_metadata);

    let root_name = root_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root_path.to_string_lossy().to_string());

    let root_id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));
    let mut root_node = FileNode::new_directory(root_id, root_name, root_timestamps);
    root_node.set_file_name(root_path.file_name().unwrap_or(root_path.as_os_str()));

    // Read immediate children
    let read_dir = match std::fs::read_dir(&root_path) {
        Ok(rd) => rd,
        Err(e) => return Err(ScanError::io(&root_path, e)),
    };

    let mut total_size: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;
    let mut entry_budget = EntryBudget::new(cfg.max_entries);
    let mut inode_tracker = if cfg.follow_symlinks {
        InodeTracker::with_alias_tracking()
    } else {
        InodeTracker::new()
    };

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
        if ignore.as_ref().is_some_and(|set| set.is_match(&entry_name))
            || entry_path
                .strip_prefix(&root_path)
                .is_ok_and(|relative| ignore.as_ref().is_some_and(|set| set.is_match(relative)))
        {
            continue;
        }

        entry_budget.consume()?;
        let metadata_result = if cfg.follow_symlinks {
            std::fs::metadata(&entry_path)
        } else {
            entry.metadata()
        };
        let metadata = match metadata_result {
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

        if !cfg.cross_filesystems && get_dev(&metadata) != get_dev(&root_metadata) {
            continue;
        }
        let timestamps = timestamps_from(&metadata);

        let child_id = NodeId::new(node_id_counter.fetch_add(1, Ordering::Relaxed));

        if metadata.is_dir() {
            // Directory - size is unknown (0) until full scan
            let mut child_node =
                FileNode::new_directory(child_id, CompactString::new(&entry_name), timestamps);
            child_node.set_file_name(&entry.file_name());
            root_node.children.push(child_node);
            dir_count += 1;
            stats.record_dir(1);
        } else if metadata.is_file() {
            let inode = inode_info(&metadata);
            let size = if cfg.apparent_size {
                metadata.len()
            } else if inode.is_some_and(|info| !inode_tracker.track(info, get_nlink(&metadata))) {
                0
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
            child_node.set_file_name(&entry.file_name());
            child_node.inode = inode;

            total_size = total_size.saturating_add(size);
            file_count = file_count.saturating_add(1);
            root_node.children.push(child_node);
            stats.record_file(&entry_path, size, metadata.modified().ok(), 1);
        } else if metadata.file_type().is_symlink() {
            let (target, broken) = resolve_symlink(&entry_path);

            if broken {
                warnings.push(ScanWarning::broken_symlink(&entry_path, target.as_str()));
            }

            let child_node = FileNode {
                id: child_id,
                name: CompactString::new(&entry_name),
                native_name: entry
                    .file_name()
                    .to_str()
                    .is_none()
                    .then(|| Box::new(entry.file_name())),
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
    root_node.children.sort_by(|a, b| a.compare_name(b));

    stats.record_dir(0);

    // quick_list observes only immediate children, even if supplied a recursive
    // scan configuration. Preserve the actual scope for downstream consumers.
    let mut scan_config = cfg.clone();
    scan_config.root = root_path.clone();
    scan_config.max_depth = Some(1);
    let scan_duration = start.elapsed();

    Ok(FileTree::new(
        root_node,
        root_path,
        scan_config,
        stats,
        scan_duration,
        warnings.into_vec(),
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
    content_hash: Option<ContentHash>,
}

fn compute_content_hash(
    path: &Path,
    observed: &std::fs::Metadata,
    token: &CancellationToken,
) -> std::io::Result<ContentHash> {
    if token.is_cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "scan cancelled before opening hash input",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "scan hash input must be a regular file",
        ));
    }
    if observed.len() != before.len()
        || observed.modified().ok() != before.modified().ok()
        || inode_info(observed) != inode_info(&before)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "scan hash input changed since traversal",
        ));
    }
    let hash = hash_exact_length(&mut file, before.len(), token)?;
    let after = file.metadata()?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "scan hash input changed while reading",
        ));
    }
    Ok(hash)
}

/// Read only the observed length plus one byte to detect growth, with no
/// buffered read-ahead beyond that boundary.
fn hash_exact_length(
    mut reader: impl Read,
    mut remaining: u64,
    token: &CancellationToken,
) -> std::io::Result<ContentHash> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        if token.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "scan cancelled while hashing",
            ));
        }
        let requested = if remaining == 0 {
            1
        } else {
            remaining.min(buffer.len() as u64) as usize
        };
        let read = reader.read(&mut buffer[..requested])?;
        if read == 0 {
            if remaining != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "scan hash input shrank while reading",
                ));
            }
            break;
        }
        if remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "scan hash input grew while reading",
            ));
        }
        remaining -= read as u64;
        hasher.update(&buffer[..read]);
    }

    Ok(ContentHash::new(*hasher.finalize().as_bytes()))
}

/// Build [`Timestamps`] from metadata, falling back to the Unix epoch only for
/// the *node's* display timestamp when the real mtime is unreadable.
fn timestamps_from(metadata: &std::fs::Metadata) -> Timestamps {
    Timestamps::new(
        metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        metadata.accessed().ok(),
        metadata.created().ok(),
    )
}

/// Resolve a symlink's target and whether it is broken.
///
/// "Broken" means the target genuinely does not exist. A failure to stat for
/// any other reason (e.g. permission denied on a parent component) is *not*
/// treated as broken, so valid links into restricted directories aren't
/// mislabelled. The target string is empty if the link itself can't be read.
fn resolve_symlink(path: &Path) -> (CompactString, bool) {
    match std::fs::read_link(path) {
        Ok(target) => {
            let broken = match std::fs::metadata(path) {
                Ok(_) => false,
                Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            };
            (CompactString::new(target.to_string_lossy()), broken)
        }
        Err(_) => (CompactString::default(), true),
    }
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

/// Get stable file identity where the standard library exposes it.
#[cfg(unix)]
fn inode_info(metadata: &std::fs::Metadata) -> Option<InodeInfo> {
    Some(InodeInfo::new(metadata.ino(), metadata.dev()))
}

#[cfg(not(unix))]
fn inode_info(_metadata: &std::fs::Metadata) -> Option<InodeInfo> {
    // A fabricated (0, 0) identity would make duplicate analysis incorrectly
    // treat every file as a hardlink of every other file.
    None
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

    #[test]
    fn hash_length_boundary_rejects_growth_and_truncation() {
        use std::io::Cursor;
        let token = CancellationToken::new();
        for bytes in [b"".as_slice(), b"fixture".as_slice()] {
            assert_eq!(
                hash_exact_length(Cursor::new(bytes), bytes.len() as u64, &token).unwrap(),
                ContentHash::new(*blake3::hash(bytes).as_bytes())
            );
        }
        let mut growing = Cursor::new(vec![42; 100_000]);
        let error = hash_exact_length(&mut growing, 17, &token).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(growing.position(), 18);
        let error = hash_exact_length(Cursor::new(b"short"), 6, &token).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn bounded_hash_handles_short_reads_and_cancellation_between_reads() {
        struct ShortReader {
            reads: usize,
            token: CancellationToken,
            cancel: bool,
        }
        impl Read for ShortReader {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                if self.cancel {
                    self.token.cancel();
                }
                if self.reads <= 3 {
                    bytes[0] = b'x';
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
        }
        for cancel in [false, true] {
            let token = CancellationToken::new();
            let mut reader = ShortReader {
                reads: 0,
                token: token.clone(),
                cancel,
            };
            let result = hash_exact_length(&mut reader, 3, &token);
            if cancel {
                assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
                assert_eq!(reader.reads, 1);
            } else {
                assert_eq!(
                    result.unwrap(),
                    ContentHash::new(*blake3::hash(b"xxx").as_bytes())
                );
                assert_eq!(reader.reads, 4);
            }
        }
    }

    #[test]
    fn hash_cancellation_precedes_opening_the_path() {
        let token = CancellationToken::new();
        token.cancel();
        let temp = TempDir::new().unwrap();
        let error = compute_content_hash(
            &temp.path().join("missing"),
            &std::fs::metadata(temp.path()).unwrap(),
            &token,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }

    #[test]
    fn hash_input_rejects_directories() {
        let temp = TempDir::new().unwrap();
        assert!(
            compute_content_hash(
                temp.path(),
                &std::fs::metadata(temp.path()).unwrap(),
                &CancellationToken::new()
            )
            .is_err()
        );
    }

    #[test]
    fn hash_rejects_length_changes_since_traversal() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("file");
        std::fs::write(&path, b"original").unwrap();
        let observed = std::fs::metadata(&path).unwrap();
        std::fs::write(&path, b"longer replacement contents").unwrap();
        let error = compute_content_hash(&path, &observed, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("since traversal"));
    }

    #[cfg(unix)]
    #[test]
    fn hash_rejects_replacement_even_with_matching_length_and_mtime() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("file");
        std::fs::write(&path, b"original").unwrap();
        let observed = std::fs::metadata(&path).unwrap();
        std::fs::rename(&path, temp.path().join("saved")).unwrap();
        std::fs::write(&path, b"replaced").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(observed.modified().unwrap()))
            .unwrap();
        let current = std::fs::metadata(&path).unwrap();
        assert_eq!(observed.len(), current.len());
        assert_eq!(observed.modified().unwrap(), current.modified().unwrap());
        assert_ne!(inode_info(&observed), inode_info(&current));
        let error = compute_content_hash(&path, &observed, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("since traversal"));
    }

    #[cfg(unix)]
    #[test]
    fn hash_input_rejects_fifo_without_a_writer_and_preserves_regular_symlinks() {
        use std::os::unix::ffi::OsStrExt;
        let temp = TempDir::new().unwrap();
        let fifo = temp.path().join("fifo");
        let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: native is a NUL-terminated path valid for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        let error = compute_content_hash(
            &fifo,
            &std::fs::metadata(&fifo).unwrap(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let file = temp.path().join("file");
        std::fs::write(&file, b"hash fixture").unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(
            compute_content_hash(
                &link,
                &std::fs::metadata(&file).unwrap(),
                &CancellationToken::new()
            )
            .unwrap(),
            ContentHash::new(*blake3::hash(b"hash fixture").as_bytes())
        );
    }

    #[test]
    fn tree_assembly_cancellation_stops_between_children() {
        let root = PathBuf::from("root");
        let entries = (0..1000)
            .map(|index| EntryInfo {
                name: CompactString::new(format!("file-{index}")),
                path: root.join(format!("file-{index}")),
                size: 1,
                blocks: 0,
                is_dir: false,
                is_symlink: false,
                symlink_target: None,
                symlink_broken: false,
                executable: false,
                timestamps: Timestamps::with_modified(std::time::UNIX_EPOCH),
                inode: None,
                content_hash: None,
            })
            .collect();
        let mut by_parent = HashMap::from([(root.clone(), entries)]);
        let next_id = AtomicU64::new(0);
        let checks = std::cell::Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() > 5
        };
        let result = JwalkScanner::new().build_node(
            &root,
            Some(Timestamps::with_modified(std::time::UNIX_EPOCH)),
            &mut by_parent,
            &next_id,
            &cancelled,
        );
        assert!(matches!(result, Err(ScanError::Interrupted)));
        assert_eq!(checks.get(), 6);
        assert_eq!(next_id.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn cancelled_tree_assembly_does_not_allocate_a_root_node() {
        let next_id = AtomicU64::new(0);
        let result =
            JwalkScanner::new().build_tree(Path::new("unused"), HashMap::new(), &next_id, &|| true);
        assert!(matches!(result, Err(ScanError::Interrupted)));
        assert_eq!(next_id.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn warnings_bound_detail_and_preserve_observed_counts() {
        let mut warnings = WarningAccumulator::default();
        warnings.push(ScanWarning::new(
            "p",
            WarningKind::ReadError,
            "x".repeat(8191),
        ));
        warnings.push(ScanWarning::new(
            "p",
            WarningKind::ReadError,
            "x".repeat(8192),
        ));
        for index in 0..100_000 {
            warnings.push(ScanWarning::read_error(
                format!("item-{index}"),
                &std::io::Error::other("failed"),
            ));
            assert!(warnings.details.len() <= 999);
        }
        assert_eq!(warnings.count, 100_002);
        let retained = warnings.into_vec();
        assert_eq!(retained.len(), 1000);
        assert_eq!(retained[0].message.len(), 8191);
        assert_eq!(
            retained
                .iter()
                .map(ScanWarning::represented_count)
                .sum::<u64>(),
            100_002
        );
        let omitted = retained.last().unwrap();
        assert!(omitted.path.as_os_str().is_empty());
        assert_eq!(omitted.kind, WarningKind::Omitted { count: 99_003 });
    }

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
        let tree = scanner.scan(&config, CancellationToken::new()).unwrap();

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
        let tree = scanner.scan(&config, CancellationToken::new()).unwrap();

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
        let tree = scanner.scan(&config, CancellationToken::new()).unwrap();

        // dir2 should be ignored
        assert!(!tree.root.children.iter().any(|c| c.name.as_str() == "dir2"));
    }

    #[test]
    fn test_ignore_patterns_match_root_relative_paths() {
        let temp = create_test_tree();
        let config = ScanConfig::builder()
            .root(temp.path())
            .ignore_patterns(vec!["dir1/subdir/**".to_string()])
            .build()
            .unwrap();

        let scanner = JwalkScanner::new();
        let tree = scanner.scan(&config, CancellationToken::new()).unwrap();
        let dir1 = tree
            .root
            .children
            .iter()
            .find(|child| child.name.as_str() == "dir1")
            .unwrap();
        let subdir = dir1
            .children
            .iter()
            .find(|child| child.name.as_str() == "subdir")
            .unwrap();
        assert!(subdir.children.is_empty());
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
