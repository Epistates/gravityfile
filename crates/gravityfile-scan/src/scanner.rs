//! Parallel directory scanner built on jwalk.
//!
//! Metadata is read on jwalk's worker threads (`process_read_dir`), so the
//! single consumer thread makes no per-entry system calls. jwalk yields
//! entries in depth-first pre-order, which lets the consumer build the tree
//! directly with a stack of open directories.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use jwalk::{Parallelism, WalkDirGeneric};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use gravityfile_core::{
    FileNode, FileTree, InodeInfo, NodeKind, ScanConfig, ScanError, ScanWarning, Timestamps,
    TreeStats, WarningKind,
};

use crate::inode::InodeTracker;
use crate::progress::ScanProgress;

/// Entries between progress messages.
const PROGRESS_INTERVAL: u64 = 1024;

/// Charge yielded descendants before retention.
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
            self.details.push(ScanWarning::new(
                PathBuf::new(),
                WarningKind::Omitted {
                    count: self.omitted,
                },
                format!(
                    "{} scan warnings omitted from retained detail (999-record / 8 KiB payload limits)",
                    self.omitted
                ),
            ));
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

/// What a worker thread learned about one entry.
#[derive(Debug, Default)]
enum Probe {
    #[default]
    Unprobed,
    Entry(Entry),
    Failed(std::io::Error),
}

type Client = ((), Probe);

#[derive(Debug)]
struct Entry {
    kind: EntryKind,
    len: u64,
    blocks: u64,
    nlink: u64,
    identity: Option<InodeInfo>,
    modified: Option<SystemTime>,
}

#[derive(Debug)]
enum EntryKind {
    File { executable: bool },
    Dir,
    Symlink { target: Box<str>, broken: bool },
    Other,
}

impl Entry {
    /// `path` is only consulted for symlinks, to read their target.
    fn from_metadata(metadata: &std::fs::Metadata, path: impl FnOnce() -> PathBuf) -> Self {
        let file_type = metadata.file_type();
        let kind = if file_type.is_dir() {
            EntryKind::Dir
        } else if file_type.is_file() {
            EntryKind::File {
                executable: is_executable(metadata),
            }
        } else if file_type.is_symlink() {
            let (target, broken) = resolve_symlink(&path());
            EntryKind::Symlink { target, broken }
        } else {
            EntryKind::Other
        };
        Self {
            kind,
            len: metadata.len(),
            blocks: get_blocks(metadata),
            nlink: get_nlink(metadata),
            identity: inode_info(metadata),
            modified: metadata.modified().ok(),
        }
    }

    /// Display timestamps; an unreadable mtime shows as the Unix epoch.
    fn timestamps(&self) -> Timestamps {
        Timestamps::with_modified(self.modified.unwrap_or(std::time::UNIX_EPOCH))
    }

    /// Node for this entry; directories start empty and are sealed later.
    fn node(&self, name: &std::ffi::OsStr, size: u64) -> FileNode {
        let timestamps = self.timestamps();
        let mut node = match &self.kind {
            EntryKind::Dir => FileNode::new_directory("", timestamps),
            EntryKind::File { executable } => {
                FileNode::new_file("", size, self.blocks, timestamps, *executable)
            }
            EntryKind::Symlink { target, broken } => {
                FileNode::new_symlink("", target.clone(), *broken, timestamps)
            }
            EntryKind::Other => {
                let mut node = FileNode::new_file("", size, self.blocks, timestamps, false);
                node.kind = NodeKind::Other;
                node
            }
        };
        node.set_file_name(name);
        node.size = size;
        node.len = self.len;
        node.blocks = self.blocks;
        node.inode = self.identity.filter(|_| self.nlink > 1 && !self.is_dir());
        node
    }

    fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir)
    }

    /// Size this entry contributes, counting each multiply-linked file (and,
    /// when following symlinks, each aliased object) once.
    fn counted_size(&self, config: &ScanConfig, tracker: &mut InodeTracker) -> u64 {
        if matches!(self.kind, EntryKind::Symlink { .. }) {
            return 0;
        }
        // Same measure as `FileNode::own_size`, so a scan and a remeasure agree.
        let size = if config.apparent_size {
            self.len
        } else {
            self.blocks.saturating_mul(512)
        };
        // Directory link counts reflect subdirectories, not aliases; they can
        // only be reached twice by following symlinks.
        let dedup = !config.count_hard_links && (!self.is_dir() || config.follow_symlinks);
        match self.identity {
            Some(identity) if dedup && !tracker.track(identity, self.nlink) => 0,
            _ => size,
        }
    }
}

/// A directory whose contents are still arriving.
struct Frame {
    /// `None` below the retained depth: counted, but not kept in the tree.
    node: Option<FileNode>,
    size: u64,
    files: u64,
    dirs: u64,
}

impl Frame {
    fn new(node: Option<FileNode>, own_size: u64) -> Self {
        Self {
            node,
            size: own_size,
            files: 0,
            dirs: 0,
        }
    }

    fn add_leaf(&mut self, size: u64, node: Option<FileNode>) {
        self.size = self.size.saturating_add(size);
        self.files += 1;
        if let (Some(parent), Some(node)) = (&mut self.node, node) {
            parent.children.push(node);
        }
    }

    /// Seal the directory: final size, counts and size-ordered children.
    fn finish(self) -> Option<FileNode> {
        let mut node = self.node?;
        node.size = self.size;
        node.kind = NodeKind::Directory {
            file_count: self.files,
            dir_count: self.dirs,
        };
        node.children
            .sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));
        Some(node)
    }
}

/// Close the innermost open directory into its parent.
fn close_frame(stack: &mut Vec<Frame>) {
    let frame = stack.pop().expect("open directory");
    let parent = stack.last_mut().expect("parent directory");
    parent.size = parent.size.saturating_add(frame.size);
    parent.files += frame.files;
    parent.dirs += frame.dirs + 1;
    if let Some(node) = frame.finish()
        && let Some(parent) = &mut parent.node
    {
        parent.children.push(node);
    }
}

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

    /// Scan `config.root` into a tree.
    ///
    /// Sizes cover the whole subtree even when `max_depth` limits which nodes
    /// are retained. Hard links count once unless `count_hard_links` is set,
    /// and directories include their own allocation.
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
        let root_metadata =
            std::fs::metadata(&root_path).map_err(|e| ScanError::io(&root_path, e))?;
        if !root_metadata.is_dir() {
            return Err(ScanError::NotADirectory { path: root_path });
        }

        let mut tracker = if config.follow_symlinks {
            InodeTracker::with_alias_tracking()
        } else {
            InodeTracker::new()
        };
        let mut stats = TreeStats::new();
        let mut warnings = WarningAccumulator::default();
        let mut budget = EntryBudget::new(config.max_entries);
        let retained_depth = config.max_depth.map_or(usize::MAX, |depth| depth as usize);

        let root_entry = Entry::from_metadata(&root_metadata, PathBuf::new);
        let root_size = root_entry.counted_size(config, &mut tracker);
        let root_node = root_entry.node(
            root_path.file_name().unwrap_or(root_path.as_os_str()),
            root_size,
        );
        stats.record_dir(0);
        let mut stack = vec![Frame::new(Some(root_node), root_size)];

        let walker = self.walker(config, &root_path, get_dev(&root_metadata), token.clone())?;
        let mut seen: u64 = 0;
        for item in walker {
            if token.is_cancelled() {
                return Err(ScanError::Interrupted);
            }
            let mut entry = match item {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(walk_warning(&error));
                    continue;
                }
            };
            let depth = entry.depth;
            if depth == 0 {
                continue;
            }
            budget.consume()?;
            while stack.len() > depth {
                close_frame(&mut stack);
            }
            if stack.len() < depth {
                // Below a directory that was not recorded.
                continue;
            }
            let fact = match std::mem::take(&mut entry.client_state) {
                Probe::Entry(fact) => fact,
                Probe::Failed(error) => {
                    warnings.push(metadata_warning(entry.path(), &error));
                    continue;
                }
                Probe::Unprobed => continue,
            };
            let size = fact.counted_size(config, &mut tracker);
            let node = (depth <= retained_depth).then(|| fact.node(&entry.file_name, size));
            match &fact.kind {
                EntryKind::Dir => {
                    stats.record_dir(depth as u32);
                    // jwalk attaches a failed read_dir to the directory entry
                    // rather than yielding it as an error item.
                    if let Some(error) = entry.read_children.as_ref().and_then(|c| c.error()) {
                        warnings.push(walk_warning(error));
                    }
                    stack.push(Frame::new(node, size));
                }
                EntryKind::File { .. } => {
                    stats.record_file_with(size, fact.modified, depth as u32, || entry.path());
                    stack
                        .last_mut()
                        .expect("open directory")
                        .add_leaf(size, node);
                }
                EntryKind::Symlink { target, broken } => {
                    stats.record_symlink();
                    if *broken {
                        warnings.push(ScanWarning::broken_symlink(entry.path(), target));
                    }
                    stack.last_mut().expect("open directory").add_leaf(0, node);
                }
                EntryKind::Other => {
                    stack
                        .last_mut()
                        .expect("open directory")
                        .add_leaf(size, node);
                }
            }
            seen += 1;
            if seen.is_multiple_of(PROGRESS_INTERVAL) {
                let _ = self.progress_tx.send(ScanProgress {
                    files_scanned: stats.total_files,
                    dirs_scanned: stats.total_dirs,
                    bytes_scanned: stats.total_size,
                    current_path: entry.path(),
                    errors_count: warnings.count,
                    elapsed: start.elapsed(),
                });
            }
        }
        // Cancellation may clear the walker's last batch without yielding
        // another entry, so the per-entry check alone is insufficient.
        if token.is_cancelled() {
            return Err(ScanError::Interrupted);
        }
        while stack.len() > 1 {
            close_frame(&mut stack);
        }
        let root = stack
            .pop()
            .and_then(Frame::finish)
            .expect("root is always retained");
        let mut tree = FileTree::new(
            root,
            root_path,
            config.clone(),
            stats,
            start.elapsed(),
            warnings.into_vec(),
        );

        #[cfg(feature = "git")]
        if config.git_status {
            crate::git::apply_git_status_cancellable(&mut tree, &token);
            if token.is_cancelled() {
                return Err(ScanError::Interrupted);
            }
        }
        // Enrichment is part of completing the scan.
        tree.scan_duration = start.elapsed();
        tree.scanned_at = SystemTime::now();
        Ok(tree)
    }

    /// Configure the parallel walk: filtering, filesystem pruning and
    /// metadata probing all run on jwalk's worker threads.
    fn walker(
        &self,
        config: &ScanConfig,
        root_path: &Path,
        root_device: u64,
        token: CancellationToken,
    ) -> Result<WalkDirGeneric<Client>, ScanError> {
        let parallelism = match config.threads {
            0 => Parallelism::RayonExistingPool {
                pool: scan_pool(),
                // The consumer never runs on this pool, so it cannot deadlock.
                busy_timeout: None,
            },
            n => Parallelism::RayonNewPool(n),
        };
        let ignore: Option<Arc<globset::GlobSet>> = config
            .effective_ignore_globset()
            .map_err(|message| ScanError::InvalidConfig { message })?
            .map(Arc::new);
        let cross_filesystems = config.cross_filesystems;
        let filter_root = root_path.to_path_buf();

        Ok(WalkDirGeneric::<Client>::new(root_path)
            .parallelism(parallelism)
            .skip_hidden(!config.include_hidden)
            .follow_links(config.follow_symlinks)
            .min_depth(0)
            .process_read_dir(move |_depth, _dir, _state, children| {
                if token.is_cancelled() {
                    children.clear();
                    return;
                }
                children.retain_mut(|child| {
                    let Ok(entry) = child else {
                        return true; // surfaces as a warning
                    };
                    if let Some(ignore) = &ignore
                        && (ignore.is_match(&entry.file_name)
                            || entry
                                .path()
                                .strip_prefix(&filter_root)
                                .is_ok_and(|relative| ignore.is_match(relative)))
                    {
                        return false;
                    }
                    let metadata = match entry.metadata() {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            let error = error
                                .into_io_error()
                                .unwrap_or_else(|| std::io::Error::other("metadata unavailable"));
                            entry.client_state = Probe::Failed(error);
                            entry.read_children = None;
                            return true;
                        }
                    };
                    if !cross_filesystems && get_dev(&metadata) != root_device {
                        // Another filesystem: neither counted nor descended.
                        entry.read_children = None;
                        return false;
                    }
                    entry.client_state =
                        Probe::Entry(Entry::from_metadata(&metadata, || entry.path()));
                    true
                });
            }))
    }
}

impl Default for JwalkScanner {
    fn default() -> Self {
        Self::new()
    }
}

fn walk_warning(error: &jwalk::Error) -> ScanWarning {
    let path = error.path().map(Path::to_path_buf).unwrap_or_default();
    let kind = match error.io_error().map(std::io::Error::kind) {
        Some(std::io::ErrorKind::PermissionDenied) => WarningKind::PermissionDenied,
        _ => WarningKind::ReadError,
    };
    ScanWarning::new(path, kind, error.to_string())
}

fn metadata_warning(path: PathBuf, error: &std::io::Error) -> ScanWarning {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        WarningKind::PermissionDenied
    } else {
        WarningKind::MetadataError
    };
    ScanWarning::new(path, kind, error.to_string())
}

/// An entry scanned on its own, for grafting into an existing tree.
#[derive(Debug)]
pub struct ScannedEntry {
    pub node: FileNode,
    pub warnings: Vec<ScanWarning>,
}

/// Scan the entry at `path` as `config` measures it: a directory with
/// everything below it, or anything else on its own. `None` if `path` no
/// longer exists. `config.root` is ignored.
///
/// Hard links are counted once within the entry; links shared with the rest
/// of a tree it is grafted into are not known here.
pub fn scan_entry(
    path: &Path,
    config: &ScanConfig,
    token: CancellationToken,
) -> Result<Option<ScannedEntry>, ScanError> {
    let metadata = if config.follow_symlinks {
        std::fs::metadata(path)
    } else {
        std::fs::symlink_metadata(path)
    };
    let metadata = match metadata {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ScanError::io(path, error)),
    };
    let name = path.file_name().unwrap_or(path.as_os_str());
    if metadata.is_dir() {
        let mut subtree = config.clone();
        subtree.root = path.to_path_buf();
        let FileTree {
            mut root, warnings, ..
        } = JwalkScanner::new().scan(&subtree, token)?;
        root.set_file_name(name);
        return Ok(Some(ScannedEntry {
            node: root,
            warnings,
        }));
    }
    let entry = Entry::from_metadata(&metadata, || path.to_path_buf());
    let size = entry.counted_size(config, &mut InodeTracker::new());
    Ok(Some(ScannedEntry {
        node: entry.node(name, size),
        warnings: Vec::new(),
    }))
}

/// Create a quick, non-recursive directory listing for immediate display.
/// This function reads only the immediate children of a directory without
/// recursing into subdirectories. Directory sizes cover only their own
/// allocation until a full scan.
///
/// Warnings encountered during the listing are included in the returned
/// `FileTree` rather than being silently dropped.
///
/// The `config` parameter controls whether hidden files are shown. Pass
/// `None` to use a default config (hidden files included).
pub fn quick_list(path: &Path, config: Option<&ScanConfig>) -> Result<FileTree, ScanError> {
    let start = Instant::now();
    let root_path = path.canonicalize().map_err(|e| ScanError::io(path, e))?;
    let root_metadata = std::fs::metadata(&root_path).map_err(|e| ScanError::io(&root_path, e))?;
    if !root_metadata.is_dir() {
        return Err(ScanError::NotADirectory { path: root_path });
    }

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

    let mut stats = TreeStats::new();
    let mut warnings = WarningAccumulator::default();
    let mut budget = EntryBudget::new(cfg.max_entries);
    let mut tracker = if cfg.follow_symlinks {
        InodeTracker::with_alias_tracking()
    } else {
        InodeTracker::new()
    };
    let root_entry = Entry::from_metadata(&root_metadata, PathBuf::new);
    let root_size = root_entry.counted_size(cfg, &mut tracker);
    let root_node = root_entry.node(
        root_path.file_name().unwrap_or(root_path.as_os_str()),
        root_size,
    );
    let mut frame = Frame::new(Some(root_node), root_size);
    stats.record_dir(0);

    let read_dir = std::fs::read_dir(&root_path).map_err(|e| ScanError::io(&root_path, e))?;
    for item in read_dir {
        let entry = match item {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(ScanWarning::read_error(root_path.clone(), &error));
                continue;
            }
        };
        let entry_path = entry.path();
        let file_name = entry.file_name();
        if !cfg.include_hidden && file_name.as_encoded_bytes().starts_with(b".") {
            continue;
        }
        if let Some(ignore) = &ignore
            && (ignore.is_match(&file_name)
                || entry_path
                    .strip_prefix(&root_path)
                    .is_ok_and(|relative| ignore.is_match(relative)))
        {
            continue;
        }
        budget.consume()?;
        let metadata = if cfg.follow_symlinks {
            std::fs::metadata(&entry_path)
        } else {
            entry.metadata()
        };
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(metadata_warning(entry_path, &error));
                continue;
            }
        };
        if !cfg.cross_filesystems && get_dev(&metadata) != get_dev(&root_metadata) {
            continue;
        }
        let fact = Entry::from_metadata(&metadata, || entry_path.clone());
        let size = fact.counted_size(cfg, &mut tracker);
        let node = fact.node(&file_name, size);
        match &fact.kind {
            EntryKind::Dir => {
                stats.record_dir(1);
                frame.dirs += 1;
                // Contents unknown until a full scan: own allocation only.
                frame.size = frame.size.saturating_add(size);
                if let Some(root) = &mut frame.node {
                    root.children.push(node);
                }
            }
            EntryKind::File { .. } => {
                stats.record_file(&entry_path, size, fact.modified, 1);
                frame.add_leaf(size, Some(node));
            }
            EntryKind::Symlink { target, broken } => {
                stats.record_symlink();
                if *broken {
                    warnings.push(ScanWarning::broken_symlink(&entry_path, target));
                }
                frame.add_leaf(0, Some(node));
            }
            EntryKind::Other => frame.add_leaf(size, Some(node)),
        }
    }

    // Ordered by size like every other tree, so a caller's sort applies the
    // same way to a listing and to a scan.
    let root = frame.finish().expect("root is retained");

    // quick_list observes only immediate children, even if supplied a recursive
    // scan configuration. Preserve the actual scope for downstream consumers.
    let mut scan_config = cfg.clone();
    scan_config.root = root_path.clone();
    scan_config.max_depth = Some(1);
    Ok(FileTree::new(
        root,
        root_path,
        scan_config,
        stats,
        start.elapsed(),
        warnings.into_vec(),
    ))
}

/// Resolve a symlink's target and whether it is broken.
///
/// "Broken" means the target genuinely does not exist. A failure to stat for
/// any other reason (e.g. permission denied on a parent component) is *not*
/// treated as broken, so valid links into restricted directories aren't
/// mislabelled. The target string is empty if the link itself can't be read.
fn resolve_symlink(path: &Path) -> (Box<str>, bool) {
    match std::fs::read_link(path) {
        Ok(target) => {
            let broken = matches!(
                std::fs::metadata(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            );
            (target.to_string_lossy().into(), broken)
        }
        Err(_) => (Box::from(""), true),
    }
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Thread pool shared by every scan. Kept separate from rayon's global pool so
/// CPU-bound work there (hashing, sorting) can never starve a walk, which
/// jwalk would otherwise report as an empty result.
fn scan_pool() -> Arc<jwalk::rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Arc<jwalk::rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        Arc::new(
            jwalk::rayon::ThreadPoolBuilder::new()
                .thread_name(|index| format!("gravityfile-scan-{index}"))
                .build()
                .expect("scan thread pool"),
        )
    })
    .clone()
}

// Cross-platform metadata helpers

#[cfg(unix)]
fn get_dev(metadata: &std::fs::Metadata) -> u64 {
    metadata.dev()
}

#[cfg(not(unix))]
fn get_dev(_metadata: &std::fs::Metadata) -> u64 {
    // No stable numeric device ID without unstable `windows_by_handle`.
    0
}

/// Stable file identity where the standard library exposes it.
#[cfg(unix)]
fn inode_info(metadata: &std::fs::Metadata) -> Option<InodeInfo> {
    Some(InodeInfo::new(metadata.ino(), metadata.dev()))
}

#[cfg(not(unix))]
fn inode_info(_metadata: &std::fs::Metadata) -> Option<InodeInfo> {
    // A fabricated identity would make every file look like a hard link of
    // every other one.
    None
}

#[cfg(unix)]
fn get_nlink(metadata: &std::fs::Metadata) -> u64 {
    metadata.nlink()
}

#[cfg(not(unix))]
fn get_nlink(_metadata: &std::fs::Metadata) -> u64 {
    // number_of_links() requires unstable `windows_by_handle`.
    1
}

/// 512-byte blocks allocated to the entry.
#[cfg(unix)]
fn get_blocks(metadata: &std::fs::Metadata) -> u64 {
    metadata.blocks()
}

/// Estimated from the length: the standard library does not expose the
/// allocation here.
#[cfg(not(unix))]
fn get_blocks(metadata: &std::fs::Metadata) -> u64 {
    metadata.len().div_ceil(512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn scan(config: &ScanConfig) -> FileTree {
        JwalkScanner::new()
            .scan(config, CancellationToken::new())
            .unwrap()
    }

    fn child<'a>(node: &'a FileNode, name: &str) -> &'a FileNode {
        node.children
            .iter()
            .find(|child| child.name.as_str() == name)
            .unwrap_or_else(|| panic!("{name} missing"))
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
        fs::create_dir(root.join("dir1")).unwrap();
        fs::create_dir(root.join("dir2")).unwrap();
        fs::create_dir(root.join("dir1/subdir")).unwrap();
        fs::write(root.join("file1.txt"), "hello").unwrap();
        fs::write(root.join("dir1/file2.txt"), "world world world").unwrap();
        fs::write(root.join("dir1/subdir/file3.txt"), "test").unwrap();
        fs::write(root.join("dir2/file4.txt"), "another file here").unwrap();
        temp
    }

    #[test]
    fn scan_counts_files_directories_and_sizes() {
        let temp = create_test_tree();
        let tree = scan(&ScanConfig::new(temp.path()));
        assert_eq!(tree.stats.total_files, 4);
        assert_eq!(tree.stats.total_dirs, 4, "three directories plus the root");
        assert_eq!(tree.root.file_count(), 4);
        assert_eq!(tree.root.dir_count(), 3);
        let children: u64 = tree.root.children.iter().map(|child| child.size).sum();
        assert!(
            tree.root.size >= children,
            "the root adds its own allocation"
        );
        let dir1 = child(&tree.root, "dir1");
        assert_eq!(dir1.file_count(), 2);
        assert_eq!(dir1.dir_count(), 1);
        assert_eq!(child(dir1, "subdir").file_count(), 1);
    }

    #[test]
    fn apparent_sizes_include_directories_themselves() {
        let temp = create_test_tree();
        let mut config = ScanConfig::new(temp.path());
        config.apparent_size = true;
        let tree = scan(&config);
        let root = temp.path();
        let dirs: u64 = [".", "dir1", "dir2", "dir1/subdir"]
            .iter()
            .map(|dir| fs::metadata(root.join(dir)).unwrap().len())
            .sum();
        assert_eq!(tree.root.size, 5 + 17 + 4 + 17 + dirs);
        assert_eq!(tree.stats.total_size, 43, "stats count file contents only");
    }

    #[test]
    fn children_are_sorted_by_size_then_name() {
        let temp = create_test_tree();
        let tree = scan(&ScanConfig::new(temp.path()));
        for pair in tree.root.children.windows(2) {
            assert!(
                pair[0].size > pair[1].size
                    || (pair[0].size == pair[1].size && pair[0].name <= pair[1].name)
            );
        }
    }

    #[test]
    fn max_depth_limits_retention_but_not_sizes() {
        let temp = create_test_tree();
        let mut config = ScanConfig::new(temp.path());
        config.apparent_size = true;
        let full = scan(&config);
        config.max_depth = Some(1);
        let shallow = scan(&config);
        assert_eq!(shallow.root.size, full.root.size);
        assert_eq!(shallow.stats.total_files, 4);
        let dir1 = child(&shallow.root, "dir1");
        assert!(
            dir1.children.is_empty(),
            "nothing below depth 1 is retained"
        );
        assert_eq!(dir1.size, child(&full.root, "dir1").size);
        assert_eq!(dir1.file_count(), 2);
        assert_eq!(dir1.dir_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_count_once_in_both_size_modes() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("a")).unwrap();
        fs::create_dir(root.join("b")).unwrap();
        fs::write(root.join("a/data"), vec![1u8; 10_000]).unwrap();
        fs::hard_link(root.join("a/data"), root.join("b/data")).unwrap();
        let linked = |tree: &FileTree| {
            child(&tree.root, "a").children[0].size + child(&tree.root, "b").children[0].size
        };
        for apparent in [false, true] {
            let one = if apparent {
                10_000
            } else {
                fs::metadata(root.join("a/data")).unwrap().blocks() * 512
            };
            let mut config = ScanConfig::new(root);
            config.apparent_size = apparent;
            assert_eq!(linked(&scan(&config)), one, "apparent={apparent}");
            config.count_hard_links = true;
            assert_eq!(linked(&scan(&config)), 2 * one, "apparent={apparent}");
        }
    }

    #[test]
    fn grafting_scanned_entries_matches_a_fresh_scan() {
        let temp = create_test_tree();
        let root = temp.path().canonicalize().unwrap();
        let config = ScanConfig::new(&root);
        let mut tree = scan(&config);
        // Change a directory and a file, and delete another file.
        fs::write(root.join("dir1/subdir/new.bin"), vec![1u8; 20_000]).unwrap();
        fs::write(root.join("file1.txt"), vec![2u8; 9_000]).unwrap();
        fs::remove_file(root.join("dir2/file4.txt")).unwrap();
        for changed in ["dir1/subdir", "file1.txt", "dir2/file4.txt"] {
            let path = root.join(changed);
            tree.remove(&path);
            if let Some(entry) = scan_entry(&path, &config, CancellationToken::new()).unwrap() {
                assert!(tree.insert(path.parent().unwrap(), entry.node));
            }
        }
        tree.recount();
        let fresh = scan(&config);
        assert_eq!(tree.root.size, fresh.root.size);
        assert_eq!(tree.root.file_count(), fresh.root.file_count());
        assert_eq!(tree.stats.total_size, fresh.stats.total_size);
        for directory in ["dir1", "dir2"] {
            assert_eq!(
                child(&tree.root, directory).size,
                child(&fresh.root, directory).size
            );
        }
        assert!(
            scan_entry(&root.join("missing"), &config, CancellationToken::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn remeasuring_a_scan_matches_scanning_with_that_measure() {
        let temp = create_test_tree();
        #[cfg(unix)]
        fs::hard_link(temp.path().join("file1.txt"), temp.path().join("dir2/link")).unwrap();
        for (apparent, links) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut config = ScanConfig::new(temp.path());
            config.apparent_size = apparent;
            config.count_hard_links = links;
            let direct = scan(&config);
            let mut converted = scan(&ScanConfig::new(temp.path()));
            converted.remeasure(apparent, links);
            let label = format!("apparent={apparent} links={links}");
            assert_eq!(converted.root.size, direct.root.size, "{label}");
            assert_eq!(
                converted.stats.total_size, direct.stats.total_size,
                "{label}"
            );
            assert_eq!(
                converted.stats.total_files, direct.stats.total_files,
                "{label}"
            );
            assert_eq!(
                child(&converted.root, "dir1").size,
                child(&direct.root, "dir1").size,
                "{label}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_listed_not_dropped() {
        use std::os::unix::ffi::OsStrExt;
        let temp = TempDir::new().unwrap();
        let fifo = temp.path().join("pipe");
        let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        let tree = scan(&ScanConfig::new(temp.path()));
        assert!(matches!(child(&tree.root, "pipe").kind, NodeKind::Other));
        assert_eq!(tree.root.file_count(), 1);
        let listed = quick_list(temp.path(), None).unwrap();
        assert!(matches!(child(&listed.root, "pipe").kind, NodeKind::Other));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_kept_as_links_and_broken_ones_warned() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("target"), "x").unwrap();
        std::os::unix::fs::symlink("target", temp.path().join("good")).unwrap();
        std::os::unix::fs::symlink("missing", temp.path().join("bad")).unwrap();
        let tree = scan(&ScanConfig::new(temp.path()));
        assert!(matches!(
            &child(&tree.root, "good").kind,
            NodeKind::Symlink { target, broken: false } if &**target == "target"
        ));
        assert!(matches!(
            child(&tree.root, "bad").kind,
            NodeKind::Symlink { broken: true, .. }
        ));
        assert_eq!(tree.stats.total_symlinks, 2);
        assert!(
            tree.warnings
                .iter()
                .any(|warning| warning.kind == WarningKind::BrokenSymlink)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directories_are_reported_as_permission_denied() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().unwrap();
        let locked = temp.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret"), "x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let tree = scan(&ScanConfig::new(temp.path()));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        // Root bypasses permissions; only assert when the read was refused.
        if child(&tree.root, "locked").children.is_empty() {
            assert!(
                tree.warnings
                    .iter()
                    .any(|warning| warning.kind == WarningKind::PermissionDenied),
                "{:?}",
                tree.warnings
            );
        }
    }

    #[test]
    fn entry_limit_fails_the_scan() {
        let temp = create_test_tree();
        let mut config = ScanConfig::new(temp.path());
        config.max_entries = Some(2);
        assert!(matches!(
            JwalkScanner::new().scan(&config, CancellationToken::new()),
            Err(ScanError::EntryLimitExceeded { limit: 2 })
        ));
    }

    #[test]
    fn cancellation_returns_interrupted() {
        let temp = create_test_tree();
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            JwalkScanner::new().scan(&ScanConfig::new(temp.path()), token),
            Err(ScanError::Interrupted)
        ));
    }

    #[test]
    fn test_ignore_patterns() {
        let temp = create_test_tree();
        let config = ScanConfig::builder()
            .root(temp.path())
            .ignore_patterns(vec!["dir2".to_string()])
            .build()
            .unwrap();
        let tree = scan(&config);
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
        let tree = scan(&config);
        assert!(
            child(child(&tree.root, "dir1"), "subdir")
                .children
                .is_empty()
        );
    }

    #[test]
    fn quick_list_respects_hidden() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::write(root.join(".hidden"), "secret").unwrap();
        fs::write(root.join("visible"), "public").unwrap();
        for include_hidden in [false, true] {
            let config = ScanConfig::builder()
                .root(root)
                .include_hidden(include_hidden)
                .build()
                .unwrap();
            let tree = quick_list(root, Some(&config)).unwrap();
            let names: Vec<_> = tree.root.children.iter().map(|c| c.name.as_str()).collect();
            assert!(names.contains(&"visible"));
            assert_eq!(names.contains(&".hidden"), include_hidden);
        }
    }
}
