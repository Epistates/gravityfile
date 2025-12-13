use gravityfile_analyze::{DuplicateConfig, DuplicateFinder};
use gravityfile_core::{FileNode, FileTree, NodeId, ScanConfig, Timestamps};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_duplicate_config_builder() {
    let config = DuplicateConfig::builder()
        .min_size(2048u64)
        .max_size(10 * 1024 * 1024u64)
        .quick_compare(false)
        .partial_hash_head(8192usize)
        .partial_hash_tail(8192usize)
        .exclude_patterns(vec!["*.log".to_string(), "temp/".to_string()])
        .max_groups(5usize)
        .build()
        .unwrap();

    assert_eq!(config.min_size, 2048);
    assert_eq!(config.max_size, 10 * 1024 * 1024);
    assert!(!config.quick_compare);
    assert_eq!(config.partial_hash_head, 8192);
    assert_eq!(config.partial_hash_tail, 8192);
    assert_eq!(config.exclude_patterns.len(), 2);
    assert_eq!(config.max_groups, 5);

    // Test default config
    let default_config = DuplicateConfig::default();
    assert_eq!(default_config.min_size, 1024);
    assert_eq!(default_config.quick_compare, true);
}

#[test]
fn test_duplicate_group_properties() {
    use std::path::PathBuf;

    let paths = vec![
        PathBuf::from("/path/file1.txt"),
        PathBuf::from("/path/file2.txt"),
        PathBuf::from("/other/file3.txt"),
    ];

    let hash_bytes = [0xaa; 32];
    let group = gravityfile_analyze::DuplicateGroup {
        hash: gravityfile_core::ContentHash::new(hash_bytes),
        size: 4096,
        paths: paths.clone(),
        wasted_bytes: 8192, // 4096 * (3 - 1)
    };

    assert_eq!(group.count(), 3);
    assert_eq!(group.deletable_count(), 2); // Can delete 2, keep 1
    assert_eq!(group.size, 4096);
    assert_eq!(group.wasted_bytes, 8192);

    let hash_hex = group.hash.to_hex();
    assert_eq!(hash_hex.len(), 64);
}

#[test]
fn test_duplicate_report_properties() {
    use std::path::PathBuf;

    let mut report = gravityfile_analyze::DuplicateReport {
        groups: Vec::new(),
        total_duplicate_size: 0,
        total_wasted_space: 0,
        files_analyzed: 100,
        files_with_duplicates: 20,
        group_count: 5,
    };

    assert!(!report.has_duplicates());
    assert_eq!(report.total_duplicate_files(), 0);

    // Add a duplicate group
    let paths = vec![PathBuf::from("/file1.txt"), PathBuf::from("/file2.txt")];
    let hash_bytes = [0xbb; 32];
    let group = gravityfile_analyze::DuplicateGroup {
        hash: gravityfile_core::ContentHash::new(hash_bytes),
        size: 4096,
        paths,
        wasted_bytes: 4096, // 4096 * (2 - 1)
    };
    report.groups.push(group);

    assert!(report.has_duplicates());
    assert_eq!(report.total_duplicate_files(), 2);
}

#[test]
fn test_find_duplicates_with_empty_tree() {
    let finder = DuplicateFinder::new();

    // Create an empty tree
    let now = std::time::SystemTime::now();
    let root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));
    let config = ScanConfig::new("/test");
    let stats = gravityfile_core::TreeStats::default();

    let tree = FileTree::new(
        root,
        std::path::PathBuf::from("/test"),
        config,
        stats,
        std::time::Duration::from_secs(0),
        Vec::new(),
    );

    let report = finder.find_duplicates(&tree);

    assert_eq!(report.files_analyzed, 0);
    assert!(!report.has_duplicates());
}

#[test]
fn test_find_duplicates_with_no_actual_duplicates() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Create files with different content
    fs::write(root.join("file1.txt"), "content one").unwrap();
    fs::write(root.join("file2.txt"), "content two").unwrap();
    fs::write(root.join("file3.txt"), "content three").unwrap();

    let finder = DuplicateFinder::new();
    let mut tree = create_test_tree_with_files(&[root.join("file1.txt"), root.join("file2.txt"), root.join("file3.txt")]);

    let report = finder.find_duplicates(&tree);

    assert_eq!(report.files_analyzed, 3);
    assert!(!report.has_duplicates());
}

#[test]
fn test_find_duplicates_with_exact_duplicates() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Create duplicate files
    let content = "This is duplicate content";
    fs::write(root.join("file1.txt"), content).unwrap();
    fs::write(root.join("file2.txt"), content).unwrap();
    fs::write(root.join("file3.txt"), content).unwrap();

    let finder = DuplicateFinder::new();
    let mut tree = create_test_tree_with_files(&[root.join("file1.txt"), root.join("file2.txt"), root.join("file3.txt")]);

    let report = finder.find_duplicates(&tree);

    assert_eq!(report.files_analyzed, 3);
    assert!(report.has_duplicates());
    assert_eq!(report.group_count, 1);
    assert_eq!(report.total_duplicate_files(), 3);
    assert_eq!(report.total_wasted_space, content.len() as u64 * 2); // 3 files - 1 kept = 2 wasted
}

#[test]
fn test_find_duplicates_with_mixed_content() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Create mix of duplicate and unique files
    fs::write(root.join("file1.txt"), "duplicate").unwrap();
    fs::write(root.join("file2.txt"), "unique one").unwrap();
    fs::write(root.join("file3.txt"), "duplicate").unwrap();
    fs::write(root.join("file4.txt"), "unique two").unwrap();

    let finder = DuplicateFinder::new();
    let mut tree = create_test_tree_with_files(&[
        root.join("file1.txt"),
        root.join("file2.txt"),
        root.join("file3.txt"),
        root.join("file4.txt"),
    ]);

    let report = finder.find_duplicates(&tree);

    assert_eq!(report.files_analyzed, 4);
    assert!(report.has_duplicates());
    assert_eq!(report.group_count, 1); // Only one duplicate group
    assert_eq!(report.total_duplicate_files(), 3); // file1, file2, file3 (file2 is unique)
}

#[test]
fn test_find_duplicates_with_exclusion_patterns() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Create duplicate files
    fs::write(root.join("file1.txt"), "duplicate").unwrap();
    fs::write(root.join("file2.txt"), "duplicate").unwrap();
    fs::write(root.join(".hidden_file"), "duplicate").unwrap();

    let config = DuplicateConfig::builder()
        .exclude_patterns(vec!["*.txt".to_string()])
        .build()
        .unwrap();

    let finder = DuplicateFinder::with_config(config);
    let mut tree = create_test_tree_with_files(&[
        root.join("file1.txt"),
        root.join("file2.txt"),
        root.join(".hidden_file"),
    ]);

    let report = finder.find_duplicates(&tree);

    // .txt files should be excluded, only .hidden_file remains
    assert_eq!(report.files_analyzed, 1);
    assert!(!report.has_duplicates());
}

#[test]
fn test_find_duplicates_with_size_filtering() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Create files of different sizes
    fs::write(root.join("small.txt"), "a").unwrap(); // 1 byte
    fs::write(root.join("medium.txt"), "duplicate content").unwrap(); // larger
    fs::write(root.join("large.txt"), "duplicate content").unwrap(); // same as medium

    let config = DuplicateConfig::builder()
        .min_size(2u64) // Exclude small files
        .build()
        .unwrap();

    let finder = DuplicateFinder::with_config(config);
    let mut tree = create_test_tree_with_files(&[
        root.join("small.txt"),
        root.join("medium.txt"),
        root.join("large.txt"),
    ]);

    let report = finder.find_duplicates(&tree);

    // Only medium and large should be analyzed
    assert_eq!(report.files_analyzed, 2);
    assert!(report.has_duplicates());
}

#[test]
fn test_find_duplicates_with_max_groups_limit() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Create multiple duplicate groups
    fs::write(root.join("group1_a.txt"), "content A").unwrap();
    fs::write(root.join("group1_b.txt"), "content A").unwrap();
    fs::write(root.join("group2_a.txt"), "content B").unwrap();
    fs::write(root.join("group2_b.txt"), "content B").unwrap();
    fs::write(root.join("group3_a.txt"), "content C").unwrap();
    fs::write(root.join("group3_b.txt"), "content C").unwrap();

    let config = DuplicateConfig::builder()
        .max_groups(2usize) // Only return top 2 groups
        .build()
        .unwrap();

    let finder = DuplicateFinder::with_config(config);
    let mut tree = create_test_tree_with_files(&[
        root.join("group1_a.txt"),
        root.join("group1_b.txt"),
        root.join("group2_a.txt"),
        root.join("group2_b.txt"),
        root.join("group3_a.txt"),
        root.join("group3_b.txt"),
    ]);

    let report = finder.find_duplicates(&tree);

    assert_eq!(report.group_count, 2);
}

#[test]
fn test_find_duplicates_in_nested_structure() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // Create nested directory structure with duplicates
    fs::create_dir(root.join("dir1")).unwrap();
    fs::create_dir(root.join("dir2")).unwrap();
    fs::write(root.join("dir1/file.txt"), "duplicate").unwrap();
    fs::write(root.join("dir2/file.txt"), "duplicate").unwrap();

    let finder = DuplicateFinder::new();
    let mut tree = create_test_tree_with_nested_files(&[
        (root.join("dir1/file.txt"), "file.txt"),
        (root.join("dir2/file.txt"), "file.txt"),
    ]);

    let report = finder.find_duplicates(&tree);

    assert_eq!(report.files_analyzed, 2);
    assert!(report.has_duplicates());
    assert_eq!(report.group_count, 1);
}

fn create_test_tree_with_files(paths: &[std::path::PathBuf]) -> FileTree {
    let now = std::time::SystemTime::now();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));

    for (i, path) in paths.iter().enumerate() {
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        let metadata = fs::metadata(path).unwrap();
        let size = metadata.len();

        let mut node = FileNode::new_file(
            NodeId::new((i + 2) as u64),
            &file_name,
            size,
            (size / 512) as u64, // Approximate blocks
            Timestamps::with_modified(now),
            false,
        );

        // Read actual content for hashing
        if let Ok(content) = fs::read(path) {
            use blake3::Hasher;
            let mut hasher = Hasher::new();
            hasher.update(&content);
            node.content_hash = Some(gravityfile_core::ContentHash::new(*hasher.finalize().as_bytes()));
        }

        root.children.push(node);
    }

    // Update directory counts
    root.update_counts();

    let config = ScanConfig::new("/test");
    let mut stats = gravityfile_core::TreeStats::default();
    for path in paths {
        if let Ok(metadata) = fs::metadata(path) {
            stats.record_file(
                path.to_path_buf(),
                metadata.len(),
                now,
                1, // depth
            );
        }
    }

    FileTree::new(
        root,
        std::path::PathBuf::from("/test"),
        config,
        stats,
        std::time::Duration::from_secs(0),
        Vec::new(),
    )
}

fn create_test_tree_with_nested_files(paths: &[(std::path::PathBuf, &str)]) -> FileTree {
    let now = std::time::SystemTime::now();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));

    for (i, (path, name)) in paths.iter().enumerate() {
        let metadata = fs::metadata(path).unwrap();
        let size = metadata.len();

        // Create parent directories
        let mut current = &mut root;
        for parent_name in path.parent().unwrap().components().skip(1) {
            let parent_name_str = parent_name.as_os_str().to_string_lossy().to_string();
            if !current.children.iter().any(|c| c.name == parent_name_str) {
                let mut dir_node = FileNode::new_directory(
                    NodeId::new(i as u64 * 10 + current.children.len() as u64 + 2),
                    &parent_name_str,
                    Timestamps::with_modified(now),
                );
                current.children.push(dir_node);
            }
            current = current.children.iter_mut().find(|c| c.name == parent_name_str).unwrap();
        }

        // Add the file
        let mut node = FileNode::new_file(
            NodeId::new(i as u64 + 2),
            name.to_string(),
            size,
            (size / 512) as u64,
            Timestamps::with_modified(now),
            false,
        );

        if let Ok(content) = fs::read(path) {
            use blake3::Hasher;
            let mut hasher = Hasher::new();
            hasher.update(&content);
            node.content_hash = Some(gravityfile_core::ContentHash::new(*hasher.finalize().as_bytes()));
        }

        current.children.push(node);
    }

    // Update directory counts recursively
    fn update_counts_recursive(node: &mut FileNode) {
        if node.is_dir() {
            for child in &mut node.children {
                update_counts_recursive(child);
            }
            node.update_counts();
        }
    }
    update_counts_recursive(&mut root);

    let config = ScanConfig::new("/test");
    let mut stats = gravityfile_core::TreeStats::default();
    for (path, _) in paths {
        if let Ok(metadata) = fs::metadata(path) {
            stats.record_file(
                path.to_path_buf(),
                metadata.len(),
                now,
                1, // depth
            );
        }
    }

    FileTree::new(
        root,
        std::path::PathBuf::from("/test"),
        config,
        stats,
        std::time::Duration::from_secs(0),
        Vec::new(),
    )
}
