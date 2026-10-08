use gravityfile_analyze::{AgeAnalyzer, AgeBucket, AgeConfig, DuplicateConfig, DuplicateFinder};
use gravityfile_core::{FileNode, FileTree, NodeId, ScanConfig, Timestamps};
use std::fs;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[test]
fn filtered_scans_do_not_certify_stale_directories() {
    use gravityfile_analyze::StaleDirectoryStatus;
    let reference = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for hidden in [true, false] {
        let root = test_dir(
            0,
            "data",
            vec![test_file(1, "old", 100, SystemTime::UNIX_EPOCH)],
        );
        let mut tree = test_tree(std::path::Path::new("data"), root);
        if hidden {
            tree.config.include_hidden = false;
        } else {
            tree.config.ignore_patterns = vec!["recent/**".into()];
        }
        let analyzer = AgeAnalyzer::with_config(AgeConfig {
            reference_time: reference,
            min_stale_size: 0,
            stale_threshold: Duration::from_secs(1),
            ..Default::default()
        })
        .unwrap();
        let report = analyzer.analyze(&tree, CancellationToken::new());
        assert!(report.status.is_complete()); // Complete for the requested filtered scope.
        assert_eq!(report.total_files, 1);
        assert_eq!(
            report.stale_directory_status,
            StaleDirectoryStatus::SuppressedFilteredScan
        );
        assert!(report.stale_directories.is_empty());
        tree.config.include_hidden = true;
        tree.config.ignore_patterns.clear();
        let report = analyzer.analyze(&tree, CancellationToken::new());
        assert_eq!(
            report.stale_directory_status,
            StaleDirectoryStatus::Evaluated
        );
        assert!(!report.stale_directories.is_empty());
    }
}

#[test]
fn average_age_preserves_subsecond_observations() {
    let reference = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
    let root = test_dir(
        0,
        "data",
        vec![
            test_file(1, "a", 1, reference - Duration::from_millis(1900)),
            test_file(2, "b", 1, reference - Duration::from_millis(2900)),
        ],
    );
    let tree = test_tree(std::path::Path::new("data"), root);
    let report = AgeAnalyzer::with_config(AgeConfig {
        reference_time: reference,
        ..Default::default()
    })
    .unwrap()
    .analyze(&tree, CancellationToken::new());
    assert_eq!(report.average_age, Duration::from_millis(2400));
}

#[test]
fn age_configuration_rejects_ambiguous_or_incomplete_buckets() {
    let invalid = [
        vec![],
        vec![AgeBucket::new("Recent", Duration::from_secs(60))],
        vec![AgeBucket::new(" ", Duration::MAX)],
        vec![
            AgeBucket::new("Same", Duration::ZERO),
            AgeBucket::new("Same", Duration::MAX),
        ],
        vec![
            AgeBucket::new("One", Duration::MAX),
            AgeBucket::new("Two", Duration::MAX),
        ],
    ];
    for buckets in invalid {
        assert!(
            AgeConfig::builder()
                .buckets(buckets.clone())
                .build()
                .is_err()
        );
        let config = AgeConfig {
            buckets,
            ..Default::default()
        };
        assert!(config.validate().is_err());
        assert!(AgeAnalyzer::with_config(config).is_err());
    }
}

#[test]
fn valid_age_buckets_partition_every_file_including_boundary_ages() {
    let reference = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let root = test_dir(
        0,
        "data",
        vec![
            test_file(1, "new", 1, reference),
            test_file(2, "boundary", 2, reference - Duration::from_secs(60)),
            test_file(3, "older", 3, reference - Duration::from_secs(61)),
        ],
    );
    let tree = test_tree(std::path::Path::new("data"), root);
    let config = AgeConfig::builder()
        .reference_time(reference)
        .buckets(vec![
            AgeBucket::new("Older", Duration::MAX),
            AgeBucket::new("Recent", Duration::from_secs(60)),
        ])
        .build()
        .unwrap();
    let report = AgeAnalyzer::with_config(config)
        .unwrap()
        .analyze(&tree, CancellationToken::new());
    assert_eq!(report.total_files, 3);
    assert_eq!(report.buckets[0].file_count, 2);
    assert_eq!(report.buckets[1].file_count, 1);
    assert_eq!(
        report
            .buckets
            .iter()
            .map(|bucket| bucket.total_size)
            .sum::<u64>(),
        report.total_size
    );
    assert_eq!(report.median_age_bucket.as_deref(), Some("Recent"));
}

#[test]
fn shallow_scans_cannot_certify_stale_directories() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("old"), b"old").unwrap();
    fs::create_dir(root.path().join("unvisited")).unwrap();
    fs::write(root.path().join("unvisited/recent"), b"recent").unwrap();
    let tree = gravityfile_scan::quick_list(root.path(), None).unwrap();
    let analyzer = AgeAnalyzer::with_config(AgeConfig {
        min_stale_size: 0,
        stale_threshold: Duration::ZERO,
        ..Default::default()
    })
    .unwrap();
    let report = analyzer.analyze(&tree, CancellationToken::new());
    assert!(report.status.is_complete()); // Complete within its explicitly shallow scope.
    assert!(!report.has_stale_directories());
}

#[test]
fn scan_warnings_suppress_stale_directory_recommendations() {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("data"), b"data").unwrap();
    let mut tree = gravityfile_scan::JwalkScanner::new()
        .scan(&ScanConfig::new(directory.path()), CancellationToken::new())
        .unwrap();
    let analyzer = AgeAnalyzer::with_config(AgeConfig {
        stale_threshold: Duration::ZERO,
        min_stale_size: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(
        analyzer
            .analyze(&tree, CancellationToken::new())
            .has_stale_directories()
    );
    tree.warnings
        .push(gravityfile_core::ScanWarning::permission_denied(
            directory.path().join("unreadable"),
        ));
    let report = analyzer.analyze(&tree, CancellationToken::new());
    assert_eq!(
        report.status.state,
        gravityfile_analyze::AnalysisState::Partial
    );
    assert!(!report.has_stale_directories());
    assert_eq!(report.total_files, 1);
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        analyzer.analyze(&tree, token).status.state,
        gravityfile_analyze::AnalysisState::Cancelled
    );
}

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
    assert!(default_config.quick_compare);
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
        status: Default::default(),
        groups: Vec::new(),
        total_duplicate_size: 0,
        total_wasted_space: 0,
        files_analyzed: 100,
        files_with_duplicates: 20,
        group_count: 5,
        groups_omitted: 0,
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

    let report = finder.find_duplicates(&tree, CancellationToken::new());

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

    let config = DuplicateConfig::builder().min_size(1u64).build().unwrap();
    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_files(
        root,
        &[
            root.join("file1.txt"),
            root.join("file2.txt"),
            root.join("file3.txt"),
        ],
    );

    let report = finder.find_duplicates(&tree, CancellationToken::new());

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

    let config = DuplicateConfig::builder().min_size(1u64).build().unwrap();
    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_files(
        root,
        &[
            root.join("file1.txt"),
            root.join("file2.txt"),
            root.join("file3.txt"),
        ],
    );

    let report = finder.find_duplicates(&tree, CancellationToken::new());

    assert_eq!(report.files_analyzed, 3);
    assert!(report.has_duplicates());
    assert_eq!(report.group_count, 1);
    assert_eq!(report.total_duplicate_files(), 3);
    assert_eq!(report.total_wasted_space, content.len() as u64 * 2); // 3 files - 1 kept = 2 wasted
}

#[test]
fn test_find_duplicates_uses_logical_length_not_scanned_allocation_size() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let content = b"identical logical content";
    let first = root.join("first.bin");
    let second = root.join("second.bin");
    fs::write(&first, content).unwrap();
    fs::write(&second, content).unwrap();

    let mut tree = create_test_tree_with_files(root, &[first, second]);
    // Simulate a physical-size scan where equal logical files happened to
    // report different allocation sizes (sparse/compressed files can do this).
    tree.root.children[0].size = 0;
    tree.root.children[1].size = 4096;

    let config = DuplicateConfig::builder().min_size(1u64).build().unwrap();
    let report = DuplicateFinder::with_config(config)
        .unwrap()
        .find_duplicates(&tree, CancellationToken::new());

    assert_eq!(report.group_count, 1);
    assert_eq!(report.groups[0].size, content.len() as u64);
    assert_eq!(report.groups[0].paths.len(), 2);
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

    let config = DuplicateConfig::builder().min_size(1u64).build().unwrap();
    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_files(
        root,
        &[
            root.join("file1.txt"),
            root.join("file2.txt"),
            root.join("file3.txt"),
            root.join("file4.txt"),
        ],
    );

    let report = finder.find_duplicates(&tree, CancellationToken::new());

    assert_eq!(report.files_analyzed, 4);
    assert!(report.has_duplicates());
    assert_eq!(report.group_count, 1); // Only one duplicate group
    assert_eq!(report.total_duplicate_files(), 2); // file1 + file3 are duplicates
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
        .min_size(1u64)
        .exclude_patterns(vec!["*.txt".to_string()])
        .build()
        .unwrap();

    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_files(
        root,
        &[
            root.join("file1.txt"),
            root.join("file2.txt"),
            root.join(".hidden_file"),
        ],
    );

    let report = finder.find_duplicates(&tree, CancellationToken::new());

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

    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_files(
        root,
        &[
            root.join("small.txt"),
            root.join("medium.txt"),
            root.join("large.txt"),
        ],
    );

    let report = finder.find_duplicates(&tree, CancellationToken::new());

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
        .min_size(1u64)
        .max_groups(2usize) // Only return top 2 groups
        .build()
        .unwrap();

    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_files(
        root,
        &[
            root.join("group1_a.txt"),
            root.join("group1_b.txt"),
            root.join("group2_a.txt"),
            root.join("group2_b.txt"),
            root.join("group3_a.txt"),
            root.join("group3_b.txt"),
        ],
    );

    let report = finder.find_duplicates(&tree, CancellationToken::new());

    // group_count reflects the full (pre-truncation) number of groups
    assert_eq!(report.group_count, 3);
    // groups Vec is truncated to max_groups
    assert_eq!(report.groups.len(), 2);
    // groups_omitted records how many were dropped
    assert_eq!(report.groups_omitted, 1);
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

    let config = DuplicateConfig::builder().min_size(1u64).build().unwrap();
    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_nested_files(
        root,
        &[
            (root.join("dir1/file.txt"), "file.txt"),
            (root.join("dir2/file.txt"), "file.txt"),
        ],
    );

    let report = finder.find_duplicates(&tree, CancellationToken::new());

    assert_eq!(report.files_analyzed, 2);
    assert!(report.has_duplicates());
    assert_eq!(report.group_count, 1);
}

#[cfg(unix)]
#[test]
fn duplicate_hardlink_representative_is_independent_of_traversal_order() {
    let temporary = TempDir::new().unwrap();
    let first = temporary.path().join("a.bin");
    let alias = temporary.path().join("z.bin");
    let independent = temporary.path().join("copy.bin");
    fs::write(&first, b"duplicate content").unwrap();
    fs::hard_link(&first, &alias).unwrap();
    fs::write(&independent, b"duplicate content").unwrap();
    let finder =
        DuplicateFinder::with_config(DuplicateConfig::builder().min_size(1u64).build().unwrap())
            .unwrap();
    for paths in [
        vec![first.clone(), alias.clone(), independent.clone()],
        vec![independent.clone(), alias.clone(), first.clone()],
        vec![alias.clone(), first.clone(), independent.clone()],
    ] {
        let tree = create_test_tree_with_files(temporary.path(), &paths);
        let report = finder.find_duplicates(&tree, CancellationToken::new());
        assert_eq!(report.files_analyzed, 2);
        assert_eq!(report.group_count, 1);
        assert_eq!(report.files_with_duplicates, 2);
        assert_eq!(
            report.groups[0].paths,
            vec![first.clone(), independent.clone()]
        );
        assert_eq!(report.total_wasted_space, b"duplicate content".len() as u64);
    }
    let filtered = DuplicateFinder::with_config(
        DuplicateConfig::builder()
            .min_size(1u64)
            .exclude_patterns(vec!["a.bin".to_string()])
            .build()
            .unwrap(),
    )
    .unwrap();
    let tree = create_test_tree_with_files(
        temporary.path(),
        &[first, alias.clone(), independent.clone()],
    );
    let report = filtered.find_duplicates(&tree, CancellationToken::new());
    assert_eq!(report.groups[0].paths, vec![independent, alias]);
    assert_eq!(report.files_analyzed, 2);
}

#[cfg(unix)]
#[test]
fn test_find_duplicates_ignores_hardlinked_aliases() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    let original = root.join("original.bin");
    let alias = root.join("alias.bin");
    fs::write(&original, "same physical file").unwrap();
    fs::hard_link(&original, &alias).unwrap();

    let config = DuplicateConfig::builder().min_size(1u64).build().unwrap();
    let finder = DuplicateFinder::with_config(config).unwrap();
    let tree = create_test_tree_with_files(root, &[original, alias]);

    let report = finder.find_duplicates(&tree, CancellationToken::new());

    assert_eq!(
        report.files_analyzed, 1,
        "hardlinked aliases should be analyzed once by inode"
    );
    assert!(
        !report.has_duplicates(),
        "hardlinks should not be reported as reclaimable duplicate content"
    );
}

#[test]
fn test_age_analyzer_reports_buckets_average_median_and_top_files() {
    let temp = TempDir::new().unwrap();
    let root_path = temp.path();
    let reference_time = SystemTime::UNIX_EPOCH + days(500);

    let root = test_dir(
        1,
        "root",
        vec![
            test_file(2, "fresh.bin", 100, reference_time - days(1)),
            test_file(3, "warm.bin", 500, reference_time - days(3)),
            test_file(4, "old.bin", 1_000, reference_time - days(400)),
        ],
    );
    let tree = test_tree(root_path, root);

    let config = AgeConfig::builder()
        .reference_time(reference_time)
        .buckets(vec![
            AgeBucket::new("Fresh", days(2)),
            AgeBucket::new("Warm", days(30)),
            AgeBucket::new("Old", Duration::MAX),
        ])
        .min_stale_size(u64::MAX)
        .top_files_per_bucket(1usize)
        .build()
        .unwrap();
    let report = AgeAnalyzer::with_config(config)
        .unwrap()
        .analyze(&tree, CancellationToken::new());

    assert_eq!(report.total_files, 3);
    assert_eq!(report.total_size, 1_600);
    assert_eq!(
        report.average_age.as_secs(),
        (days(1).as_secs() + days(3).as_secs() + days(400).as_secs()) / 3
    );
    assert_eq!(report.median_age_bucket.as_deref(), Some("Warm"));

    let fresh = bucket(&report, "Fresh");
    assert_eq!(fresh.file_count, 1);
    assert_eq!(fresh.total_size, 100);
    assert_eq!(fresh.largest_files[0].0, root_path.join("fresh.bin"));

    let warm = bucket(&report, "Warm");
    assert_eq!(warm.file_count, 1);
    assert_eq!(warm.total_size, 500);
    assert_eq!(warm.largest_files[0].0, root_path.join("warm.bin"));

    let old = bucket(&report, "Old");
    assert_eq!(old.file_count, 1);
    assert_eq!(old.total_size, 1_000);
    assert_eq!(old.largest_files[0].0, root_path.join("old.bin"));
}

#[test]
fn test_age_analyzer_reports_stale_directories_sorted_and_limited() {
    let temp = TempDir::new().unwrap();
    let root_path = temp.path();
    let reference_time = SystemTime::UNIX_EPOCH + days(1_000);

    let root = test_dir(
        1,
        "root",
        vec![
            test_dir(
                2,
                "old_big",
                vec![test_file(3, "data.bin", 2_000, reference_time - days(500))],
            ),
            test_dir(
                4,
                "old_bigger",
                vec![test_file(5, "data.bin", 4_000, reference_time - days(450))],
            ),
            test_dir(
                6,
                "old_small",
                vec![test_file(7, "data.bin", 500, reference_time - days(600))],
            ),
            test_dir(
                8,
                "new_big",
                vec![test_file(9, "data.bin", 10_000, reference_time - days(2))],
            ),
        ],
    );
    let tree = test_tree(root_path, root);

    let config = AgeConfig::builder()
        .reference_time(reference_time)
        .stale_threshold(days(365))
        .min_stale_size(1_000u64)
        .max_stale_dirs(1usize)
        .buckets(vec![AgeBucket::new("Any", Duration::MAX)])
        .build()
        .unwrap();
    let report = AgeAnalyzer::with_config(config)
        .unwrap()
        .analyze(&tree, CancellationToken::new());

    assert!(report.has_stale_directories());
    assert_eq!(report.stale_directories.len(), 1);
    let stale = &report.stale_directories[0];
    assert_eq!(stale.path, root_path.join("old_bigger"));
    assert_eq!(stale.size, 4_000);
    assert_eq!(stale.file_count, 1);
    assert_eq!(stale.newest_file_age, days(450));
    assert_eq!(report.total_stale_size(), 4_000);
}

#[test]
fn test_age_analyzer_honors_pre_cancelled_token() {
    let temp = TempDir::new().unwrap();
    let reference_time = SystemTime::UNIX_EPOCH + days(100);
    let root = test_dir(
        1,
        "root",
        vec![test_file(2, "ignored.bin", 100, reference_time - days(10))],
    );
    let tree = test_tree(temp.path(), root);
    let token = CancellationToken::new();
    token.cancel();

    let config = AgeConfig::builder()
        .reference_time(reference_time)
        .buckets(vec![AgeBucket::new("Any", Duration::MAX)])
        .build()
        .unwrap();
    let report = AgeAnalyzer::with_config(config)
        .unwrap()
        .analyze(&tree, token);

    assert_eq!(report.total_files, 0);
    assert_eq!(report.total_size, 0);
    assert_eq!(report.average_age, Duration::ZERO);
    assert_eq!(report.median_age_bucket, None);
    assert_eq!(report.buckets[0].file_count, 0);
    assert!(report.stale_directories.is_empty());
}

fn create_test_tree_with_files(
    root_dir: &std::path::Path,
    paths: &[std::path::PathBuf],
) -> FileTree {
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            node.inode = Some(gravityfile_core::InodeInfo::new(
                metadata.ino(),
                metadata.dev(),
            ));
        }

        // Read actual content for hashing
        if let Ok(content) = fs::read(path) {
            use blake3::Hasher;
            let mut hasher = Hasher::new();
            hasher.update(&content);
            node.content_hash = Some(gravityfile_core::ContentHash::new(
                *hasher.finalize().as_bytes(),
            ));
        }

        root.children.push(node);
    }

    // Update directory counts
    root.update_counts();

    let config = ScanConfig::new(root_dir);
    let mut stats = gravityfile_core::TreeStats::default();
    for path in paths {
        if let Ok(metadata) = fs::metadata(path) {
            stats.record_file(
                path,
                metadata.len(),
                Some(now),
                1, // depth
            );
        }
    }

    FileTree::new(
        root,
        root_dir.to_path_buf(),
        config,
        stats,
        std::time::Duration::from_secs(0),
        Vec::new(),
    )
}

fn create_test_tree_with_nested_files(
    root_dir: &std::path::Path,
    paths: &[(std::path::PathBuf, &str)],
) -> FileTree {
    let now = std::time::SystemTime::now();
    let root_dir_components: usize = root_dir.components().count();
    let mut root = FileNode::new_directory(NodeId::new(1), "root", Timestamps::with_modified(now));

    for (i, (path, name)) in paths.iter().enumerate() {
        let metadata = fs::metadata(path).unwrap();
        let size = metadata.len();

        // Create parent directories (skip root_dir components)
        let mut current = &mut root;
        for parent_name in path
            .parent()
            .unwrap()
            .components()
            .skip(root_dir_components)
        {
            let parent_name_str = parent_name.as_os_str().to_string_lossy().to_string();
            if !current.children.iter().any(|c| c.name == parent_name_str) {
                let dir_node = FileNode::new_directory(
                    NodeId::new(i as u64 * 10 + current.children.len() as u64 + 2),
                    &parent_name_str,
                    Timestamps::with_modified(now),
                );
                current.children.push(dir_node);
            }
            current = current
                .children
                .iter_mut()
                .find(|c| c.name == parent_name_str)
                .unwrap();
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            node.inode = Some(gravityfile_core::InodeInfo::new(
                metadata.ino(),
                metadata.dev(),
            ));
        }

        if let Ok(content) = fs::read(path) {
            use blake3::Hasher;
            let mut hasher = Hasher::new();
            hasher.update(&content);
            node.content_hash = Some(gravityfile_core::ContentHash::new(
                *hasher.finalize().as_bytes(),
            ));
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

    let config = ScanConfig::new(root_dir);
    let mut stats = gravityfile_core::TreeStats::default();
    for (path, _) in paths {
        if let Ok(metadata) = fs::metadata(path) {
            stats.record_file(
                path,
                metadata.len(),
                Some(now),
                1, // depth
            );
        }
    }

    FileTree::new(
        root,
        root_dir.to_path_buf(),
        config,
        stats,
        std::time::Duration::from_secs(0),
        Vec::new(),
    )
}

fn days(days: u64) -> Duration {
    Duration::from_secs(days * 24 * 60 * 60)
}

fn test_file(id: u64, name: &str, size: u64, modified: SystemTime) -> FileNode {
    FileNode::new_file(
        NodeId::new(id),
        name,
        size,
        (size / 512).max(1),
        Timestamps::with_modified(modified),
        false,
    )
}

fn test_dir(id: u64, name: &str, children: Vec<FileNode>) -> FileNode {
    let mut node = FileNode::new_directory(
        NodeId::new(id),
        name,
        Timestamps::with_modified(SystemTime::UNIX_EPOCH),
    );
    node.size = children.iter().map(|child| child.size).sum();
    node.children = children;
    node.update_counts();
    node
}

fn test_tree(root_dir: &std::path::Path, mut root: FileNode) -> FileTree {
    root.update_counts();
    let config = ScanConfig::new(root_dir);
    FileTree::new(
        root,
        root_dir.to_path_buf(),
        config,
        gravityfile_core::TreeStats::default(),
        Duration::ZERO,
        Vec::new(),
    )
}

fn bucket<'a>(
    report: &'a gravityfile_analyze::AgeReport,
    name: &str,
) -> &'a gravityfile_analyze::AgeBucketStats {
    report
        .buckets
        .iter()
        .find(|bucket| bucket.name == name)
        .unwrap_or_else(|| panic!("missing age bucket {name}"))
}

#[test]
fn age_analysis_handles_deep_library_trees_without_recursive_traversal() {
    let depth = 5000;
    let mut node = test_file(depth + 1, "leaf", 10, SystemTime::UNIX_EPOCH);
    for id in (0..depth).rev() {
        let mut parent = FileNode::new_directory(
            NodeId::new(id),
            "d",
            Timestamps::with_modified(SystemTime::UNIX_EPOCH),
        );
        parent.kind = gravityfile_core::NodeKind::Directory {
            file_count: 1,
            dir_count: depth - id - 1,
        };
        parent.size = 10;
        parent.children.push(node);
        node = parent;
    }
    let mut tree = FileTree::new(
        node,
        "root".into(),
        ScanConfig::new("root"),
        gravityfile_core::TreeStats::default(),
        Duration::ZERO,
        Vec::new(),
    );
    let report = AgeAnalyzer::with_config(AgeConfig {
        reference_time: SystemTime::UNIX_EPOCH + Duration::from_secs(100),
        stale_threshold: Duration::from_secs(1),
        min_stale_size: 0,
        max_stale_dirs: 1,
        ..Default::default()
    })
    .unwrap()
    .analyze(&tree, CancellationToken::new());
    // FileNode destruction is independently recursive; dismantle this synthetic
    // tree before assertions so the test isolates analyzer traversal behavior.
    let mut pending = std::mem::take(&mut tree.root.children);
    while let Some(mut node) = pending.pop() {
        pending.append(&mut node.children);
    }
    assert!(report.status.is_complete());
    assert_eq!(report.total_files, 1);
    assert_eq!(report.total_size, 10);
    assert_eq!(report.average_age, Duration::from_secs(100));
    assert_eq!(report.stale_directories.len(), 1);
    assert_eq!(
        report.stale_directories[0].path,
        std::path::Path::new("root")
    );
}

#[test]
fn equal_size_age_files_have_stable_report_order() {
    let modified = SystemTime::UNIX_EPOCH;
    let expected = ["root/a", "root/b", "root/c", "root/d"];
    for names in [
        ["d", "b", "a", "c"],
        ["c", "a", "b", "d"],
        ["a", "b", "c", "d"],
    ] {
        let children = names
            .iter()
            .enumerate()
            .map(|(id, name)| test_file(id as u64 + 1, name, 10, modified))
            .collect();
        let tree = test_tree(std::path::Path::new("root"), test_dir(0, "root", children));
        let report = AgeAnalyzer::with_config(AgeConfig {
            reference_time: modified + Duration::from_secs(1),
            top_files_per_bucket: 4,
            ..Default::default()
        })
        .unwrap()
        .analyze(&tree, CancellationToken::new());
        let actual: Vec<_> = report
            .buckets
            .iter()
            .flat_map(|bucket| bucket.largest_files.iter().map(|file| file.0.clone()))
            .collect();
        assert_eq!(actual, expected.map(std::path::PathBuf::from));
    }
}

#[test]
fn many_age_buckets_preserve_inclusive_boundaries_and_catch_all() {
    let reference = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    let ages = [
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(500),
        Duration::from_secs(500) + Duration::from_nanos(1),
        Duration::from_secs(999),
        Duration::from_secs(1000),
        Duration::from_secs(9000),
    ];
    let mut buckets: Vec<_> = (0..1000)
        .rev()
        .map(|i| AgeBucket::new(format!("b{i}"), Duration::from_secs(i)))
        .collect();
    buckets.push(AgeBucket::new("rest", Duration::MAX));
    let children = ages
        .iter()
        .enumerate()
        .map(|(i, age)| test_file(i as u64 + 1, &format!("f{i}"), 10, reference - *age))
        .collect();
    let tree = test_tree(std::path::Path::new("root"), test_dir(0, "root", children));
    let report = AgeAnalyzer::with_config(AgeConfig {
        reference_time: reference,
        buckets,
        ..Default::default()
    })
    .unwrap()
    .analyze(&tree, CancellationToken::new());
    assert!(report.status.is_complete());
    assert_eq!(report.total_files, 7);
    assert_eq!(report.total_size, 70);
    for entry in &report.buckets {
        let expected = match entry.name.as_str() {
            "b0" | "b1" | "b500" | "b501" | "b999" => 1,
            "rest" => 2,
            _ => 0,
        };
        assert_eq!(entry.file_count, expected, "{}", entry.name);
        assert_eq!(entry.total_size, expected * 10);
    }
}

/// Manual optimized measurement; excludes fixture construction and filesystem I/O.
#[test]
#[ignore = "manual age-analysis scale benchmark"]
fn age_analysis_scale_benchmark() {
    use std::hint::black_box;
    use std::time::Instant;
    let reference = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    for files in [100_000usize, 1_000_000] {
        let children = (0..files)
            .map(|i| {
                test_file(
                    i as u64 + 1,
                    &format!("f{i:09}"),
                    256,
                    reference - Duration::from_secs((i % 2000) as u64),
                )
            })
            .collect();
        let tree = test_tree(std::path::Path::new("root"), test_dir(0, "root", children));
        for custom in [false, true] {
            let mut config = AgeConfig {
                reference_time: reference,
                ..Default::default()
            };
            if custom {
                config.buckets = (0..1000)
                    .map(|i| AgeBucket::new(format!("b{i}"), Duration::from_secs(i)))
                    .collect();
                config.buckets.push(AgeBucket::new("rest", Duration::MAX));
            }
            let mut expected = vec![0u64; config.buckets.len()];
            // Independent linear reference over the 2,000 distinct fixture ages.
            for seconds in 0..2000usize {
                let bucket = config
                    .buckets
                    .iter()
                    .position(|b| Duration::from_secs(seconds as u64) <= b.max_age)
                    .unwrap();
                expected[bucket] += (files / 2000 + usize::from(seconds < files % 2000)) as u64;
            }
            let analyzer = AgeAnalyzer::with_config(config).unwrap();
            let mut samples = Vec::new();
            for _ in 0..3 {
                let started = Instant::now();
                let report = analyzer.analyze(black_box(&tree), CancellationToken::new());
                let elapsed = started.elapsed().as_nanos();
                assert!(report.status.is_complete());
                assert_eq!(report.total_files, files as u64);
                assert_eq!(report.total_size, files as u64 * 256);
                assert!(report.stale_directories.is_empty());
                for (bucket, expected) in report.buckets.iter().zip(&expected) {
                    assert_eq!(bucket.file_count, *expected);
                    assert_eq!(bucket.total_size, expected * 256);
                    assert_eq!(bucket.largest_files.len(), (*expected).min(10) as usize);
                }
                black_box(&report);
                samples.push(elapsed);
            }
            println!(
                "AGE_BENCH {{\"files\":{files},\"buckets\":{},\"samples_ns\":{samples:?}}}",
                expected.len()
            );
        }
    }
}
