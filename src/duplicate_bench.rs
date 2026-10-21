//! Manual phase measurements, separate from whole-command CLI benchmarks.

use gravityfile_analyze::{AnalysisState, DuplicateConfig, DuplicateFinder};
use gravityfile_core::ScanConfig;
use gravityfile_scan::JwalkScanner;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

#[test]
#[ignore = "manual release-mode filesystem benchmark"]
#[allow(clippy::assertions_on_constants)]
fn benchmark_duplicate_phases() {
    // This runtime guard must also compile in normal debug test builds.
    assert!(!cfg!(debug_assertions), "run this benchmark with --release");
    let files: u64 = std::env::var("GRAVITYFILE_DUPLICATE_PROFILE_FILES")
        .map(|value| value.parse().expect("file count must be an integer"))
        .unwrap_or(100_000);
    assert!(files >= 512, "each of 256 contents must have a duplicate");
    let fixture = tempfile::tempdir().unwrap();
    for index in 0..files {
        let directory = fixture.path().join(format!("d{:06}", index / 1000));
        if index % 1000 == 0 {
            std::fs::create_dir(&directory).unwrap();
        }
        std::fs::write(directory.join(format!("f{index:09}")), [index as u8; 256]).unwrap();
    }
    let started = Instant::now();
    let tree = JwalkScanner::new()
        .scan(&ScanConfig::new(fixture.path()), CancellationToken::new())
        .unwrap();
    let scan_seconds = started.elapsed().as_secs_f64();
    assert_eq!(tree.stats.total_files, files);
    assert!(tree.warnings.is_empty());
    let finder = DuplicateFinder::with_config(DuplicateConfig {
        min_size: 0,
        max_groups: 1,
        ..Default::default()
    })
    .unwrap();
    let mut samples = Vec::new();
    for run in 0..3 {
        let started = Instant::now();
        let (report, timings) =
            finder.find_duplicates_with_timings(&tree, CancellationToken::new());
        let elapsed = started.elapsed().as_secs_f64();
        assert_eq!(report.status.state, AnalysisState::Complete);
        assert_eq!(report.files_analyzed, files);
        assert_eq!(report.files_with_duplicates, files);
        assert_eq!(report.group_count, 256);
        assert_eq!(report.groups_omitted, 255);
        assert_eq!(report.total_duplicate_size, files * 256);
        assert_eq!(report.total_wasted_space, (files - 256) * 256);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].paths.len() as u64, files.div_ceil(256));
        samples.push(serde_json::json!({
            "run": run, "elapsed_seconds": elapsed,
            "collection_seconds": timings.collection.as_secs_f64(),
            "grouping_seconds": timings.grouping.as_secs_f64(),
            "hashing_seconds": timings.hashing.as_secs_f64(),
            "finalization_seconds": timings.finalization.as_secs_f64(),
        }));
    }
    fixture.close().unwrap();
    println!(
        "DUPLICATE_PHASE_BENCHMARK={}",
        serde_json::json!({
            "files": files, "file_bytes": 256, "distinct_contents": 256,
            "files_per_directory": 1000, "scan_seconds": scan_seconds,
            "samples": samples, "fixture_removed": true,
            "method": "one scan followed by three analyses; warm-cache possible; fixture setup, report serialization and cleanup excluded; no RSS measurement",
        })
    );
}
