use std::time::{Duration, SystemTime};

use gravityfile_core::{
    ContentHash, FileNode, FileTree, GitStatus, InodeInfo, NodeId, NodeKind, ScanConfig, ScanError,
    ScanWarning, Timestamps, TreeStats, WarningKind,
};

// ---------------------------------------------------------------------------
// NodeId
// ---------------------------------------------------------------------------

#[test]
fn test_node_id_operations() {
    let id1 = NodeId::new(42);
    let id2 = NodeId::new(42);

    assert_eq!(id1, id2);
    assert_eq!(id1.get(), 42);
}

#[test]
fn test_node_id_zero() {
    let id = NodeId::new(0);
    assert_eq!(id.get(), 0);
}

#[test]
fn test_node_id_max() {
    let id = NodeId::new(u64::MAX);
    assert_eq!(id.get(), u64::MAX);
}

#[test]
fn test_node_id_display() {
    let id = NodeId::new(42);
    assert_eq!(format!("{id}"), "42");
    assert_eq!(format!("{}", NodeId::new(0)), "0");
    assert_eq!(format!("{}", NodeId::new(u64::MAX)), u64::MAX.to_string());
}

#[test]
fn test_node_id_hash() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(NodeId::new(1));
    set.insert(NodeId::new(2));
    set.insert(NodeId::new(1)); // duplicate
    assert_eq!(set.len(), 2);
}

// ---------------------------------------------------------------------------
// ContentHash
// ---------------------------------------------------------------------------

#[test]
fn test_content_hash_creation_and_hex() {
    let bytes = [0xab; 32];
    let hash = ContentHash::new(bytes);

    let hex = hash.to_hex();
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(hex.starts_with("ab"));

    let hash2 = ContentHash::new(bytes);
    assert_eq!(hash, hash2);

    let hash3 = ContentHash::new([0xcd; 32]);
    assert_ne!(hash, hash3);
}

#[test]
fn test_content_hash_as_bytes_roundtrip() {
    let bytes = [0xcd; 32];
    let hash = ContentHash::new(bytes);
    assert_eq!(hash.as_bytes(), &bytes);
    // Reconstructing from as_bytes should produce equal hash
    assert_eq!(ContentHash::new(*hash.as_bytes()), hash);
}

#[test]
fn test_content_hash_display_matches_to_hex() {
    let hash = ContentHash::new([0xab; 32]);
    assert_eq!(format!("{hash}"), hash.to_hex());
}

#[test]
fn test_content_hash_all_zeros() {
    let hash = ContentHash::new([0u8; 32]);
    let hex = hash.to_hex();
    assert_eq!(hex, "0".repeat(64));
}

#[test]
fn test_content_hash_all_ff() {
    let hash = ContentHash::new([0xffu8; 32]);
    let hex = hash.to_hex();
    assert_eq!(hex, "ff".repeat(32));
}

// ---------------------------------------------------------------------------
// InodeInfo
// ---------------------------------------------------------------------------

#[test]
fn test_inode_info() {
    let inode1 = InodeInfo::new(12345, 67890);
    assert_eq!(inode1.inode, 12345);
    assert_eq!(inode1.device, 67890);

    let inode2 = InodeInfo::new(12345, 67890);
    assert_eq!(inode1, inode2);
}

#[test]
fn test_inode_info_inequality() {
    let a = InodeInfo::new(1, 1);
    let b = InodeInfo::new(1, 2); // different device
    let c = InodeInfo::new(2, 1); // different inode
    assert_ne!(a, b);
    assert_ne!(a, c);
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

#[test]
fn test_timestamps() {
    let now = SystemTime::now();
    let timestamps = Timestamps::with_modified(now);

    assert_eq!(timestamps.modified, now);
    assert!(timestamps.accessed.is_none());
    assert!(timestamps.created.is_none());

    let accessed = now - Duration::from_secs(3600);
    let created = now - Duration::from_secs(7200);

    let full = Timestamps::new(now, Some(accessed), Some(created));
    assert_eq!(full.modified, now);
    assert_eq!(full.accessed, Some(accessed));
    assert_eq!(full.created, Some(created));
}

#[test]
fn test_timestamps_none_fields() {
    let t = Timestamps::new(SystemTime::UNIX_EPOCH, None, None);
    assert_eq!(t.modified, SystemTime::UNIX_EPOCH);
    assert!(t.accessed.is_none());
    assert!(t.created.is_none());
}

// ---------------------------------------------------------------------------
// NodeKind
// ---------------------------------------------------------------------------

#[test]
fn test_node_kind_discrimination() {
    let file_node = NodeKind::File { executable: false };
    assert!(file_node.is_file());
    assert!(!file_node.is_dir());
    assert!(!file_node.is_symlink());

    let dir_node = NodeKind::Directory {
        file_count: 10,
        dir_count: 5,
    };
    assert!(dir_node.is_dir());
    assert!(!dir_node.is_file());
    assert!(!dir_node.is_symlink());

    let symlink_node = NodeKind::Symlink {
        target: "target/path".into(),
        broken: false,
    };
    assert!(symlink_node.is_symlink());
    assert!(!symlink_node.is_file());
    assert!(!symlink_node.is_dir());

    let other_node = NodeKind::Other;
    assert!(!other_node.is_file());
    assert!(!other_node.is_dir());
    assert!(!other_node.is_symlink());
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
    assert_eq!(
        format!(
            "{}",
            NodeKind::Symlink {
                target: "x".into(),
                broken: false
            }
        ),
        "Symlink"
    );
    assert_eq!(
        format!(
            "{}",
            NodeKind::Symlink {
                target: "x".into(),
                broken: true
            }
        ),
        "Broken Symlink"
    );
    assert_eq!(format!("{}", NodeKind::Other), "Other");
}

// ---------------------------------------------------------------------------
// GitStatus
// ---------------------------------------------------------------------------

#[test]
fn test_git_status_display() {
    assert_eq!(format!("{}", GitStatus::Modified), "Modified");
    assert_eq!(format!("{}", GitStatus::Staged), "Staged");
    assert_eq!(format!("{}", GitStatus::Untracked), "Untracked");
    assert_eq!(format!("{}", GitStatus::Ignored), "Ignored");
    assert_eq!(format!("{}", GitStatus::Conflict), "Conflict");
    assert_eq!(format!("{}", GitStatus::Clean), "Clean");
}

#[test]
fn test_git_status_indicator() {
    assert_eq!(GitStatus::Modified.indicator(), "M");
    assert_eq!(GitStatus::Staged.indicator(), "A");
    assert_eq!(GitStatus::Untracked.indicator(), "?");
    assert_eq!(GitStatus::Ignored.indicator(), "!");
    assert_eq!(GitStatus::Conflict.indicator(), "C");
    assert_eq!(GitStatus::Clean.indicator(), " ");
}

#[test]
fn test_git_status_is_displayable() {
    assert!(GitStatus::Modified.is_displayable());
    assert!(GitStatus::Staged.is_displayable());
    assert!(GitStatus::Untracked.is_displayable());
    assert!(GitStatus::Ignored.is_displayable());
    assert!(GitStatus::Conflict.is_displayable());
    assert!(!GitStatus::Clean.is_displayable());
}

#[test]
fn test_git_status_default() {
    let status: GitStatus = Default::default();
    assert_eq!(status, GitStatus::Clean);
}

#[test]
fn test_git_status_hash() {
    use std::collections::HashSet;
    let statuses: HashSet<GitStatus> = [
        GitStatus::Modified,
        GitStatus::Staged,
        GitStatus::Clean,
        GitStatus::Clean, // duplicate
    ]
    .into();
    assert_eq!(statuses.len(), 3);
}

// ---------------------------------------------------------------------------
// FileNode — creation
// ---------------------------------------------------------------------------

#[test]
fn test_file_node_creation_and_properties() {
    let now = SystemTime::now();
    let node = FileNode::new_file(
        NodeId::new(1),
        "test.txt",
        2048,
        4,
        Timestamps::with_modified(now),
        true,
    );

    assert!(node.is_file());
    assert!(!node.is_dir());
    assert_eq!(node.name.as_str(), "test.txt");
    assert_eq!(node.size, 2048);
    assert_eq!(node.blocks, 4);
    assert_eq!(node.child_count(), 0);
    assert_eq!(node.file_count(), 1);
    assert_eq!(node.dir_count(), 0);

    match &node.kind {
        NodeKind::File { executable } => assert!(executable),
        _ => panic!("Expected File node kind"),
    }
}

#[test]
fn test_directory_node_creation_and_properties() {
    let now = SystemTime::now();
    let mut node =
        FileNode::new_directory(NodeId::new(1), "test_dir", Timestamps::with_modified(now));

    assert!(node.is_dir());
    assert!(!node.is_file());
    assert_eq!(node.name.as_str(), "test_dir");
    assert_eq!(node.size, 0);
    assert_eq!(node.blocks, 0);
    assert_eq!(node.file_count(), 0);
    assert_eq!(node.dir_count(), 0);

    let child1 = FileNode::new_file(
        NodeId::new(2),
        "file1.txt",
        1024,
        2,
        Timestamps::with_modified(now),
        false,
    );
    let mut child_dir =
        FileNode::new_directory(NodeId::new(3), "subdir", Timestamps::with_modified(now));
    let subfile = FileNode::new_file(
        NodeId::new(5),
        "subfile.txt",
        256,
        1,
        Timestamps::with_modified(now),
        false,
    );
    child_dir.children.push(subfile);
    node.children.push(child1);
    node.children.push(child_dir);

    node.update_counts();
    assert_eq!(node.file_count(), 2);
    assert_eq!(node.dir_count(), 1);

    let child2 = FileNode::new_file(
        NodeId::new(4),
        "file2.txt",
        512,
        1,
        Timestamps::with_modified(now),
        false,
    );
    node.children.push(child2);
    node.sort_children_by_size();

    assert!(node.children[0].size >= node.children[1].size);
    assert!(node.children[1].size >= node.children[2].size);
}

#[test]
fn test_file_node_with_inode() {
    let now = SystemTime::now();
    let mut node = FileNode::new_file(
        NodeId::new(1),
        "hardlink.txt",
        4096,
        8,
        Timestamps::with_modified(now),
        false,
    );

    node.inode = Some(InodeInfo::new(999, 888));

    assert!(node.inode.is_some());
    let inode = node.inode.unwrap();
    assert_eq!(inode.inode, 999);
    assert_eq!(inode.device, 888);
}

#[test]
fn test_file_node_with_content_hash() {
    let now = SystemTime::now();
    let mut node = FileNode::new_file(
        NodeId::new(1),
        "file.txt",
        1024,
        2,
        Timestamps::with_modified(now),
        false,
    );

    let hash_bytes = [0xde; 32];
    node.content_hash = Some(ContentHash::new(hash_bytes));

    assert!(node.content_hash.is_some());
    let hash = node.content_hash.unwrap();
    assert_eq!(hash.to_hex().len(), 64);
    assert_eq!(hash.as_bytes(), &hash_bytes);
}

// ---------------------------------------------------------------------------
// FileNode — update_counts
// ---------------------------------------------------------------------------

#[test]
fn test_update_counts_recursive() {
    let now = SystemTime::now();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));
    let mut dir1 = FileNode::new_directory(NodeId::new(2), "dir1", Timestamps::with_modified(now));
    let mut dir2 = FileNode::new_directory(NodeId::new(3), "dir2", Timestamps::with_modified(now));
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

    // Single call should handle the full subtree (post-order).
    root.update_counts();

    assert_eq!(root.file_count(), 2); // f1 + f2
    assert_eq!(root.dir_count(), 2); // dir1 + dir2
}

#[test]
fn test_update_counts_includes_symlinks_and_other() {
    let now = SystemTime::now();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));

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
    let other = FileNode {
        id: NodeId::new(4),
        name: "sock".into(),
        kind: NodeKind::Other,
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
    root.children.push(other);
    root.update_counts();

    // file + symlink + other all count toward file_count
    assert_eq!(root.file_count(), 3);
    assert_eq!(root.dir_count(), 0);
}

#[test]
fn test_update_counts_empty_dir() {
    let now = SystemTime::now();
    let mut dir = FileNode::new_directory(NodeId::new(1), "empty", Timestamps::with_modified(now));
    dir.update_counts();
    assert_eq!(dir.file_count(), 0);
    assert_eq!(dir.dir_count(), 0);
}

#[test]
fn test_update_counts_noop_on_file() {
    let now = SystemTime::now();
    let mut file = FileNode::new_file(
        NodeId::new(1),
        "f",
        512,
        1,
        Timestamps::with_modified(now),
        false,
    );
    // Should not panic or change anything.
    file.update_counts();
    assert_eq!(file.file_count(), 1);
    assert_eq!(file.dir_count(), 0);
}

// ---------------------------------------------------------------------------
// FileNode — sort_children_by_size
// ---------------------------------------------------------------------------

#[test]
fn test_sort_children_by_size_descending() {
    let now = SystemTime::now();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));
    let small = FileNode::new_file(
        NodeId::new(2),
        "small",
        100,
        1,
        Timestamps::with_modified(now),
        false,
    );
    let large = FileNode::new_file(
        NodeId::new(3),
        "large",
        999,
        1,
        Timestamps::with_modified(now),
        false,
    );
    let medium = FileNode::new_file(
        NodeId::new(4),
        "medium",
        500,
        1,
        Timestamps::with_modified(now),
        false,
    );
    root.children.extend([small, large, medium]);
    root.sort_children_by_size();

    assert_eq!(root.children[0].size, 999);
    assert_eq!(root.children[1].size, 500);
    assert_eq!(root.children[2].size, 100);
}

#[test]
fn test_sort_children_by_size_deterministic_tie_break() {
    let now = SystemTime::now();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));
    let b = FileNode::new_file(
        NodeId::new(2),
        "bbb",
        100,
        1,
        Timestamps::with_modified(now),
        false,
    );
    let a = FileNode::new_file(
        NodeId::new(3),
        "aaa",
        100,
        1,
        Timestamps::with_modified(now),
        false,
    );
    root.children.extend([b, a]);
    root.sort_children_by_size();

    // Equal size: secondary sort is name ascending.
    assert_eq!(root.children[0].name.as_str(), "aaa");
    assert_eq!(root.children[1].name.as_str(), "bbb");
}

// ---------------------------------------------------------------------------
// FileNode — finalize
// ---------------------------------------------------------------------------

#[test]
fn test_finalize_updates_then_sorts() {
    let now = SystemTime::now();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));

    let mut dir = FileNode::new_directory(NodeId::new(2), "dir", Timestamps::with_modified(now));
    dir.size = 2000;
    let f_inside = FileNode::new_file(
        NodeId::new(3),
        "big",
        2000,
        1,
        Timestamps::with_modified(now),
        false,
    );
    dir.children.push(f_inside);

    let small = FileNode::new_file(
        NodeId::new(4),
        "small",
        10,
        1,
        Timestamps::with_modified(now),
        false,
    );

    root.children.extend([small, dir]);
    root.finalize();

    // Counts should be updated.
    assert_eq!(root.file_count(), 2); // small + big
    assert_eq!(root.dir_count(), 1); // dir

    // Largest child first.
    assert_eq!(root.children[0].name.as_str(), "dir");
    assert_eq!(root.children[1].name.as_str(), "small");
}

// ---------------------------------------------------------------------------
// FileNode — nested structure (existing integration test)
// ---------------------------------------------------------------------------

#[test]
fn test_nested_directory_structure() {
    let now = SystemTime::now();

    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));

    let mut dir1 = FileNode::new_directory(NodeId::new(2), "dir1", Timestamps::with_modified(now));
    let file1 = FileNode::new_file(
        NodeId::new(3),
        "file1.txt",
        512,
        1,
        Timestamps::with_modified(now),
        false,
    );
    dir1.children.push(file1);
    dir1.size = 512;

    let mut dir2 = FileNode::new_directory(NodeId::new(4), "dir2", Timestamps::with_modified(now));
    let file2 = FileNode::new_file(
        NodeId::new(5),
        "file2.txt",
        1024,
        2,
        Timestamps::with_modified(now),
        false,
    );
    dir2.children.push(file2);
    dir2.size = 1024;

    root.children.push(dir1);
    root.children.push(dir2);
    root.update_counts();

    assert_eq!(root.file_count(), 2);
    assert_eq!(root.dir_count(), 2);

    root.sort_children_by_size();
    assert_eq!(root.children[0].name.as_str(), "dir2");
    assert_eq!(root.children[1].name.as_str(), "dir1");
}

// ---------------------------------------------------------------------------
// FileNode — symlink
// ---------------------------------------------------------------------------

#[test]
fn test_symlink_node() {
    let now = SystemTime::now();
    let node = FileNode {
        id: NodeId::new(1),
        name: "symlink".into(),
        kind: NodeKind::Symlink {
            target: "/path/to/target".into(),
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

    assert!(matches!(node.kind, NodeKind::Symlink { .. }));
    match &node.kind {
        NodeKind::Symlink { target, broken } => {
            assert_eq!(target.as_str(), "/path/to/target");
            assert!(!broken);
        }
        _ => panic!("Expected Symlink node kind"),
    }

    let broken_node = FileNode {
        id: NodeId::new(2),
        name: "broken_symlink".into(),
        kind: NodeKind::Symlink {
            target: "/nonexistent".into(),
            broken: true,
        },
        size: 0,
        blocks: 0,
        timestamps: Timestamps::with_modified(now),
        inode: None,
        content_hash: None,
        git_status: None,
        children: Vec::new(),
    };

    match &broken_node.kind {
        NodeKind::Symlink { broken, .. } => assert!(broken),
        _ => panic!("Expected Symlink node kind"),
    }
}

// ---------------------------------------------------------------------------
// ScanConfig
// ---------------------------------------------------------------------------

#[test]
fn test_scan_config_builder() {
    let config = ScanConfig::builder()
        .root("/test/path")
        .max_depth(Some(5))
        .include_hidden(true)
        .follow_symlinks(false)
        .cross_filesystems(true)
        .ignore_patterns(vec!["*.tmp".to_string(), ".DS_Store".to_string()])
        .build()
        .unwrap();

    assert_eq!(config.root.to_str().unwrap(), "/test/path");
    assert_eq!(config.max_depth, Some(5));
    assert!(config.include_hidden);
    assert!(!config.follow_symlinks);
    assert!(config.cross_filesystems);

    assert!(config.should_ignore("file.tmp"));
    assert!(config.should_ignore(".DS_Store"));
    assert!(!config.should_ignore("normal.txt"));

    let default_config = ScanConfig::new("/default");
    assert_eq!(default_config.root.to_str().unwrap(), "/default");
    assert_eq!(default_config.max_depth, None);
    assert!(default_config.include_hidden);
    assert!(!default_config.follow_symlinks);
    assert!(!default_config.cross_filesystems);
}

/// Builder-produced configs do not call compile_patterns automatically.
/// should_ignore must still work via the fallback path.
#[test]
fn test_scan_config_builder_ignore_works_without_compile() {
    let config = ScanConfig::builder()
        .root("/test")
        .ignore_patterns(vec!["*.log".to_string(), "node_modules".to_string()])
        .build()
        .unwrap();

    // No compile_patterns() called — fallback path must handle this correctly.
    assert!(config.should_ignore("app.log"));
    assert!(config.should_ignore("node_modules"));
    assert!(!config.should_ignore("main.rs"));
}

#[test]
fn test_scan_config_new_ignore_works_immediately() {
    // ScanConfig::new calls compile_patterns internally.
    let mut config = ScanConfig::new("/test");
    config.ignore_patterns = vec!["*.bak".to_string()];
    config.compile_patterns();

    assert!(config.should_ignore("file.bak"));
    assert!(!config.should_ignore("file.rs"));
}

#[test]
fn test_scan_config_compile_patterns_glob_wildcards() {
    let mut config = ScanConfig::builder()
        .root("/test")
        .ignore_patterns(vec![
            "node_modules".to_string(),
            "*.log".to_string(),
            "**/*.tmp".to_string(),
        ])
        .build()
        .unwrap();
    config.compile_patterns();

    assert!(config.should_ignore("node_modules"));
    assert!(config.should_ignore("test.log"));
    assert!(config.should_ignore("cache.tmp"));
    assert!(!config.should_ignore("src"));
    assert!(!config.should_ignore("test.txt"));
}

#[test]
fn test_scan_config_compile_patterns_prefix_glob() {
    let mut config = ScanConfig::builder()
        .root("/test")
        .ignore_patterns(vec!["build*".to_string()])
        .build()
        .unwrap();
    config.compile_patterns();

    assert!(config.should_ignore("build"));
    assert!(config.should_ignore("build-output"));
    assert!(!config.should_ignore("rebuild"));
}

#[test]
fn test_scan_config_compile_patterns_empty() {
    let mut config = ScanConfig::new("/test");
    config.ignore_patterns = vec![];
    config.compile_patterns();

    assert!(!config.should_ignore("anything"));
}

#[test]
fn test_scan_config_should_skip_hidden() {
    let mut config = ScanConfig::new("/test");

    assert!(!config.should_skip_hidden(".git")); // include_hidden = true by default
    config.include_hidden = false;
    assert!(config.should_skip_hidden(".git"));
    assert!(config.should_skip_hidden(".env"));
    assert!(!config.should_skip_hidden("src"));
}

#[test]
fn test_scan_config_builder_missing_root_errors() {
    let result = ScanConfig::builder().build();
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// ScanError
// ---------------------------------------------------------------------------

#[test]
fn test_scan_error_io_dispatch() {
    let perm = ScanError::io(
        "/test/path",
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
    );
    assert!(matches!(perm, ScanError::PermissionDenied { .. }));

    let not_found = ScanError::io(
        "/test/path",
        std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
    );
    assert!(matches!(not_found, ScanError::NotFound { .. }));

    let other = ScanError::io(
        "/test/path",
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken"),
    );
    assert!(matches!(other, ScanError::Io { .. }));
}

#[test]
fn test_scan_error_preserves_source_kind() {
    let err = ScanError::io(
        "/test/path",
        std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
    );
    match err {
        ScanError::NotFound { source, .. } => {
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        _ => panic!("Expected NotFound variant"),
    }
}

#[test]
fn test_scan_error_preserves_permission_denied_source() {
    let err = ScanError::io(
        "/secret",
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope"),
    );
    match err {
        ScanError::PermissionDenied { path, source } => {
            assert_eq!(path.to_str().unwrap(), "/secret");
            assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        }
        _ => panic!("Expected PermissionDenied variant"),
    }
}

#[test]
fn test_scan_error_display() {
    let err = ScanError::io(
        "/a/b",
        std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
    );
    let msg = format!("{err}");
    assert!(msg.contains("/a/b"), "display should include path: {msg}");
}

// ---------------------------------------------------------------------------
// ScanWarning
// ---------------------------------------------------------------------------

#[test]
fn test_scan_warning_permission_denied() {
    let warning = ScanWarning::permission_denied("/test/path");
    assert_eq!(warning.kind, WarningKind::PermissionDenied);
    assert!(warning.message.contains("Permission denied"));
    // Display impl delegates to message field.
    assert_eq!(format!("{warning}"), warning.message);
}

#[test]
fn test_scan_warning_broken_symlink() {
    let warning = ScanWarning::broken_symlink("/link", "/target");
    assert_eq!(warning.kind, WarningKind::BrokenSymlink);
    assert!(warning.message.contains("Broken symlink"));
    assert!(warning.message.contains("/link"));
    assert!(warning.message.contains("/target"));
}

#[test]
fn test_scan_warning_new_parameter_order() {
    // Signature: new(path, kind, message) — kind before message.
    let warning = ScanWarning::new("/some/path", WarningKind::ReadError, "custom message");
    assert_eq!(warning.kind, WarningKind::ReadError);
    assert_eq!(warning.message, "custom message");
    assert_eq!(warning.path.to_str().unwrap(), "/some/path");
}

#[test]
fn test_scan_warning_display_matches_message() {
    let warning = ScanWarning::new("/p", WarningKind::MetadataError, "stat failed");
    assert_eq!(format!("{warning}"), "stat failed");
}

// ---------------------------------------------------------------------------
// TreeStats
// ---------------------------------------------------------------------------

#[test]
fn test_tree_stats_default() {
    let stats = TreeStats::default();
    assert_eq!(stats.total_size, 0);
    assert_eq!(stats.total_files, 0);
    assert_eq!(stats.total_dirs, 0);
    assert_eq!(stats.total_symlinks, 0);
}

#[test]
fn test_tree_stats_record_file() {
    let mut stats = TreeStats::new();
    let now = SystemTime::now();

    stats.record_file(std::path::Path::new("/test/file.txt"), 1024, now, 2);

    assert_eq!(stats.total_files, 1);
    assert_eq!(stats.total_size, 1024);
    assert_eq!(stats.max_depth, 2);
    assert!(stats.largest_file.is_some());
}

#[test]
fn test_tree_stats_record_file_tracks_extremes() {
    let mut stats = TreeStats::new();
    let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);

    stats.record_file(std::path::Path::new("/small_old"), 100, t1, 1);
    stats.record_file(std::path::Path::new("/big_new"), 999, t2, 2);

    assert_eq!(stats.largest_file.as_ref().unwrap().1, 999);
    assert_eq!(stats.oldest_file.as_ref().unwrap().1, t1);
    assert_eq!(stats.newest_file.as_ref().unwrap().1, t2);
    assert_eq!(stats.max_depth, 2);
    assert_eq!(stats.total_size, 1099);
}

#[test]
fn test_tree_stats_record_dir_and_symlink() {
    let mut stats = TreeStats::new();
    stats.record_dir(3);
    stats.record_dir(5);
    stats.record_symlink();

    assert_eq!(stats.total_dirs, 2);
    assert_eq!(stats.total_symlinks, 1);
    assert_eq!(stats.max_depth, 5);
}

// ---------------------------------------------------------------------------
// FileTree
// ---------------------------------------------------------------------------

#[test]
fn test_file_tree_accessors() {
    let now = SystemTime::now();
    let root = FileNode::new_file(
        NodeId::new(1),
        "file.txt",
        4096,
        8,
        Timestamps::with_modified(now),
        false,
    );

    let mut stats = TreeStats::new();
    stats.record_file(std::path::Path::new("/file.txt"), 4096, now, 0);

    let tree = FileTree::new(
        root,
        std::path::PathBuf::from("/"),
        ScanConfig::new("/"),
        stats,
        Duration::from_millis(10),
        vec![],
    );

    assert_eq!(tree.total_size(), 4096);
    assert_eq!(tree.total_files(), 1);
    assert_eq!(tree.total_dirs(), 0);
    assert!(!tree.has_warnings());
}

#[test]
fn test_file_tree_has_warnings() {
    let now = SystemTime::now();
    let root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));

    let tree = FileTree::new(
        root,
        std::path::PathBuf::from("/"),
        ScanConfig::new("/"),
        TreeStats::new(),
        Duration::ZERO,
        vec![ScanWarning::permission_denied("/secret")],
    );

    assert!(tree.has_warnings());
}
