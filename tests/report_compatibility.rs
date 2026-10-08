//! Fixed consumer fixtures must not be regenerated from the current writer.

use gravityfile_analyze::{AgeReport, AnalysisState, DuplicateReport};

const DUPLICATES: &str = include_str!("fixtures/reports/legacy-duplicates.json");
const AGE: &str = include_str!("fixtures/reports/legacy-age.json");
const SCAN: &str = include_str!("fixtures/reports/legacy-scan.json");

#[test]
fn fixed_legacy_scan_preserves_tree_precision_warnings_and_defaults() {
    use gravityfile_core::{FileTree, WarningKind};
    use std::time::{Duration, UNIX_EPOCH};
    let tree: FileTree = serde_json::from_str(SCAN).unwrap();
    assert_eq!(tree.root_path, std::path::Path::new("data"));
    assert_eq!(tree.root.size, 9_007_199_254_740_993);
    assert_eq!(tree.stats.total_size, tree.root.size);
    assert_eq!(tree.stats.total_files, 1);
    assert_eq!(tree.root.children.len(), 1);
    let child = &tree.root.children[0];
    assert_eq!(child.file_name(), "a\n.txt");
    assert_eq!(child.inode.unwrap().inode, 9_007_199_254_740_995);
    assert!(child.native_name.is_none());
    assert!(child.git_status.is_none());
    assert_eq!(
        child.timestamps.modified,
        UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789)
    );
    assert_eq!(
        tree.scanned_at,
        UNIX_EPOCH + Duration::new(1_700_000_001, 987_654_321)
    );
    assert_eq!(tree.scan_duration, Duration::new(1, 42));
    assert_eq!(tree.config.max_entries, None);
    assert!(tree.config.include_hidden);
    assert!(!tree.config.follow_symlinks);
    assert!(!tree.config.git_status);
    assert!(tree.has_warnings());
    assert_eq!(tree.warning_count(), 1);
    assert_eq!(tree.warnings[0].kind, WarningKind::PermissionDenied);
    assert_eq!(tree.warnings[0].path, std::path::Path::new("data/private"));

    // Every field supplied by the fixed consumer fixture must survive writing,
    // except those schema version 2 dropped (node ids, content hashes, access
    // and creation times, hashing options); newly defaulted optional fields
    // may be added by the current writer.
    const DROPPED: [&str; 6] = [
        "id",
        "content_hash",
        "accessed",
        "created",
        "compute_hashes",
        "min_hash_size",
    ];
    fn assert_preserved(expected: &serde_json::Value, actual: &serde_json::Value) {
        match expected {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    if !DROPPED.contains(&key.as_str()) {
                        assert_preserved(value, &actual[key]);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                assert_eq!(values.len(), actual.as_array().unwrap().len());
                for (index, value) in values.iter().enumerate() {
                    assert_preserved(value, &actual[index]);
                }
            }
            _ => assert_eq!(expected, actual),
        }
    }
    assert_preserved(
        &serde_json::from_str(SCAN).unwrap(),
        &serde_json::to_value(tree).unwrap(),
    );
}

#[test]
fn fixed_scan_accepts_additions_but_rejects_malformed_known_fields() {
    use gravityfile_core::FileTree;
    let mut value: serde_json::Value = serde_json::from_str(SCAN).unwrap();
    value["future_metadata"] = serde_json::json!({"revision": 2});
    value["root"]["children"][0]["future_flag"] = serde_json::json!(true);
    assert!(serde_json::from_value::<FileTree>(value.clone()).is_ok());
    for (pointer, invalid) in [
        ("/root/children/0/size", serde_json::json!(-1)),
        ("/stats/total_size", serde_json::json!("9007199254740993")),
        ("/root/children/0/kind", serde_json::json!("FutureNode")),
        ("/warnings/0/kind", serde_json::json!("FutureWarning")),
    ] {
        let mut malformed = value.clone();
        *malformed.pointer_mut(pointer).unwrap() = invalid;
        assert!(
            serde_json::from_value::<FileTree>(malformed).is_err(),
            "{pointer}"
        );
    }
}

#[cfg(unix)]
#[test]
fn reports_roundtrip_native_paths_and_reject_foreign_or_malformed_encodings() {
    use gravityfile_core::{ScanConfig, ScanWarning, TreeStats, WarningKind};
    use std::os::unix::ffi::OsStringExt;
    let path =
        std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/data/invalid-\xff".to_vec()));
    let config = ScanConfig::new(&path);
    let wire = serde_json::to_value(&config).unwrap();
    assert_eq!(wire["root"]["platform"], "unix");
    assert_eq!(
        serde_json::from_value::<ScanConfig>(wire).unwrap().root,
        path
    );
    for root in [
        serde_json::json!({"platform":"windows","wide":[0xd800]}),
        serde_json::json!({"platform":"unix","bytes":[256]}),
        serde_json::json!({"platform":"unix","bytes":[1],"extra":true}),
    ] {
        assert!(serde_json::from_value::<ScanConfig>(serde_json::json!({"root":root})).is_err());
    }
    let mut stats = TreeStats::default();
    stats.record_file(&path, 42, Some(std::time::UNIX_EPOCH), 1);
    let restored: TreeStats =
        serde_json::from_value(serde_json::to_value(&stats).unwrap()).unwrap();
    assert_eq!(restored.largest_file.unwrap().0, path);
    assert_eq!(restored.oldest_file.unwrap().0, path);
    assert_eq!(restored.newest_file.unwrap().0, path);
    let warning = ScanWarning::new(&path, WarningKind::ReadError, "unreadable");
    assert_eq!(
        serde_json::from_value::<ScanWarning>(serde_json::to_value(warning).unwrap())
            .unwrap()
            .path,
        path
    );
    let mut duplicates: DuplicateReport = serde_json::from_str(DUPLICATES).unwrap();
    duplicates.groups[0].paths[0] = path.clone();
    let restored: DuplicateReport =
        serde_json::from_value(serde_json::to_value(&duplicates).unwrap()).unwrap();
    assert_eq!(restored.groups[0].paths[0], path);
    let mut age: AgeReport = serde_json::from_str(AGE).unwrap();
    age.buckets[0].largest_files[0].0 = path.clone();
    let restored: AgeReport = serde_json::from_value(serde_json::to_value(&age).unwrap()).unwrap();
    assert_eq!(restored.buckets[0].largest_files[0].0, path);
}

#[cfg(unix)]
#[test]
fn native_node_names_roundtrip_without_lossy_aliasing() {
    use gravityfile_core::{FileNode, Timestamps};
    use std::os::unix::ffi::OsStringExt;
    let names = [
        std::ffi::OsString::from_vec(b"bad-\xff".to_vec()),
        std::ffi::OsString::from_vec(b"bad-\xfe".to_vec()),
    ];
    let mut nodes = Vec::new();
    for name in &names {
        let mut node =
            FileNode::new_directory("", Timestamps::with_modified(std::time::UNIX_EPOCH));
        node.set_file_name(name);
        let restored: FileNode =
            serde_json::from_slice(&serde_json::to_vec(&node).unwrap()).unwrap();
        assert_eq!(restored.file_name(), name);
        nodes.push(restored);
    }
    assert_eq!(nodes[0].name, nodes[1].name);
    assert_ne!(nodes[0].file_name(), nodes[1].file_name());
    nodes[0].set_file_name(std::ffi::OsStr::new("ordinary"));
    assert!(nodes[0].native_name.is_none());
    let mut legacy = serde_json::to_value(&nodes[0]).unwrap();
    legacy.as_object_mut().unwrap().remove("native_name");
    assert_eq!(
        serde_json::from_value::<FileNode>(legacy)
            .unwrap()
            .file_name(),
        "ordinary"
    );
}

#[test]
fn scan_entry_budget_preserves_legacy_defaults_and_explicit_limits() {
    use gravityfile_core::ScanConfig;
    let legacy: ScanConfig = serde_json::from_str(r#"{"root":"."}"#).unwrap();
    assert_eq!(legacy.max_entries, None);
    assert_eq!(ScanConfig::new(".").max_entries, None);
    assert_eq!(
        ScanConfig::builder().root(".").build().unwrap().max_entries,
        None
    );
    for limit in [0, 1, u64::MAX] {
        let config: ScanConfig = serde_json::from_value(serde_json::json!({
            "root": ".", "max_entries": limit
        }))
        .unwrap();
        assert_eq!(config.max_entries, Some(limit));
        assert_eq!(serde_json::to_value(config).unwrap()["max_entries"], limit);
    }
    assert_eq!(
        serde_json::to_value(gravityfile_core::ScanError::EntryLimitExceeded { limit: 7 }).unwrap(),
        serde_json::json!({"EntryLimitExceeded": {"limit": 7}})
    );
}

#[test]
fn fixed_legacy_duplicates_preserve_integer_precision_and_unknown_status() {
    let report: DuplicateReport = serde_json::from_str(DUPLICATES).unwrap();
    assert_eq!(report.status.state, AnalysisState::Unknown);
    assert!(!report.status.is_complete());
    assert_eq!(report.groups[0].size, 9_007_199_254_740_993);
    assert_eq!(report.total_duplicate_size, 18_014_398_509_481_986);
    assert_eq!(report.groups[0].paths[1], std::path::Path::new("data/b"));
    assert_eq!(report.group_count, 1);
    let round_trip: DuplicateReport =
        serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
    assert_eq!(round_trip.total_wasted_space, report.total_wasted_space);
    assert_eq!(round_trip.status.state, AnalysisState::Unknown);
}

#[test]
fn fixed_legacy_age_preserves_time_precision_and_unknown_status() {
    let report: AgeReport = serde_json::from_str(AGE).unwrap();
    assert_eq!(
        report.stale_directory_status,
        gravityfile_analyze::StaleDirectoryStatus::Unknown
    );
    assert_eq!(report.status.state, AnalysisState::Unknown);
    assert!(!report.status.is_complete());
    assert_eq!(report.total_size, 9_007_199_254_740_993);
    assert_eq!(report.average_age, std::time::Duration::new(86_400, 42));
    let timestamp = report.buckets[0].largest_files[0]
        .2
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    assert_eq!(
        timestamp,
        std::time::Duration::new(1_700_000_000, 123_456_789)
    );
    let round_trip: AgeReport =
        serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
    assert_eq!(round_trip.average_age, report.average_age);
    assert_eq!(round_trip.status.state, AnalysisState::Unknown);
}

#[test]
fn additive_fields_remain_compatible_and_malformed_known_fields_fail() {
    let mut value: serde_json::Value = serde_json::from_str(DUPLICATES).unwrap();
    value["future_metadata"] = serde_json::json!({"revision": 2});
    let report: DuplicateReport = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(report.status.state, AnalysisState::Unknown);
    value["total_wasted_space"] = serde_json::json!("9007199254740993");
    assert!(serde_json::from_value::<DuplicateReport>(value).is_err());
    let mut age: serde_json::Value = serde_json::from_str(AGE).unwrap();
    age["total_size"] = serde_json::json!(-1);
    assert!(serde_json::from_value::<AgeReport>(age).is_err());
}

#[test]
fn scan_errors_preserve_unicode_and_native_path_values() {
    use gravityfile_core::ScanError;
    use std::path::PathBuf;
    let mut cases = vec![(PathBuf::from("data/file"), serde_json::json!("data/file"))];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        for bytes in [b"data/\xff".to_vec(), b"data/\xfe".to_vec()] {
            cases.push((
                std::ffi::OsString::from_vec(bytes.clone()).into(),
                serde_json::json!({"platform":"unix","bytes":bytes}),
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        for wide in [vec![100, 47, 0xd800], vec![100, 47, 0xd801]] {
            cases.push((
                std::ffi::OsString::from_wide(&wide).into(),
                serde_json::json!({"platform":"windows","wide":wide}),
            ));
        }
    }
    for (path, expected) in cases {
        for (kind, variant) in [
            (std::io::ErrorKind::PermissionDenied, "PermissionDenied"),
            (std::io::ErrorKind::NotFound, "NotFound"),
            (std::io::ErrorKind::Other, "Io"),
        ] {
            let error = ScanError::io(&path, std::io::Error::new(kind, "fixture error"));
            let source = std::error::Error::source(&error)
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap();
            assert_eq!(source.kind(), kind);
            assert_eq!(
                serde_json::to_value(&error).unwrap(),
                serde_json::json!({variant:{"path":expected,"source":"fixture error"}})
            );
        }
        assert_eq!(
            serde_json::to_value(ScanError::NotADirectory { path }).unwrap(),
            serde_json::json!({"NotADirectory":{"path":expected}})
        );
    }
}
