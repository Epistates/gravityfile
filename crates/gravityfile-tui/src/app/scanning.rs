//! Background scanning and analysis.

use std::path::PathBuf;

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use gravityfile_analyze::{AgeAnalyzer, AgeConfig, DuplicateConfig, DuplicateFinder};
use std::sync::Arc;

use gravityfile_core::FileTree;
use gravityfile_scan::{JwalkScanner, ScanConfig, ScanProgress};

use super::constants::{
    ANALYSIS_CHANNEL_SIZE, MAX_DUPLICATE_GROUPS, MIN_DUPLICATE_SIZE, SCAN_CHANNEL_SIZE,
};
use super::state::ScanResult;

/// Start a background filesystem scan.
///
/// Returns a receiver that will receive scan progress updates and the final result.
pub fn start_scan(
    path: PathBuf,
    include_hidden: bool,
    apparent_size: bool,
    max_entries: Option<u64>,
    token: CancellationToken,
) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(SCAN_CHANNEL_SIZE);

    tokio::spawn(async move {
        let mut config = ScanConfig::new(&path);
        config.include_hidden = include_hidden;
        config.apparent_size = apparent_size;
        config.max_entries = max_entries;
        config.git_status = true;
        let scanner = JwalkScanner::new();
        let progress_task = tokio::spawn(forward_progress(
            scanner.subscribe(),
            tx.clone(),
            token.clone(),
        ));

        // Run scan in blocking task (jwalk uses rayon internally)
        let token_clone = token.clone();
        let result = tokio::task::spawn_blocking(move || scanner.scan(&config, token_clone))
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

async fn forward_progress(
    mut progress: broadcast::Receiver<ScanProgress>,
    destination: mpsc::Sender<ScanResult>,
    token: CancellationToken,
) {
    loop {
        let update = tokio::select! {
            biased;
            _ = token.cancelled() => break,
            _ = destination.closed() => { token.cancel(); break; },
            update = progress.recv() => update,
        };
        match update {
            Ok(progress) => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => break,
                    result = destination.send(ScanResult::Progress(progress)) => {
                        if result.is_err() { token.cancel(); break; }
                    },
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Start background analysis of a scanned tree.
///
/// Returns a receiver that will receive the analysis results.
pub fn start_analysis(
    tree: Arc<FileTree>,
    compare_contents: bool,
    token: CancellationToken,
) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(ANALYSIS_CHANNEL_SIZE);

    tokio::spawn(async move {
        let work_token = token.clone();
        let work = tokio::task::spawn_blocking(move || {
            let token = work_token;
            let dup_config = DuplicateConfig::builder()
                .min_size(MIN_DUPLICATE_SIZE)
                .max_groups(MAX_DUPLICATE_GROUPS)
                .build()
                .expect("duplicate config uses only defaulted fields");
            let finder = DuplicateFinder::with_config(dup_config)
                .expect("validated constant duplicate configuration");
            let duplicates = if compare_contents {
                finder.find_duplicates(&tree, token.clone())
            } else {
                gravityfile_analyze::DuplicateReport::default()
            };

            if token.is_cancelled() {
                return None;
            }

            let age_config = AgeConfig::default();
            let analyzer =
                AgeAnalyzer::with_config(age_config).expect("valid default age configuration");
            let age_report = analyzer.analyze(&tree, token.clone());

            if token.is_cancelled() {
                return None;
            }

            let counts =
                super::duplicate_filter::prepare_counts(&duplicates, &tree.root_path, &token)?;
            Some(ScanResult::AnalysisComplete {
                duplicates,
                age_report,
                duplicate_scope: (tree.root_path.clone(), counts),
            })
        });
        complete_analysis(work, tx, token).await;
    });

    rx
}

async fn complete_analysis(
    work: tokio::task::JoinHandle<Option<ScanResult>>,
    destination: mpsc::Sender<ScanResult>,
    token: CancellationToken,
) {
    let result = tokio::select! {
        biased;
        _ = destination.closed() => { token.cancel(); return; },
        _ = token.cancelled() => ScanResult::AnalysisCancelled,
        result = work => match result {
            Ok(Some(result)) => result,
            Ok(None) => ScanResult::AnalysisCancelled,
            Err(error) => ScanResult::AnalysisFailed(error.to_string()),
        },
    };
    let _ = destination.send(result).await;
}

/// Run a rename batch off the event-loop thread. Once started it is awaited
/// through rollback, including when cancellation is requested. Success
/// returns the pairs for the undo log.
pub(super) fn start_bulk_rename(
    renames: Vec<(PathBuf, PathBuf)>,
    token: CancellationToken,
) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            gravityfile_ops::rename_paths(&renames, &token).map(|()| renames)
        })
        .await
        .unwrap_or_else(|error| Err(format!("Bulk rename worker failed: {error}")));
        let _ = tx.send(ScanResult::BulkRenameComplete(result)).await;
    });
    rx
}

/// Create a leaf directory or accept an existing real directory, off the UI.
/// A competing creator after inspection is reported as a conflict, not adopted.
pub(super) fn start_take(path: PathBuf) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let work_path = path.clone();
        let receiver = tx.clone();
        let work = tokio::task::spawn_blocking(move || -> Option<Result<bool, String>> {
            if receiver.is_closed() {
                return None;
            }
            let result = match std::fs::symlink_metadata(&work_path) {
                Ok(metadata) if metadata.is_dir() => Ok(false),
                Ok(_) => Err("The destination exists and is not a real directory".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if receiver.is_closed() {
                        return None;
                    }
                    std::fs::create_dir(&work_path)
                        .map(|()| true)
                        .map_err(|error| error.to_string())
                }
                Err(error) => Err(error.to_string()),
            };
            Some(result)
        })
        .await;
        let result = match work {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(error) => Err(format!("Directory task failed: {error}")),
        };
        let _ = tx.send(ScanResult::TakeComplete { path, result }).await;
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn analysis_panic_is_delivered_as_an_explicit_failure() {
        let (destination, mut received) = mpsc::channel(1);
        let work = tokio::task::spawn_blocking(|| panic!("injected analysis failure"));
        complete_analysis(work, destination, CancellationToken::new()).await;
        assert!(
            matches!(received.recv().await, Some(ScanResult::AnalysisFailed(error)) if error.contains("injected analysis failure"))
        );
    }

    #[tokio::test]
    async fn dropped_analysis_receiver_cancels_worker_token() {
        let (destination, received) = mpsc::channel(1);
        let token = CancellationToken::new();
        let work_token = token.clone();
        let work = tokio::spawn(async move {
            work_token.cancelled().await;
            None
        });
        drop(received);
        tokio::time::timeout(
            Duration::from_secs(1),
            complete_analysis(work, destination, token.clone()),
        )
        .await
        .unwrap();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_analysis_has_a_terminal_result() {
        let (destination, mut received) = mpsc::channel(1);
        let token = CancellationToken::new();
        token.cancel();
        let work = tokio::spawn(async { None });
        complete_analysis(work, destination, token).await;
        assert!(matches!(
            received.recv().await,
            Some(ScanResult::AnalysisCancelled)
        ));
        assert!(received.recv().await.is_none());
    }

    #[tokio::test]
    async fn lagged_progress_recovers_to_retained_updates() {
        let (source, updates) = broadcast::channel(2);
        for files in 1..=4 {
            source
                .send(ScanProgress {
                    files_scanned: files,
                    ..Default::default()
                })
                .unwrap();
        }
        drop(source);
        let (destination, mut received) = mpsc::channel(8);
        let token = CancellationToken::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            forward_progress(updates, destination, token.clone()),
        )
        .await
        .unwrap();
        let mut files = Vec::new();
        while let Some(ScanResult::Progress(progress)) = received.recv().await {
            files.push(progress.files_scanned);
        }
        assert_eq!(files, [3, 4]);
        assert!(!token.is_cancelled());
    }

    #[tokio::test]
    async fn closing_idle_ui_receiver_cancels_scan_work() {
        let (_source, updates) = broadcast::channel(2);
        let (destination, received) = mpsc::channel(1);
        let token = CancellationToken::new();
        drop(received);
        tokio::time::timeout(
            Duration::from_secs(1),
            forward_progress(updates, destination, token.clone()),
        )
        .await
        .unwrap();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_full_progress_queue() {
        let (source, updates) = broadcast::channel(2);
        let (destination, _received) = mpsc::channel(1);
        destination
            .send(ScanResult::Progress(ScanProgress::new()))
            .await
            .unwrap();
        source.send(ScanProgress::new()).unwrap();
        let token = CancellationToken::new();
        let forwarding = tokio::spawn(forward_progress(updates, destination, token.clone()));
        tokio::task::yield_now().await;
        token.cancel();
        tokio::time::timeout(Duration::from_secs(1), forwarding)
            .await
            .unwrap()
            .unwrap();
    }
}
