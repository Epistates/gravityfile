//! Live scan progress on stderr.

use std::io::IsTerminal;
use std::time::Duration;

use gravityfile_core::units::{Units, format_count, format_size};
use gravityfile_core::{FileTree, ScanConfig, ScanError};
use gravityfile_scan::JwalkScanner;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

/// Scan `config`, showing a spinner with running totals while stderr is a
/// terminal. The spinner is cleared before returning.
pub fn scan(
    config: &ScanConfig,
    cancel: &CancellationToken,
    units: Units,
) -> Result<FileTree, ScanError> {
    let scanner = JwalkScanner::new();
    if !std::io::stderr().is_terminal() {
        return scanner.scan(config, cancel.clone());
    }
    let spinner = ProgressBar::new_spinner().with_style(
        ProgressStyle::with_template("{spinner} {msg}").expect("valid progress template"),
    );
    spinner.set_message("Scanning…");
    spinner.enable_steady_tick(Duration::from_millis(80));
    let mut updates = scanner.subscribe();
    let watcher = {
        let spinner = spinner.clone();
        std::thread::spawn(move || {
            loop {
                match updates.blocking_recv() {
                    Ok(progress) => spinner.set_message(format!(
                        "Scanning… {} files, {} directories, {}",
                        format_count(progress.files_scanned),
                        format_count(progress.dirs_scanned),
                        format_size(progress.bytes_scanned, units),
                    )),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        })
    };
    let result = scanner.scan(config, cancel.clone());
    // Dropping the scanner closes the progress channel and ends the watcher.
    drop(scanner);
    let _ = watcher.join();
    spinner.finish_and_clear();
    result
}
