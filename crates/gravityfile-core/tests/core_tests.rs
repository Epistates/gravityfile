use gravityfile_core::{
    ContentHash, FileNode, InodeInfo, NodeId, NodeKind, ScanConfig, Timestamps,
};
use std::time::SystemTime;

#[test]
fn test_node_id_operations() {
    let id1 = NodeId::new(42);
    let id2 = NodeId::new(42);

    assert_eq!(id1, id2);
    assert_eq!(id1.0, 42);
}

#[test]
fn test_content_hash_creation_and_hex() {
    let bytes = [0xab; 32];
    let hash = ContentHash::new(bytes);

    // Test hex conversion
    let hex = hash.to_hex();
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(hex.starts_with("ab"));

    // Test equality
    let hash2 = ContentHash::new(bytes);
    assert_eq!(hash, hash2);

    // Test inequality
    let different_bytes = [0xcd; 32];
    let hash3 = ContentHash::new(different_bytes);
    assert_ne!(hash, hash3);
}

#[test]
fn test_inode_info() {
    let inode1 = InodeInfo::new(12345, 67890);
    assert_eq!(inode1.inode, 12345);
    assert_eq!(inode1.device, 67890);

    let inode2 = InodeInfo::new(12345, 67890);
    assert_eq!(inode1, inode2);
}

#[test]
fn test_timestamps() {
    let now = SystemTime::now();
    let timestamps = Timestamps::with_modified(now);

    assert_eq!(timestamps.modified, now);
    assert!(timestamps.accessed.is_none());
    assert!(timestamps.created.is_none());

    // Test with all fields
    let accessed = now - std::time::Duration::from_secs(3600);
    let created = now - std::time::Duration::from_secs(7200);

    let full_timestamps = Timestamps::new(now, Some(accessed), Some(created));
    assert_eq!(full_timestamps.modified, now);
    assert_eq!(full_timestamps.accessed, Some(accessed));
    assert_eq!(full_timestamps.created, Some(created));
}

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
fn test_file_node_creation_and_properties() {
    let now = SystemTime::now();
    let node = FileNode::new_file(
        NodeId::new(1),
        "test.txt",
        2048,
        4,
        Timestamps::with_modified(now),
        true, // executable
    );

    assert!(node.is_file());
    assert!(!node.is_dir());
    assert_eq!(node.name.as_str(), "test.txt");
    assert_eq!(node.size, 2048);
    assert_eq!(node.blocks, 4);
    assert_eq!(node.child_count(), 0);

    // Test file_count for files
    assert_eq!(node.file_count(), 1);
    assert_eq!(node.dir_count(), 0);

    // Verify it's executable
    match &node.kind {
        NodeKind::File { executable } => {
            assert!(executable);
        }
        _ => panic!("Expected File node kind"),
    }
}

#[test]
fn test_directory_node_creation_and_properties() {
    let now = SystemTime::now();
    let mut node = FileNode::new_directory(
        NodeId::new(1),
        "test_dir",
        Timestamps::with_modified(now),
    );

    assert!(node.is_dir());
    assert!(!node.is_file());
    assert_eq!(node.name.as_str(), "test_dir");
    assert_eq!(node.size, 0);
    assert_eq!(node.blocks, 0);

    // Test file_count and dir_count for empty directory
    assert_eq!(node.file_count(), 0);
    assert_eq!(node.dir_count(), 0);

    // Add some children to test count updates
    let child1 = FileNode::new_file(
        NodeId::new(2),
        "file1.txt",
        1024,
        2,
        Timestamps::with_modified(now),
        false,
    );
    let mut child_dir = FileNode::new_directory(NodeId::new(3), "subdir", Timestamps::with_modified(now));
    child_dir.kind = NodeKind::Directory {
        file_count: 1,
        dir_count: 0,
    };
    node.children.push(child1);
    node.children.push(child_dir);

    // Update counts
    node.update_counts();
    assert_eq!(node.file_count(), 2); // 1 direct + 1 in subdir
    assert_eq!(node.dir_count(), 1); // 1 subdir

    // Test sorting by size
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

    // Children should be sorted by size descending
    assert!(node.children[0].size >= node.children[1].size);
    assert!(node.children[1].size >= node.children[2].size);
}

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

    // Test ignore patterns
    assert!(config.should_ignore("file.tmp"));
    assert!(config.should_ignore(".DS_Store"));
    assert!(!config.should_ignore("normal.txt"));

    // Test default config
    let default_config = ScanConfig::new("/default");
    assert_eq!(default_config.root.to_str().unwrap(), "/default");
    assert_eq!(default_config.max_depth, None);
    assert!(!default_config.include_hidden);
    assert!(default_config.follow_symlinks);
    assert!(!default_config.cross_filesystems);
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

    // Add inode info
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

    // Add content hash
    let hash_bytes = [0xde; 32];
    node.content_hash = Some(ContentHash::new(hash_bytes));

    assert!(node.content_hash.is_some());
    let hash = node.content_hash.unwrap();
    assert_eq!(hash.to_hex().len(), 64);
}

#[test]
fn test_nested_directory_structure() {
    let now = SystemTime::now();

    // Create nested structure
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

    root.children.push(dir1);
    root.children.push(dir2);

    // Update counts recursively
    for child in &mut root.children {
        if child.is_dir() {
            child.update_counts();
        }
    }

    assert_eq!(root.file_count(), 2); // Both files in subdirectories
    assert_eq!(root.dir_count(), 2); // dir1 and dir2

    // Test sorting
    root.sort_children_by_size();
    // dir2 should come first (contains larger file)
    assert_eq!(root.children[0].name.as_str(), "dir2");
    assert_eq!(root.children[1].name.as_str(), "dir1");
}

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

    // Test broken symlink
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
        children: Vec::new(),
    };

    match &broken_node.kind {
        NodeKind::Symlink { broken, .. } => {
            assert!(broken);
        }
        _ => panic!("Expected Symlink node kind"),
    }
}
