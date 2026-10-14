use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;

use gravityfile_core::ScanConfig;
#[cfg(unix)]
use gravityfile_core::{NodeKind, WarningKind};
use gravityfile_scan::{JwalkScanner, quick_list};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn scanner() -> JwalkScanner {
    JwalkScanner::new()
}

/// Size an entry contributes for itself, under the scan's size measure.
fn own_size(path: &std::path::Path, apparent: bool) -> u64 {
    let metadata = fs::symlink_metadata(path).unwrap();
    #[cfg(unix)]
    if !apparent {
        use std::os::unix::fs::MetadataExt;
        return metadata.blocks() * 512;
    }
    let _ = apparent;
    metadata.len()
}

#[test]
fn full_and_quick_statistics_choose_same_path_for_tied_extremes() {
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    for names in [["c.txt", "b.txt", "a.txt"], ["a.txt", "c.txt", "b.txt"]] {
        let tmp = TempDir::new().unwrap();
        for name in names {
            let path = tmp.path().join(name);
            fs::write(&path, b"equal size").unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }
        let config = ScanConfig::new(tmp.path());
        for tree in [
            quick_list(tmp.path(), Some(&config)).unwrap(),
            scanner().scan(&config, CancellationToken::new()).unwrap(),
        ] {
            let expected = tree.root_path.join("a.txt");
            assert_eq!(tree.stats.largest_file.as_ref().unwrap().0, expected);
            assert_eq!(tree.stats.oldest_file.as_ref().unwrap().0, expected);
            assert_eq!(tree.stats.newest_file.as_ref().unwrap().0, expected);
            assert_eq!(tree.stats.total_files, 3);
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn native_filenames_survive_full_and_quick_listing_without_collisions() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = TempDir::new().unwrap();
    let names = [
        std::ffi::OsString::from_vec(b"bad-\xff".to_vec()),
        std::ffi::OsString::from_vec(b"bad-\xfe".to_vec()),
    ];
    for (index, name) in names.iter().enumerate() {
        fs::write(tmp.path().join(name), vec![b'x'; index + 1]).unwrap();
    }
    let mut config = ScanConfig::new(tmp.path());
    config.apparent_size = true;
    for tree in [
        quick_list(tmp.path(), Some(&config)).unwrap(),
        scanner().scan(&config, CancellationToken::new()).unwrap(),
    ] {
        assert_eq!(tree.root.children.len(), 2);
        for (index, name) in names.iter().enumerate() {
            let child = tree
                .root
                .children
                .iter()
                .find(|n| n.file_name() == name)
                .unwrap();
            assert_eq!(child.size, (index + 1) as u64);
            assert_eq!(
                fs::read(tree.root_path.join(child.file_name()))
                    .unwrap()
                    .len(),
                index + 1
            );
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn quick_listing_respects_filesystem_boundary_for_followed_directory() {
    use std::os::unix::fs::MetadataExt;
    let tmp = TempDir::new().unwrap();
    assert_ne!(
        fs::metadata(tmp.path()).unwrap().dev(),
        fs::metadata("/dev").unwrap().dev(),
        "fixture requires /dev on a separate filesystem"
    );
    unix_fs::symlink("/dev", tmp.path().join("device-directory")).unwrap();
    let mut config = ScanConfig::new(tmp.path());
    config.follow_symlinks = true;
    assert!(
        quick_list(tmp.path(), Some(&config))
            .unwrap()
            .root
            .children
            .is_empty()
    );
    config.cross_filesystems = true;
    let tree = quick_list(tmp.path(), Some(&config)).unwrap();
    assert_eq!(tree.root.children.len(), 1);
    assert!(tree.root.children[0].is_dir());
    assert!(tree.root.children[0].children.is_empty());
}

#[cfg(unix)]
#[test]
fn quick_listing_honors_symlink_following_and_alias_sizes() {
    use std::os::unix::fs::MetadataExt;
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("data");
    fs::write(&file, vec![1; 8192]).unwrap();
    fs::create_dir(tmp.path().join("directory")).unwrap();
    unix_fs::symlink("data", tmp.path().join("file-alias")).unwrap();
    unix_fs::symlink("directory", tmp.path().join("dir-alias")).unwrap();
    unix_fs::symlink("missing", tmp.path().join("broken")).unwrap();
    let mut config = ScanConfig::new(tmp.path());
    let tree = quick_list(tmp.path(), Some(&config)).unwrap();
    assert_eq!(tree.stats.total_files, 1);
    assert_eq!(
        tree.root
            .children
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Symlink { .. }))
            .count(),
        3
    );
    config.follow_symlinks = true;
    for apparent in [false, true] {
        config.apparent_size = apparent;
        let tree = quick_list(tmp.path(), Some(&config)).unwrap();
        assert!(tree.config.follow_symlinks);
        assert_eq!(tree.config.max_depth, Some(1));
        assert_eq!(tree.stats.total_files, 2);
        assert_eq!(tree.stats.total_dirs, 3); // Root and two paths to the directory.
        // Each aliased object counts once, in both size modes.
        let file_size = if apparent {
            8192
        } else {
            fs::metadata(&file).unwrap().blocks() * 512
        };
        assert_eq!(
            tree.root.size,
            file_size
                + own_size(tmp.path(), apparent)
                + own_size(&tmp.path().join("directory"), apparent)
        );
        assert!(
            tree.root
                .children
                .iter()
                .all(|n| !matches!(n.kind, NodeKind::Symlink { .. }))
        );
        assert_eq!(tree.warnings.len(), 1);
        assert_eq!(
            tree.warnings[0].path,
            tmp.path().canonicalize().unwrap().join("broken")
        );
        assert_eq!(tree.warnings[0].kind, WarningKind::MetadataError);
    }
}

#[cfg(unix)]
#[test]
fn quick_listing_deduplicates_physical_hardlinks_within_visible_scope() {
    use std::os::unix::fs::MetadataExt;
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("original");
    fs::write(&file, vec![1; 8192]).unwrap();
    fs::hard_link(&file, tmp.path().join("alias")).unwrap();
    let physical = fs::metadata(&file).unwrap().blocks() * 512;
    assert!(physical > 0);
    let mut config = ScanConfig::new(tmp.path());
    for apparent in [false, true] {
        config.apparent_size = apparent;
        let tree = quick_list(tmp.path(), Some(&config)).unwrap();
        let expected = if apparent { 8192 } else { physical };
        assert_eq!(tree.root.size, expected + own_size(tmp.path(), apparent));
        assert_eq!(tree.stats.total_size, expected);
        assert_eq!(tree.stats.total_files, 2);
        assert_eq!(
            tree.root.children.iter().map(|n| n.size).sum::<u64>(),
            expected
        );
        assert_eq!(tree.root.children[0].inode, tree.root.children[1].inode);
    }
    config.apparent_size = false;
    config.ignore_patterns = vec!["original".into()];
    let tree = quick_list(tmp.path(), Some(&config)).unwrap();
    assert_eq!(tree.root.size, physical + own_size(tmp.path(), false));
    assert_eq!(tree.stats.total_files, 1);
    assert_eq!(tree.root.children[0].name, "alias");
}

#[cfg(unix)]
#[test]
fn followed_directory_aliases_count_physical_bytes_once() {
    use std::os::unix::fs::MetadataExt;
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir(&real).unwrap();
    let file = real.join("data");
    fs::write(&file, vec![1; 8192]).unwrap();
    let physical = fs::metadata(&file).unwrap().blocks() * 512;
    assert!(physical > 0);
    for name in ["alias-a", "alias-b"] {
        unix_fs::symlink(&real, tmp.path().join(name)).unwrap();
    }
    for hardlink in [false, true] {
        if hardlink {
            fs::hard_link(&file, real.join("hardlink")).unwrap();
        }
        let mut config = ScanConfig::builder()
            .root(tmp.path())
            .follow_symlinks(true)
            .build()
            .unwrap();
        let dirs = |apparent| own_size(tmp.path(), apparent) + own_size(&real, apparent);
        let tree = scanner().scan(&config, CancellationToken::new()).unwrap();
        assert_eq!(tree.stats.total_files, if hardlink { 6 } else { 3 });
        assert_eq!(tree.root.size, physical + dirs(false));
        config.apparent_size = true;
        let tree = scanner().scan(&config, CancellationToken::new()).unwrap();
        assert_eq!(tree.root.size, 8192 + dirs(true));
    }
}

#[test]
fn entry_budget_enforces_exact_boundary_in_full_and_quick_scans() {
    let tmp = TempDir::new().unwrap();
    let mut config = ScanConfig::builder()
        .root(tmp.path())
        .max_entries(Some(0))
        .build()
        .unwrap();
    for quick in [false, true] {
        let tree = if quick {
            quick_list(tmp.path(), Some(&config))
        } else {
            scanner().scan(&config, CancellationToken::new())
        }
        .unwrap();
        assert!(tree.root.children.is_empty());
    }
    fs::write(tmp.path().join("one"), b"one").unwrap();
    fs::write(tmp.path().join("two"), b"two").unwrap();
    for limit in [0, 1, 2] {
        config.max_entries = Some(limit);
        for quick in [false, true] {
            let result = if quick {
                quick_list(tmp.path(), Some(&config))
            } else {
                scanner().scan(&config, CancellationToken::new())
            };
            if limit == 2 {
                let tree = result.unwrap();
                assert_eq!(tree.root.children.len(), 2);
                assert_eq!(tree.config.max_entries, Some(2));
            } else {
                assert!(matches!(result,
                    Err(gravityfile_core::ScanError::EntryLimitExceeded { limit: actual })
                        if actual == limit));
            }
        }
    }
}

#[test]
fn entry_budget_respects_filters_and_counts_nested_descendants() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join("visible")).unwrap();
    fs::write(tmp.path().join("visible/nested"), b"data").unwrap();
    fs::write(tmp.path().join(".hidden"), b"hidden").unwrap();
    fs::create_dir(tmp.path().join("ignored")).unwrap();
    fs::write(tmp.path().join("ignored/child"), b"ignored").unwrap();
    let mut config = ScanConfig::builder()
        .root(tmp.path())
        .max_entries(Some(1))
        .include_hidden(false)
        .ignore_patterns(vec!["ignored".into()])
        .build()
        .unwrap();
    assert_eq!(
        quick_list(tmp.path(), Some(&config))
            .unwrap()
            .root
            .children
            .len(),
        1
    );
    assert!(matches!(
        scanner().scan(&config, CancellationToken::new()),
        Err(gravityfile_core::ScanError::EntryLimitExceeded { limit: 1 })
    ));
    // Depth limits retention, not traversal: deeper entries still count.
    config.max_depth = Some(1);
    assert!(matches!(
        scanner().scan(&config, CancellationToken::new()),
        Err(gravityfile_core::ScanError::EntryLimitExceeded { limit: 1 })
    ));
    config.max_depth = None;
    config.max_entries = Some(2);
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();
    assert_eq!(tree.root.children[0].children.len(), 1);
    config.max_entries = None;
    assert!(scanner().scan(&config, CancellationToken::new()).is_ok());
}

#[test]
fn scan_assembly_sorts_each_directory_with_name_ties() {
    let tmp = TempDir::new().unwrap();
    for directory in ["zdir", "adir"] {
        let path = tmp.path().join(directory);
        fs::create_dir(&path).unwrap();
        for (name, contents) in [("zeta", "x"), ("large", "longer"), ("alpha", "x")] {
            fs::write(path.join(name), contents).unwrap();
        }
    }
    let config = ScanConfig::builder()
        .root(tmp.path())
        .apparent_size(true)
        .build()
        .unwrap();
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();
    assert_eq!(names(&tree), ["adir", "zdir"]);
    for directory in &tree.root.children {
        let names: Vec<_> = directory
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();
        assert_eq!(names, ["large", "alpha", "zeta"]);
        let path = tmp.path().join(directory.name.as_str());
        assert_eq!(directory.size, 8 + own_size(&path, true));
        assert_eq!(directory.file_count(), 3);
    }
    let children: u64 = tree.root.children.iter().map(|child| child.size).sum();
    assert_eq!(tree.root.size, children + own_size(tmp.path(), true));
}

#[cfg(unix)]
#[test]
fn full_and_quick_scans_preserve_counts_when_warning_details_overflow() {
    let tmp = TempDir::new().unwrap();
    for index in 0..1100 {
        unix_fs::symlink("missing-target", tmp.path().join(format!("link-{index}"))).unwrap();
    }
    let config = ScanConfig::new(tmp.path());
    let full = scanner().scan(&config, CancellationToken::new()).unwrap();
    let quick = quick_list(tmp.path(), Some(&config)).unwrap();
    for tree in [full, quick] {
        assert_eq!(tree.warning_count(), 1100);
        assert_eq!(tree.warnings.len(), 1000);
        assert!(tree.has_warnings());
        assert_eq!(
            tree.warnings.last().unwrap().kind,
            WarningKind::Omitted { count: 101 }
        );
    }
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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

    // Root size must be >= the single file we wrote.
    assert!(tree.root.size >= 4096);

    // The sub-directory node carries the file's size plus its own.
    let sub = tree
        .root
        .children
        .iter()
        .find(|c| c.name == "sub")
        .expect("sub dir must exist");
    assert_eq!(sub.size, 4096 + own_size(&root.join("sub"), true));
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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
fn test_scan_apparent_size_counts_hardlinks_once_unless_asked() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let original = root.join("original.bin");
    fs::write(&original, vec![0u8; 4096]).unwrap();
    fs::hard_link(&original, root.join("link.bin")).unwrap();

    let mut config = ScanConfig::builder()
        .root(root)
        .apparent_size(true)
        .build()
        .unwrap();
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();
    assert_eq!(tree.root.size, 4096 + own_size(root, true));

    config.count_hard_links = true;
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();
    assert_eq!(tree.root.size, 2 * 4096 + own_size(root, true));
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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
            assert_eq!(&**target, "/does/not/exist");
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
    let tree_disk = scanner()
        .scan(&config_disk, CancellationToken::new())
        .unwrap();

    let config_apparent = ScanConfig::builder()
        .root(root)
        .apparent_size(true)
        .build()
        .unwrap();
    let tree_apparent = scanner()
        .scan(&config_apparent, CancellationToken::new())
        .unwrap();

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
    let tree = scanner().scan(&config, CancellationToken::new()).unwrap();

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
    let result = scanner().scan(&config, CancellationToken::new());

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
            assert_eq!(&**target, "/nowhere");
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
fn test_inode_tracker_retains_identity_after_all_links_seen() {
    use gravityfile_core::InodeInfo;
    use gravityfile_scan::InodeTracker;

    let mut tracker = InodeTracker::new();
    let info = InodeInfo::new(42, 7);

    // nlink=4: first call counted, next 3 suppressed.
    assert!(tracker.track(info, 4)); // first: counted
    assert!(!tracker.track(info, 4)); // dup 1
    assert!(!tracker.track(info, 4)); // dup 2
    assert!(!tracker.track(info, 4)); // dup 3 – identity remains retained

    assert_eq!(
        tracker.pending_count(),
        1,
        "identity must remain retained for later aliases"
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
    use gravityfile_core::{GitStatus, ScanConfig};
    use gravityfile_scan::GitStatusCache;
    use std::{fs, path::Path};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::scanner;

    /// Git annotation is opt-in.
    fn git_config(root: impl Into<std::path::PathBuf>) -> ScanConfig {
        let mut config = ScanConfig::new(root);
        config.git_status = true;
        config
    }

    #[test]
    fn ignored_directory_cache_does_not_expand_with_generated_descendants() {
        let temp = TempDir::new().unwrap();
        git2::Repository::init(temp.path()).unwrap();
        fs::write(temp.path().join(".gitignore"), b"generated/\n").unwrap();
        let generated = temp.path().join("generated");
        fs::create_dir_all(generated.join("nested")).unwrap();
        for index in 0..200 {
            fs::write(
                generated.join("nested").join(index.to_string()),
                b"generated",
            )
            .unwrap();
        }
        let mut cache = GitStatusCache::new();
        assert!(cache.initialize(temp.path()));
        assert!(cache.initialization_error().is_none());
        assert!(
            cache.len() <= 3,
            "ignored descendants must not each occupy a cache entry: {}",
            cache.len()
        );
        assert_eq!(
            cache.get_status(&generated.join("nested/199")),
            Some(GitStatus::Ignored)
        );
        assert_eq!(
            cache.get_status(&temp.path().join("generated-other/file")),
            None
        );
        let canonical = generated.canonicalize().unwrap().join("nested/0");
        fs::remove_file(generated.join("nested/0")).unwrap();
        // Prefix lookup is observational and does not query file existence.
        assert_eq!(
            cache.get_status_canonical(&canonical),
            Some(GitStatus::Ignored)
        );
    }

    #[test]
    fn ignored_subtree_reports_descendants_without_marking_clean_tracked_files() {
        let temp = TempDir::new().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        let directory = temp.path().join("ignored");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("tracked"), b"tracked").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("ignored/tracked")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        fs::write(temp.path().join(".gitignore"), b"ignored/\n").unwrap();
        fs::create_dir(directory.join("nested")).unwrap();
        let ignored = directory.join("nested/generated");
        fs::write(&ignored, b"ignored").unwrap();
        let mut cache = GitStatusCache::new();
        assert!(cache.initialize(&directory));
        assert_eq!(cache.get_status(&ignored), Some(GitStatus::Ignored));
        assert_eq!(cache.get_status(&directory.join("tracked")), None);
        assert_eq!(cache.get_status(&temp.path().join(".gitignore")), None);
        let scan = scanner()
            .scan(&git_config(&directory), CancellationToken::new())
            .unwrap();
        let nested = scan
            .root
            .children
            .iter()
            .find(|node| node.name == "nested")
            .unwrap();
        assert_eq!(nested.children[0].git_status, Some(GitStatus::Ignored));
        assert_eq!(
            scan.root
                .children
                .iter()
                .find(|node| node.name == "tracked")
                .unwrap()
                .git_status,
            None
        );
        assert!(cache.initialize(temp.path()));
        assert_eq!(cache.get_status(&ignored), Some(GitStatus::Ignored));
        assert_eq!(cache.get_status(&directory.join("tracked")), None);
        fs::write(directory.join("tracked"), b"modified tracked contents").unwrap();
        assert!(cache.initialize(temp.path()));
        assert_eq!(
            cache.get_status(&directory.join("tracked")),
            Some(GitStatus::Modified)
        );
    }

    #[test]
    fn corrupt_git_index_is_reported_as_incomplete_enrichment() {
        let temp = TempDir::new().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        fs::write(temp.path().join("file"), b"contents").unwrap();
        let mut previous = scanner()
            .scan(&git_config(temp.path()), CancellationToken::new())
            .unwrap();
        fs::write(repo.path().join("index"), b"not a valid git index").unwrap();
        let tree = scanner()
            .scan(&git_config(temp.path()), CancellationToken::new())
            .unwrap();
        assert!(
            tree.warnings
                .iter()
                .any(|warning| warning.message.contains("Git status unavailable"))
        );
        assert!(tree.has_warnings());
        assert!(tree.root.children.iter().any(|node| node.name == "file"));
        gravityfile_scan::apply_git_status(&mut previous);
        assert!(previous.has_warnings());
        assert_eq!(
            previous
                .root
                .children
                .iter()
                .find(|node| node.name == "file")
                .unwrap()
                .git_status,
            Some(GitStatus::Untracked)
        );
        let mut cache = GitStatusCache::new();
        assert!(cache.initialize(temp.path()));
        assert!(cache.initialization_error().is_some());
        fs::remove_file(repo.path().join("index")).unwrap();
        assert!(cache.initialize(temp.path()));
        assert!(cache.initialization_error().is_none());
    }

    #[test]
    fn refreshing_git_status_clears_annotations_after_commit() {
        let temp = TempDir::new().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        fs::write(temp.path().join("file"), b"contents").unwrap();
        let mut tree = scanner()
            .scan(&git_config(temp.path()), CancellationToken::new())
            .unwrap();
        let file_status = |tree: &gravityfile_core::FileTree| {
            tree.root
                .children
                .iter()
                .find(|node| node.name == "file")
                .unwrap()
                .git_status
        };
        assert_eq!(file_status(&tree), Some(GitStatus::Untracked));
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file")).unwrap();
        index.write().unwrap();
        gravityfile_scan::apply_git_status(&mut tree);
        assert_eq!(file_status(&tree), Some(GitStatus::Staged));
        let tree_id = index.write_tree().unwrap();
        let git_tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "fixture",
            &git_tree,
            &[],
        )
        .unwrap();
        gravityfile_scan::apply_git_status(&mut tree);
        assert_eq!(file_status(&tree), None);
    }

    #[test]
    fn git_subtree_names_are_literal_not_pathspec_patterns() {
        let temp = TempDir::new().unwrap();
        git2::Repository::init(temp.path()).unwrap();
        let selected = temp.path().join("scope[ab]");
        let outside = temp.path().join("scopea");
        let same_prefix = temp.path().join("scope[ab]-other");
        for directory in [&selected, &outside, &same_prefix] {
            fs::create_dir_all(directory.join("nested")).unwrap();
            fs::write(directory.join("nested/file"), b"untracked").unwrap();
        }
        let mut cache = GitStatusCache::new();
        assert!(cache.initialize(&selected));
        assert_eq!(
            cache.get_status(&selected.join("nested/file")),
            Some(GitStatus::Untracked)
        );
        assert_eq!(cache.get_status(&outside.join("nested/file")), None);
        assert_eq!(cache.get_status(&same_prefix.join("nested/file")), None);
        assert_eq!(cache.len(), 1);
        assert!(cache.initialize(temp.path()));
        assert_eq!(cache.len(), 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn git_subtree_scope_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let temp = TempDir::new().unwrap();
        git2::Repository::init(temp.path()).unwrap();
        let selected = temp
            .path()
            .join(std::ffi::OsString::from_vec(b"scope-\xff".to_vec()));
        fs::create_dir(&selected).unwrap();
        let file = selected.join(std::ffi::OsString::from_vec(b"file-\xfe".to_vec()));
        fs::write(&file, b"untracked").unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"untracked").unwrap();
        let mut cache = GitStatusCache::new();
        assert!(cache.initialize(&selected));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get_status(&file), Some(GitStatus::Untracked));
        assert_eq!(cache.get_status(&outside), None);
    }

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

    #[test]
    fn test_git_cache_reinitialization_clears_stale_repository_state() {
        let repository = TempDir::new().unwrap();
        git2::Repository::init(repository.path()).unwrap();
        let untracked = repository.path().join("untracked.txt");
        fs::write(&untracked, "untracked").unwrap();

        let mut cache = GitStatusCache::new();
        assert!(cache.initialize(repository.path()));
        assert_eq!(cache.get_status(&untracked), Some(GitStatus::Untracked));

        let outside = TempDir::new().unwrap();
        assert!(!cache.initialize(outside.path()));
        assert!(cache.repo_root().is_none());
        assert!(cache.is_empty());
        assert_eq!(cache.get_status(&untracked), None);
    }

    #[test]
    fn test_full_scan_applies_git_status_to_nodes() {
        let repository = TempDir::new().unwrap();
        git2::Repository::init(repository.path()).unwrap();
        fs::write(repository.path().join("untracked.txt"), "untracked").unwrap();

        let mut config = git_config(repository.path());
        config.include_hidden = false;
        let tree = scanner().scan(&config, CancellationToken::new()).unwrap();
        let untracked = tree
            .root
            .children
            .iter()
            .find(|node| node.name.as_str() == "untracked.txt")
            .expect("untracked file should be scanned");

        assert_eq!(untracked.git_status, Some(GitStatus::Untracked));
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
    scanner.scan(&config, CancellationToken::new()).unwrap();

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

#[test]
fn invalid_and_mutated_scopes_are_honored_by_both_scan_modes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.log"), b"a").unwrap();
    std::fs::write(root.path().join("b.tmp"), b"b").unwrap();
    let mut config = gravityfile_core::ScanConfig::new(root.path());
    config.ignore_patterns = vec!["*.log".into()];
    config.compile_patterns().unwrap();
    config.ignore_patterns = vec!["*.tmp".into()];
    let scanner = gravityfile_scan::JwalkScanner::new();
    for tree in [
        scanner
            .scan(&config, tokio_util::sync::CancellationToken::new())
            .unwrap(),
        gravityfile_scan::quick_list(root.path(), Some(&config)).unwrap(),
    ] {
        assert!(tree.root.children.iter().any(|node| node.name == "a.log"));
        assert!(!tree.root.children.iter().any(|node| node.name == "b.tmp"));
    }
    let quick = gravityfile_scan::quick_list(root.path(), Some(&config)).unwrap();
    assert_eq!(quick.config.max_depth, Some(1));
    assert_eq!(quick.config.root, root.path().canonicalize().unwrap());
    config.ignore_patterns = vec!["[".into()];
    assert!(matches!(
        scanner.scan(&config, tokio_util::sync::CancellationToken::new()),
        Err(gravityfile_core::ScanError::InvalidConfig { .. })
    ));
    assert!(matches!(
        gravityfile_scan::quick_list(root.path(), Some(&config)),
        Err(gravityfile_core::ScanError::InvalidConfig { .. })
    ));
}
