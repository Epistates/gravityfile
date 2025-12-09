//! Background scanning and analysis.

use std::path::PathBuf;

use tokio::sync::mpsc;

use gravityfile_analyze::{AgeAnalyzer, AgeConfig, DuplicateConfig, DuplicateFinder};
use gravityfile_core::FileTree;
use gravityfile_scan::{JwalkScanner, ScanConfig};

use super::constants::{
    ANALYSIS_CHANNEL_SIZE, MAX_DUPLICATE_GROUPS, MIN_DUPLICATE_SIZE, SCAN_CHANNEL_SIZE,
};
use super::state::ScanResult;

/// Start a background filesystem scan.
///
/// Returns a receiver that will receive scan progress updates and the final result.
pub fn start_scan(path: PathBuf) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(SCAN_CHANNEL_SIZE);

    tokio::spawn(async move {
        let config = ScanConfig::new(&path);
        let scanner = JwalkScanner::new();
        let mut progress_rx = scanner.subscribe();

        // Spawn task to forward progress updates
        let tx_progress = tx.clone();
        let progress_task = tokio::spawn(async move {
            while let Ok(progress) = progress_rx.recv().await {
                if tx_progress.send(ScanResult::Progress(progress)).await.is_err() {
                    break;
                }
            }
        });

        // Run scan in blocking task (jwalk uses rayon internally)
        let result = tokio::task::spawn_blocking(move || scanner.scan(&config))
            .await
            .unwrap_or_else(|e| {
                Err(gravityfile_scan::ScanError::Other {
                    message: e.to_string(),
                })
            });

        // Cancel progress task and send final result
        progress_task.abort();
        let _ = tx.send(ScanResult::ScanComplete(result)).await;
    });

    rx
}

/// Start background analysis of a scanned tree.
///
/// Returns a receiver that will receive the analysis results.
pub fn start_analysis(tree: FileTree) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(ANALYSIS_CHANNEL_SIZE);

    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let dup_config = DuplicateConfig::builder()
                .min_size(MIN_DUPLICATE_SIZE)
                .max_groups(MAX_DUPLICATE_GROUPS)
                .build()
                .unwrap();
            let finder = DuplicateFinder::with_config(dup_config);
            let duplicates = finder.find_duplicates(&tree);

            let age_config = AgeConfig::default();
            let analyzer = AgeAnalyzer::with_config(age_config);
            let age_report = analyzer.analyze(&tree);

            (duplicates, age_report)
        })
        .await;

        if let Ok((duplicates, age_report)) = result {
            let _ = tx
                .send(ScanResult::AnalysisComplete {
                    duplicates,
                    age_report,
                })
                .await;
        }
    });

    rx
}
