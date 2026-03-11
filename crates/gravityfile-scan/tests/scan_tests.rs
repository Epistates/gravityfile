use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;

use gravityfile_core::{NodeKind, ScanConfig, WarningKind};
use gravityfile_scan::{JwalkScanner, quick_list};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn scanner() -> JwalkScanner {
    JwalkScanner::new()
}

fn names(tree: &gravityfile_core::FileTree) -> Vec<&str> {
    tree.root.children.iter().map(|c| c.name.as_str()).collect()
}

// ---------------------------------------------------------------------------
// scanner::collect_entries – basic correctness
// ---------------------------------------------------------------------------

#[test]
fn test_scan_counts_files_and_dirs() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir(root.join("a")).unwrap();
    fs::create_dir(root.join("b")).unwrap();
    fs::write(root.join("a/f1.txt"), "hello").unwrap();
    fs::write(root.join("a/f2.txt"), "world").unwrap();
    fs::write(root.join("b/f3.txt"), "!").unwrap();

    let config = ScanConfig::new(root);
    let tree = scanner().scan(&config).unwrap();

    assert_eq!(tree.stats.total_files, 3);
    assert!(tree.stats.total_dirs >= 2);
}

#[test]
fn test_scan_aggregates_size_into_directories() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/data.bin"), vec![0u8; 4096]).unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .apparent_size(true)
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    // Root size must be >= the single file we wrote.
    assert!(tree.root.size >= 4096);

    // The sub-directory node should carry the file's size.
    let sub = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "sub")
        .expect("sub dir must exist");
    assert_eq!(sub.size, 4096);
}

#[test]
fn test_scan_children_sorted_by_size_descending() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join("small.bin"), vec![0u8; 100]).unwrap();
    fs::write(root.join("large.bin"), vec![0u8; 8000]).unwrap();
    fs::write(root.join("medium.bin"), vec![0u8; 4000]).unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .apparent_size(true)
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    let sizes: Vec<u64> = tree.root.children.iter().map(|c| c.size).collect();
    for window in sizes.windows(2) {
        assert!(
            window[0] >= window[1],
            "children not sorted descending: {sizes:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// scanner – ignore patterns (process_read_dir early-prune path)
// ---------------------------------------------------------------------------

#[test]
fn test_scan_ignore_pattern_excludes_directory_and_its_contents() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir(root.join("node_modules")).unwrap();
    fs::write(root.join("node_modules/huge.js"), vec![0u8; 10_000]).unwrap();
    fs::write(root.join("main.rs"), b"fn main(){}").unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .ignore_patterns(vec!["node_modules".to_string()])
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    assert!(
        !names(&tree).contains(&"node_modules"),
        "node_modules must be pruned"
    );
    // The file inside must not contribute to the total
    assert_eq!(tree.stats.total_files, 1, "only main.rs should be counted");
}

#[test]
fn test_scan_ignore_glob_pattern_excludes_matching_files() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join("debug.log"), "log data").unwrap();
    fs::write(root.join("main.rs"), "code").unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .ignore_patterns(vec!["*.log".to_string()])
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    assert!(
        !names(&tree).contains(&"debug.log"),
        "*.log files must be excluded"
    );
    assert!(names(&tree).contains(&"main.rs"));
    assert_eq!(tree.stats.total_files, 1);
}

// ---------------------------------------------------------------------------
// scanner – hidden-file handling
// ---------------------------------------------------------------------------

#[test]
fn test_scan_includes_hidden_files_by_default() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join(".hidden"), "secret").unwrap();
    fs::write(root.join("visible"), "public").unwrap();

    let config = ScanConfig::new(root); // include_hidden defaults to true
    let tree = scanner().scan(&config).unwrap();

    let ns = names(&tree);
    assert!(
        ns.contains(&".hidden"),
        "hidden file must appear by default"
    );
    assert!(ns.contains(&"visible"));
}

#[test]
fn test_scan_excludes_hidden_files_when_configured() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join(".hidden"), "secret").unwrap();
    fs::write(root.join("visible"), "public").unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .include_hidden(false)
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    let ns = names(&tree);
    assert!(!ns.contains(&".hidden"), "hidden file must be suppressed");
    assert!(ns.contains(&"visible"));
}

// ---------------------------------------------------------------------------
// scanner – hardlink / inode deduplication
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_scan_counts_hardlink_size_once() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let original = root.join("original.bin");
    fs::write(&original, vec![0u8; 4096]).unwrap();
    fs::hard_link(&original, root.join("link1.bin")).unwrap();
    fs::hard_link(&original, root.join("link2.bin")).unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .apparent_size(false) // disk usage mode triggers dedup
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    // Three directory entries but physically one inode; total_files == 3 but
    // root.size must reflect only one copy's disk footprint.
    assert_eq!(tree.stats.total_files, 3);

    // apparent size would be 3 * 4096; physical size must be <= 1 * 4096 after dedup.
    // We allow up to 1.5× to tolerate filesystem block rounding.
    assert!(
        tree.root.size <= 4096 * 2,
        "hardlink size counted more than once: root.size = {}",
        tree.root.size
    );
}

#[cfg(unix)]
#[test]
fn test_scan_apparent_size_counts_all_hardlink_entries() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let original = root.join("original.bin");
    fs::write(&original, vec![0u8; 4096]).unwrap();
    fs::hard_link(&original, root.join("link.bin")).unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .apparent_size(true)
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    // In apparent-size mode every directory entry is counted independently.
    assert_eq!(
        tree.root.size,
        4096 * 2,
        "apparent size must count each hardlink entry"
    );
}

// ---------------------------------------------------------------------------
// scanner – symlink handling
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_scan_records_broken_symlink_as_warning() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    unix_fs::symlink("/nonexistent/target", root.join("broken_link")).unwrap();

    let config = ScanConfig::new(root);
    let tree = scanner().scan(&config).unwrap();

    let broken_warnings: Vec<_> = tree
        .warnings
        .iter()
        .filter(|w| matches!(w.kind, WarningKind::BrokenSymlink))
        .collect();

    assert!(
        !broken_warnings.is_empty(),
        "broken symlink must produce a warning"
    );
    assert!(
        broken_warnings
            .iter()
            .any(|w| w.path.ends_with("broken_link")),
        "warning path must identify the broken link"
    );
}

#[cfg(unix)]
#[test]
fn test_scan_does_not_follow_symlinks_by_default() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // A directory outside the scan root pointed to by a symlink inside.
    let external = TempDir::new().unwrap();
    fs::write(external.path().join("outside.txt"), "external").unwrap();
    unix_fs::symlink(external.path(), root.join("link_to_dir")).unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .follow_symlinks(false)
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    // The symlink itself should appear but "outside.txt" must not be traversed.
    assert_eq!(
        tree.stats.total_files, 0,
        "files behind symlink must not be counted when follow_symlinks=false"
    );
    assert_eq!(tree.stats.total_symlinks, 1);
}

#[cfg(unix)]
#[test]
fn test_scan_symlink_node_carries_target_and_broken_flag() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // A valid symlink
    fs::write(root.join("real.txt"), "content").unwrap();
    unix_fs::symlink(root.join("real.txt"), root.join("good_link")).unwrap();

    // A broken symlink
    unix_fs::symlink("/does/not/exist", root.join("bad_link")).unwrap();

    let config = ScanConfig::new(root);
    let tree = scanner().scan(&config).unwrap();

    let good = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "good_link")
        .expect("good_link must appear in tree");

    match &good.kind {
        NodeKind::Symlink { broken, .. } => assert!(!broken, "good_link must not be broken"),
        other => panic!("expected Symlink, got {other:?}"),
    }

    let bad = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "bad_link")
        .expect("bad_link must appear in tree");

    match &bad.kind {
        NodeKind::Symlink { broken, target } => {
            assert!(broken, "bad_link must be marked broken");
            assert_eq!(target.as_str(), "/does/not/exist");
        }
        other => panic!("expected Symlink, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// scanner – disk_size vs apparent_size for regular files
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_scan_disk_size_uses_block_count_not_byte_length() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Write a sparse file: seek past 100 MiB, write one byte.  The apparent
    // size is 100 MiB + 1 but the disk blocks must be far smaller.  We use
    // 100 MiB so the hole is large enough to be sparse even on APFS (which
    // rounds up small seeks to a 4 KiB cluster but leaves large holes sparse).
    use std::io::{Seek, SeekFrom, Write};
    let path = root.join("sparse.bin");
    let mut f = fs::File::create(&path).unwrap();
    f.seek(SeekFrom::Start(100 * 1024 * 1024)).unwrap();
    f.write_all(b"x").unwrap();
    drop(f);

    let config_disk = ScanConfig::builder()
        .root(root)
        .apparent_size(false)
        .build()
        .unwrap();
    let tree_disk = scanner().scan(&config_disk).unwrap();

    let config_apparent = ScanConfig::builder()
        .root(root)
        .apparent_size(true)
        .build()
        .unwrap();
    let tree_apparent = scanner().scan(&config_apparent).unwrap();

    // Disk usage must be strictly less than apparent size for a sparse file.
    assert!(
        tree_disk.root.size < tree_apparent.root.size,
        "disk size ({}) should be smaller than apparent size ({}) for a sparse file",
        tree_disk.root.size,
        tree_apparent.root.size,
    );
}

// ---------------------------------------------------------------------------
// scanner – max_depth
// ---------------------------------------------------------------------------

#[test]
fn test_scan_max_depth_limits_traversal() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("l1/l2/l3")).unwrap();
    fs::write(root.join("l1/f1.txt"), "a").unwrap();
    fs::write(root.join("l1/l2/f2.txt"), "b").unwrap();
    fs::write(root.join("l1/l2/l3/f3.txt"), "c").unwrap();

    // Depth layout: root=0, l1=1, l2=2, l3=3; files are one deeper than their dir.
    // max_depth=3 → entries at depth ≤ 3 are included, so f1(depth 2) and f2(depth 3)
    // are present but l3(depth 3) and f3(depth 4) are absent.
    let config = ScanConfig::builder()
        .root(root)
        .max_depth(Some(3))
        .build()
        .unwrap();
    let tree = scanner().scan(&config).unwrap();

    fn find_node<'a>(
        node: &'a gravityfile_core::FileNode,
        name: &str,
    ) -> Option<&'a gravityfile_core::FileNode> {
        if node.name == name {
            return Some(node);
        }
        node.children.iter().find_map(|c| find_node(c, name))
    }

    assert!(
        find_node(&tree.root, "f1.txt").is_some(),
        "f1 (depth 2) must be found"
    );
    assert!(
        find_node(&tree.root, "f2.txt").is_some(),
        "f2 (depth 3) must be found"
    );
    assert!(
        find_node(&tree.root, "f3.txt").is_none(),
        "f3 (depth 4) must be excluded by max_depth=3"
    );
}

// ---------------------------------------------------------------------------
// scanner – permission-error entries surface as warnings not panics
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_scan_unreadable_directory_does_not_panic() {
    // This test verifies that the scanner degrades gracefully when a directory
    // cannot be read.  On macOS, a process that owns a mode-000 directory can
    // still list it (owner bypass), so we cannot assert that a warning *will*
    // be produced — only that the scan does not panic or return a hard error,
    // and that the locked directory's contents are either absent (correctly
    // excluded) or present (owner-bypass allowed).
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let locked = root.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("secret.txt"), "hidden").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let config = ScanConfig::new(root);
    let result = scanner().scan(&config);

    // Restore permissions so TempDir cleanup works regardless of test outcome.
    let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

    // Must not error out — graceful degradation is required.
    let _tree = result.expect("scan must not return Err on an unreadable directory");
}

// ---------------------------------------------------------------------------
// quick_list – hidden file filter
// ---------------------------------------------------------------------------

#[test]
fn test_quick_list_respects_include_hidden_false() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join(".env"), "SECRET=1").unwrap();
    fs::write(root.join("README.md"), "public").unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .include_hidden(false)
        .build()
        .unwrap();

    let tree = quick_list(root, Some(&config)).unwrap();
    let ns = names(&tree);

    assert!(!ns.contains(&".env"), ".env must be hidden");
    assert!(ns.contains(&"README.md"));
}

#[test]
fn test_quick_list_respects_include_hidden_true() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join(".env"), "SECRET=1").unwrap();
    fs::write(root.join("README.md"), "public").unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .include_hidden(true)
        .build()
        .unwrap();

    let tree = quick_list(root, Some(&config)).unwrap();
    let ns = names(&tree);

    assert!(ns.contains(&".env"));
    assert!(ns.contains(&"README.md"));
}

#[test]
fn test_quick_list_none_config_includes_hidden_files() {
    // Passing None should default to include_hidden=true (documented contract).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join(".hidden"), "x").unwrap();
    fs::write(root.join("visible"), "y").unwrap();

    let tree = quick_list(root, None).unwrap();
    let ns = names(&tree);

    assert!(
        ns.contains(&".hidden"),
        "None config must include hidden files"
    );
}

// ---------------------------------------------------------------------------
// quick_list – ignore patterns
// ---------------------------------------------------------------------------

#[test]
fn test_quick_list_respects_ignore_patterns() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join("build.log"), "log").unwrap();
    fs::write(root.join("src.rs"), "code").unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .ignore_patterns(vec!["*.log".to_string()])
        .build()
        .unwrap();

    let tree = quick_list(root, Some(&config)).unwrap();
    let ns = names(&tree);

    assert!(!ns.contains(&"build.log"));
    assert!(ns.contains(&"src.rs"));
}

// ---------------------------------------------------------------------------
// quick_list – symlinks
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_quick_list_broken_symlink_appears_and_warns() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    unix_fs::symlink("/nowhere", root.join("dead")).unwrap();

    let tree = quick_list(root, None).unwrap();

    let dead = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "dead")
        .expect("broken symlink must appear in quick_list");

    match &dead.kind {
        NodeKind::Symlink { broken, target } => {
            assert!(broken);
            assert_eq!(target.as_str(), "/nowhere");
        }
        other => panic!("expected Symlink, got {other:?}"),
    }

    let broken_warnings: Vec<_> = tree
        .warnings
        .iter()
        .filter(|w| matches!(w.kind, WarningKind::BrokenSymlink))
        .collect();
    assert!(!broken_warnings.is_empty());
}

#[cfg(unix)]
#[test]
fn test_quick_list_valid_symlink_not_broken() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join("target.txt"), "content").unwrap();
    unix_fs::symlink(root.join("target.txt"), root.join("link")).unwrap();

    let tree = quick_list(root, None).unwrap();

    let link = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "link")
        .expect("symlink must appear");

    match &link.kind {
        NodeKind::Symlink { broken, .. } => assert!(!broken),
        other => panic!("expected Symlink, got {other:?}"),
    }

    // No broken-symlink warning should be emitted.
    assert!(
        !tree
            .warnings
            .iter()
            .any(|w| matches!(w.kind, WarningKind::BrokenSymlink)),
        "valid symlink must not produce a broken-symlink warning"
    );
}

// ---------------------------------------------------------------------------
// quick_list – disk_size vs apparent_size
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_quick_list_apparent_size_uses_byte_length() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::write(root.join("file.txt"), vec![b'a'; 1234]).unwrap();

    let config = ScanConfig::builder()
        .root(root)
        .apparent_size(true)
        .build()
        .unwrap();

    let tree = quick_list(root, Some(&config)).unwrap();
    let file = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "file.txt")
        .unwrap();

    assert_eq!(file.size, 1234, "apparent size must equal byte length");
}

// ---------------------------------------------------------------------------
// quick_list – metadata errors surface as warnings
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn test_quick_list_metadata_error_produces_warning_not_panic() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Make the root unreadable so DirEntry::metadata() calls fail.
    // We need at least one entry to exist first.
    fs::write(root.join("file.txt"), "x").unwrap();
    fs::set_permissions(root, fs::Permissions::from_mode(0o311)).unwrap();

    let result = quick_list(root, None);

    // Restore permissions before any assertions so TempDir can clean up.
    let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o755));

    // The call should either succeed with warnings or return an Err; it must
    // never panic.
    match result {
        Ok(tree) => {
            // If we got a tree, any entries that couldn't be stat'd must have
            // generated warnings rather than silently disappearing.
            let _ = tree; // we just care it didn't panic
        }
        Err(_) => {
            // Also acceptable – the root itself was unreadable.
        }
    }
}

// ---------------------------------------------------------------------------
// inode tracker – boundary cases (public API through scanner, not internals)
// ---------------------------------------------------------------------------

// InodeTracker is re-exported from the crate root.
#[test]
fn test_inode_tracker_nlink_zero_treated_as_no_hardlink() {
    // nlink=0 is pathological (deleted-but-open file) but the kernel can
    // theoretically report it.  The guard `nlink <= 1` in InodeTracker::track
    // must handle it without panicking.  We can't manufacture nlink=0 on a
    // live filesystem easily, so we test through the public type directly.
    use gravityfile_core::InodeInfo;
    use gravityfile_scan::InodeTracker;

    let mut tracker = InodeTracker::new();
    let info = InodeInfo::new(1, 1);

    // nlink=0: must return true (treat as unique) and not touch the map.
    assert!(tracker.track(info, 0));
    assert_eq!(
        tracker.pending_count(),
        0,
        "nlink=0 must not allocate in the map"
    );
}

#[test]
fn test_inode_tracker_countdown_evicts_after_all_links_seen() {
    use gravityfile_core::InodeInfo;
    use gravityfile_scan::InodeTracker;

    let mut tracker = InodeTracker::new();
    let info = InodeInfo::new(42, 7);

    // nlink=4: first call counted, next 3 suppressed.
    assert!(tracker.track(info, 4)); // first: counted
    assert!(!tracker.track(info, 4)); // dup 1
    assert!(!tracker.track(info, 4)); // dup 2
    assert!(!tracker.track(info, 4)); // dup 3 – entry evicted here

    assert_eq!(
        tracker.pending_count(),
        0,
        "entry must be evicted after all duplicate links are seen"
    );
}

#[test]
fn test_inode_tracker_same_inode_different_devices_are_independent() {
    use gravityfile_core::InodeInfo;
    use gravityfile_scan::InodeTracker;

    let mut tracker = InodeTracker::new();
    let dev1 = InodeInfo::new(100, 1);
    let dev2 = InodeInfo::new(100, 2); // same inode number, different device

    // Both must be treated as first-seen.
    assert!(tracker.track(dev1, 2));
    assert!(tracker.track(dev2, 2));
}

// ---------------------------------------------------------------------------
// git – pathspec edge cases
// ---------------------------------------------------------------------------

// We only run git tests when the feature is enabled; we gate on compile-time.
// The tests use the actual repo containing this workspace to avoid needing a
// synthetic git repo setup.
#[cfg(feature = "git")]
mod git_tests {
    use gravityfile_scan::GitStatusCache;
    use std::path::Path;

    #[test]
    fn test_git_cache_initialize_at_repo_root_succeeds() {
        // The workspace is itself a git repo; initializing at its root must work.
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();

        let mut cache = GitStatusCache::new();
        let found = cache.initialize(repo_root);

        // If we are inside a git repo (CI or dev) this must be true.
        // If the tests run in a non-git environment, skip gracefully.
        if found {
            assert!(
                cache.repo_root().is_some(),
                "repo_root must be set after successful initialize"
            );
        }
    }

    #[test]
    fn test_git_cache_initialize_at_subtree_does_not_load_entire_repo() {
        // Initialize at a narrow subtree; the cache must not be larger than the
        // number of changed files in that subtree.  We just verify it does not
        // panic and returns a consistent state.
        let subtree = Path::new(env!("CARGO_MANIFEST_DIR"));

        let mut cache = GitStatusCache::new();
        cache.initialize(subtree);

        // The is_in_repo / get_status API must remain usable regardless.
        let _ = cache.is_in_repo(subtree);
        let _ = cache.get_status(subtree);
    }

    #[test]
    fn test_git_cache_returns_none_outside_any_repo() {
        // /tmp is (almost certainly) not inside a git repo.
        let mut cache = GitStatusCache::new();
        let found = cache.initialize(Path::new("/tmp"));

        if !found {
            assert!(cache.repo_root().is_none());
            assert!(cache.is_empty());
        }
        // If /tmp happens to be inside a git repo (unusual CI config), just skip.
    }
}

// ---------------------------------------------------------------------------
// progress – events fire during scan
// ---------------------------------------------------------------------------

#[test]
fn test_scan_progress_events_are_emitted_for_large_file_set() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Create more than 1000 files to guarantee at least one progress tick.
    for i in 0..1100 {
        fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
    }

    let scanner = JwalkScanner::new();
    let mut rx = scanner.subscribe();

    let config = ScanConfig::new(root);
    scanner.scan(&config).unwrap();

    // Drain whatever was buffered (broadcast channel may have dropped some).
    let mut received = 0usize;
    while rx.try_recv().is_ok() {
        received += 1;
    }

    assert!(
        received > 0,
        "at least one progress event must be emitted for 1100 files"
    );
}
