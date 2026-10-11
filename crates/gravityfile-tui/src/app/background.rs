//! Background work reporting back to the run loop: scans, analysis,
//! partial refreshes and the channels the loop selects over.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use gravityfile_scan::ScanProgress;

use super::state::{AppMode, ScanResult, View};
use super::{App, Background, deletion, scanning, state};
use crate::ui::TreeState;

impl App {
    /// Whether any background task can still report back.
    pub(super) fn has_background(&self) -> bool {
        self.scan_rx.is_some()
            || self.op_rx.is_some()
            || self.take_rx.is_some()
            || self.preview_rx.is_some()
            || self.archive_rx.is_some()
            || self.refresh_rx.is_some()
            || !self.listings.is_empty()
            || self.history_rx.is_some()
    }

    /// The next result from a background task. Never completes when
    /// [`Self::has_background`] is false.
    pub(super) async fn next_background(&mut self) -> Background {
        tokio::select! {
            result = async {
                self.scan_rx.as_mut().expect("guarded scan receiver").recv().await
            }, if self.scan_rx.is_some() => Background::Scan(result),
            result = async {
                self.op_rx.as_mut().expect("guarded operation receiver").recv().await
            }, if self.op_rx.is_some() => Background::Operation(result),
            take = async {
                self.take_rx.as_mut().expect("guarded take receiver").recv().await
            }, if self.take_rx.is_some() => Background::Take(take),
            preview = async {
                self.preview_rx.as_mut().expect("guarded preview receiver").await.ok()
            }, if self.preview_rx.is_some() => Background::Preview(preview),
            archive = async {
                self.archive_rx.as_mut().expect("guarded archive receiver").await.ok()
            }, if self.archive_rx.is_some() => Background::Archive(archive),
            refreshed = async {
                self.refresh_rx.as_mut().expect("guarded refresh receiver").await.ok()
            }, if self.refresh_rx.is_some() => Background::Refresh(refreshed),
            listed = self.listing_rx.recv(), if !self.listings.is_empty() => Background::Listed(listed),
            previous = async {
                self.history_rx.as_mut().expect("guarded history receiver").await.ok()
            }, if self.history_rx.is_some() => Background::History(previous),
            else => std::future::pending().await,
        }
    }

    pub(super) fn handle_background(&mut self, background: Background) {
        match background {
            Background::Scan(Some(result)) | Background::Operation(Some(result)) => {
                self.handle_scan_result(result)
            }
            Background::Scan(None) => self.handle_scan_channel_closed(),
            Background::Operation(None) => self.handle_operation_channel_closed(),
            Background::Take(take) => {
                self.take_rx = None;
                if let Some(ScanResult::TakeComplete { path, result }) = take {
                    self.finish_take(path, result);
                } else {
                    self.error =
                        Some("Create-and-enter ended without completion; outcome unknown".into());
                }
            }
            Background::Preview(preview) => self.finish_preview(preview),
            Background::Archive(archive) => self.finish_archive_task(archive),
            Background::Refresh(refreshed) => {
                self.refresh_rx = None;
                self.apply_refresh(refreshed.unwrap_or_default());
            }
            Background::History(previous) => {
                self.history_rx = None;
                self.history_baseline = previous.flatten();
            }
            // The App holds a sender, so the channel never closes.
            Background::Listed(listed) => {
                if let Some(listed) = listed {
                    self.finish_listing(listed);
                }
            }
        }
        // Changes that waited for this task can be picked up now.
        if !self.queued_refresh.is_empty() {
            self.refresh_paths(Vec::new());
        }
        self.needs_redraw = true;
    }

    /// Bring the tree up to date for `paths`, which an operation may have
    /// changed, without rescanning anything else: each one still on disk is
    /// scanned again and grafted in place, and the rest are removed.
    pub(super) fn refresh_paths(&mut self, paths: Vec<PathBuf>) {
        if self.snapshot.is_some() {
            return;
        }
        self.queued_refresh.extend(paths);
        // Wait for a running refresh, and for an operation or scan in
        // progress: one could still be changing these paths, and a full scan
        // would cancel it.
        if self.refresh_rx.is_some() || self.scan_rx.is_some() || self.op_rx.is_some() {
            return;
        }
        let Some(tree) = &self.tree else {
            self.queued_refresh.clear();
            return;
        };
        let root = tree.root_path.clone();
        if self.queued_refresh.contains(&root) {
            self.start_scan();
            return;
        }
        let mut paths: Vec<PathBuf> = std::mem::take(&mut self.queued_refresh)
            .into_iter()
            .filter(|path| path.starts_with(&root))
            .collect();
        // Ancestors sort before their descendants; refreshing one covers both.
        paths.sort();
        paths.dedup_by(|later, earlier| later.starts_with(earlier));
        if paths.is_empty() {
            return;
        }
        let config = tree.config.clone();
        let (tx, rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let refreshed = paths
                .into_iter()
                .map(|path| state::Refreshed {
                    entry: gravityfile_scan::scan_entry(&path, &config, CancellationToken::new())
                        .map_err(|error| error.to_string()),
                    path,
                })
                .collect();
            let _ = tx.send(refreshed);
        });
        self.refresh_rx = Some(rx);
    }

    /// Rescan the directory being viewed: the whole scan at the scan root,
    /// otherwise just that directory, in place.
    pub(super) fn refresh_view(&mut self) {
        self.marked.clear();
        if self.tree.is_some() {
            self.refresh_paths(vec![self.nav.view_root.clone()]);
        } else {
            self.start_scan();
        }
    }

    /// Graft re-scanned entries into the tree.
    pub(super) fn apply_refresh(&mut self, refreshed: Vec<state::Refreshed>) {
        let mut warnings = Vec::new();
        let mut errors = Vec::new();
        let mut changed = Vec::new();
        if let Some(tree) = self.tree.as_mut() {
            let tree = Arc::make_mut(tree);
            for state::Refreshed { path, entry } in refreshed {
                tree.remove(&path);
                match entry {
                    Ok(Some(entry)) => {
                        warnings.extend(entry.warnings);
                        if let Some(parent) = path.parent() {
                            // A parent outside the loaded tree has nothing to update.
                            tree.insert(parent, entry.node);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
                changed.push(path);
            }
            tree.recount();
            Self::order_new_subtree(&mut tree.root, self.sort_mode);
        }
        self.merge_scan_warnings(&warnings);
        if !errors.is_empty() {
            self.error = Some(format!("Cannot refresh {}", errors.join("; ")));
        }
        // Results derived from the previous contents no longer hold.
        self.duplicates = None;
        self.age_report = None;
        if let Some(cache) = &mut self.scanned_cache {
            cache.retain(|cached, _| {
                !changed
                    .iter()
                    .any(|path| path.starts_with(cached) || cached.starts_with(path))
            });
        }
        self.leave_vanished_view();
        self.update_cached_tree_len();
        self.update_cached_miller_len();
        if self.view == View::Treemap {
            self.update_cached_treemap_len();
        }
    }

    /// Start a background scan of the current view_root directory.
    /// Note: Scanning runs in the background while the app remains in Normal mode.
    /// The UI will detect scanning state via `scan_progress.is_some()`.
    /// If we have a quick tree, it remains visible during scanning.
    pub(super) fn start_scan(&mut self) {
        if self.refuse_in_snapshot() {
            return;
        }
        // Cancel any existing scan
        self.scan_token.cancel();
        self.scan_token = CancellationToken::new();
        // A full scan supersedes any refresh of part of the old tree.
        self.refresh_rx = None;
        self.queued_refresh.clear();

        self.scan_progress = Some(ScanProgress::new());
        // Don't clear tree - keep quick listing visible during scan
        // self.tree = None;
        self.duplicates = None;
        self.age_report = None;
        self.analyzing = false;
        self.warnings.clear();
        self.selected_warning = 0;
        // Don't reset cached lengths - keep them for navigation
        // self.cached_tree_len = 0;

        self.scan_rx = Some(scanning::start_scan(
            self.path.clone(),
            self.user_settings.show_hidden,
            self.apparent_size,
            self.user_settings.max_scan_entries,
            self.scan_token.clone(),
        ));
    }

    /// Add the finished scan to the history, and learn the latest earlier
    /// scan of the same place measured the same way, on the blocking pool.
    pub(super) fn record_history(&mut self) {
        use gravityfile_core::history::{self, Record};
        let (Some(file), Some(tree)) = (self.history_file.clone(), &self.tree) else {
            return;
        };
        let record = Record::of(tree);
        self.history_baseline = None;
        let (sender, receiver) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            // A rescan moments ago is not a useful baseline.
            let previous = history::read(&file, Some(&record.root))
                .ok()
                .and_then(|records| {
                    history::previous(&records, &record, std::time::Duration::from_secs(60))
                        .cloned()
                });
            if let Err(error) = history::append(&file, &record) {
                tracing::warn!(%error, file = %file.display(), "Cannot record the scan");
            }
            let _ = sender.send(previous);
        });
        self.history_rx = Some(receiver);
    }

    /// Stop the running scan or analysis, whose tree is being replaced.
    pub(super) fn cancel_scan(&mut self) {
        self.scan_token.cancel();
        self.scan_token = CancellationToken::new();
        self.scan_rx = None;
        self.scan_progress = None;
        self.analyzing = false;
    }

    /// Cancel the active scan or post-scan analysis without leaving the explorer.
    pub(super) fn cancel_background_work(&mut self) {
        let was_scanning = self.scan_progress.is_some();
        let was_analyzing = self.analyzing;
        self.cancel_scan();

        if was_scanning {
            self.operation_message =
                Some((true, "Scan cancelled - showing current listing".to_string()));
        } else if was_analyzing {
            self.operation_message = Some((true, "Analysis cancelled".to_string()));
        }
    }

    /// Start duplicate/age analysis for the current full scan when an analysis
    /// surface is active. Keeping this on demand avoids a large memory and I/O
    /// spike immediately after startup scans.
    pub(super) fn maybe_start_analysis_for_view(&mut self) {
        if !matches!(self.view, View::Duplicates | View::Age) {
            return;
        }
        self.start_analysis_for_current_tree();
    }

    pub(super) fn start_analysis_for_current_tree(&mut self) {
        if self.scan_progress.is_some() || self.analyzing || !self.has_full_scan {
            return;
        }
        if self.duplicates.is_some() && self.age_report.is_some() {
            return;
        }

        // Cloning the Arc, not the tree: analysis reads it concurrently.
        let Some(tree) = self.tree.as_ref().map(Arc::clone) else {
            return;
        };

        self.analyzing = true;
        // A snapshot has no file contents to compare, so only ages are analysed.
        let compare_contents = self.snapshot.is_none();
        self.scan_rx = Some(scanning::start_analysis(
            tree,
            compare_contents,
            self.scan_token.clone(),
        ));
    }

    /// Merge final warnings without quadratic path scans or loss of distinct
    /// warning kinds/messages. Borrow keys to avoid duplicating stored strings.
    pub(super) fn merge_scan_warnings(&mut self, warnings: &[gravityfile_core::ScanWarning]) {
        let mut seen: HashSet<&gravityfile_core::ScanWarning> = self.warnings.iter().collect();
        let additional: Vec<_> = warnings
            .iter()
            .filter(|warning| seen.insert(warning))
            .cloned()
            .collect();
        drop(seen);
        self.warnings.extend(additional);
    }

    /// A scan or analysis worker ended without a result.
    pub(super) fn handle_scan_channel_closed(&mut self) {
        let analyzing = self.analyzing;
        self.cancel_scan();
        self.error = Some(if analyzing {
            "Analysis ended without a result".to_string()
        } else {
            "Scan ended without a result; showing the previous listing".to_string()
        });
        self.needs_redraw = true;
    }

    /// Handle a scan result from the background task.
    pub(super) fn handle_scan_result(&mut self, result: ScanResult) {
        match result {
            ScanResult::BulkRenameComplete(result) => {
                self.op_rx = None;
                // After a failure, rollback may have left any of the viewed
                // directory's entries renamed.
                let mut touched = vec![self.nav.view_root.clone()];
                self.operation_message = Some(match result {
                    Ok(renames) => {
                        self.marked.clear();
                        let count = renames.len();
                        touched = renames
                            .iter()
                            .flat_map(|(from, to)| [from.clone(), to.clone()])
                            .collect();
                        self.undo_log.record_renames(renames);
                        (true, format!("Renamed {count} items"))
                    }
                    Err(error) => (false, format!("Bulk rename failed: {error}")),
                });
                self.mode = AppMode::Normal;
                self.refresh_paths(touched);
            }
            ScanResult::Progress(progress) => {
                self.scan_progress = Some(progress);
            }
            ScanResult::ScanComplete(Ok(tree)) => {
                let root_path = tree.root_path.clone();
                let scan_path = root_path.clone();
                let total_files = tree.stats.total_files;
                let total_dirs = tree.stats.total_dirs;
                let total_size = tree.stats.total_size;

                self.merge_scan_warnings(&tree.warnings);

                self.install_tree(tree);
                self.record_history();

                // Preserve tree state (selection, expanded) across rescans
                // Only reset if this is a different root path
                if self.nav.tree_state.expanded.is_empty()
                    || !self
                        .nav
                        .tree_state
                        .expanded
                        .iter()
                        .any(|p| p.starts_with(&root_path))
                {
                    self.nav.tree_state = TreeState::new(root_path);
                } else {
                    // Just ensure root is expanded
                    self.nav.tree_state.expand(&root_path);
                }
                self.update_cached_tree_len();
                self.update_cached_miller_len();
                self.update_cached_treemap_len();

                // Update cached parent tree for Miller columns display
                self.update_cached_parent();

                // Clamp selection to valid range
                if self.nav.tree_state.selected >= self.cached_tree_len && self.cached_tree_len > 0
                {
                    self.nav.tree_state.selected = self.cached_tree_len - 1;
                }
                self.scan_progress = None;
                self.has_full_scan = true;
                self.analyzing = false;
                self.scan_rx = None;

                // Analysis can be expensive on large scans, so run it when
                // the user actually opens an analysis view.
                self.maybe_start_analysis_for_view();

                // Dispatch scan complete hook (extract only the stats we need)
                if let Some(pm) = self.plugin_manager.clone() {
                    let view_root = self.nav.view_root.clone();
                    tokio::spawn(async move {
                        let ctx = gravityfile_plugin::HookContext::new().with_view_root(view_root);

                        let hook = gravityfile_plugin::Hook::OnScanComplete {
                            path: scan_path,
                            total_files,
                            total_dirs,
                            total_size,
                        };

                        let pm_lock = pm.read().await;
                        let _ = pm_lock.dispatch_hook(&hook, &ctx).await;
                    });
                }
            }
            ScanResult::ScanComplete(Err(e)) => {
                self.error = Some(e.to_string());
                self.scan_progress = None;
                self.scan_rx = None;
                self.analyzing = false;
            }
            ScanResult::AnalysisComplete {
                duplicates,
                age_report,
                duplicate_scope: (root, counts),
            } => {
                let duplicates = Arc::new(duplicates);
                self.duplicate_filter.seed(&duplicates, root, counts);
                self.duplicates = Some(duplicates);
                self.age_report = Some(age_report);
                self.analyzing = false;
                self.scan_rx = None;
            }
            ScanResult::AnalysisFailed(error) => {
                self.analyzing = false;
                self.scan_rx = None;
                self.error = Some(format!("Analysis failed: {error}"));
            }
            ScanResult::AnalysisCancelled => {
                self.analyzing = false;
                self.scan_rx = None;
            }
            ScanResult::DeletionProgress(progress) => {
                self.deletion_progress = Some(progress);
            }
            ScanResult::DeletionComplete {
                mode,
                deleted,
                failed,
                bytes_freed,
                errors,
            } => {
                self.set_operation_errors(gravityfile_ops::OperationType::Delete, errors);
                self.operation_message = None;
                self.deletion_progress = None;
                self.op_rx = None;

                let (success, msg) =
                    deletion::format_deletion_result(mode, deleted.len(), failed, bytes_freed);
                let touched = deleted.clone();
                if mode == gravityfile_ops::DeleteMode::Trash {
                    self.undo_log.record_trash(deleted);
                }
                self.deletion_message = Some((success, msg));
                self.mode = AppMode::Normal;
                self.refresh_paths(touched);
            }
            ScanResult::TakeComplete { path, result } => self.finish_take(path, result),
            ScanResult::OperationProgress(progress) => {
                self.operation_progress = Some(progress);
            }
            ScanResult::OperationConflict(conflict) => {
                if self.op_token.is_cancelled() {
                    conflict.resolve(gravityfile_ops::ConflictResolution::Abort);
                    return;
                }
                self.pending_conflict = Some(conflict);
                self.mode = AppMode::ConflictResolution;
            }
            ScanResult::OperationComplete {
                cancelled,
                operation_type,
                succeeded,
                failed,
                skipped,
                bytes_processed,
                errors,
                undo_info,
                undo_remaining,
                touched,
            } => {
                self.operation_progress = None;
                self.op_rx = None;
                let error_count = errors.iter().fold(0usize, |total, error| {
                    total.saturating_add(error.represented_count())
                });
                let undo_incomplete = undo_remaining.is_some();
                if let Some(entry) = undo_remaining {
                    let count = entry.operation.item_count();
                    self.undo_log.push(entry);
                    self.error = Some(format!(
                        "Undo incomplete: {count} item(s) were not restored; fix the cause and press u to retry"
                    ));
                }
                self.set_operation_errors(operation_type, errors);

                // Record in undo log if successful
                if let Some(undo) = undo_info {
                    let description = match operation_type {
                        gravityfile_ops::OperationType::Copy => {
                            format!("Copied {} items", succeeded)
                        }
                        gravityfile_ops::OperationType::Move => {
                            format!("Moved {} items", succeeded)
                        }
                        gravityfile_ops::OperationType::Rename => "Renamed item".to_string(),
                        gravityfile_ops::OperationType::CreateFile => "Created file".to_string(),
                        gravityfile_ops::OperationType::CreateDirectory => {
                            "Created directory".to_string()
                        }
                        _ => format!("{:?} operation", operation_type),
                    };
                    self.undo_log.record(undo, description);
                }

                let action = match operation_type {
                    gravityfile_ops::OperationType::Copy => "Copied",
                    gravityfile_ops::OperationType::Move => "Moved",
                    gravityfile_ops::OperationType::Delete => "Deleted",
                    gravityfile_ops::OperationType::Restore => "Restored",
                    gravityfile_ops::OperationType::Rename => "Renamed",
                    gravityfile_ops::OperationType::CreateFile => "Created file",
                    gravityfile_ops::OperationType::CreateDirectory => "Created directory",
                };

                // Skipping is a user decision, not a failure, so it is reported
                // separately and does not mark the operation unsuccessful.
                let success = failed == 0 && error_count == 0 && !cancelled && !undo_incomplete;
                let mut msg = format!("{action} {succeeded} items");
                if matches!(
                    operation_type,
                    gravityfile_ops::OperationType::Copy | gravityfile_ops::OperationType::Move
                ) && bytes_processed > 0
                {
                    msg.push_str(&format!(" ({})", crate::ui::format_size(bytes_processed)));
                }
                if undo_incomplete {
                    msg.push_str("; undo incomplete");
                }
                if skipped > 0 {
                    msg.push_str(&format!(", {skipped} skipped"));
                }
                if failed > 0 {
                    msg.push_str(&format!(", {failed} failed"));
                } else if error_count > 0 {
                    msg.push_str(&format!(", {error_count} errors"));
                }
                if cancelled {
                    msg.push_str("; operation cancelled");
                }
                self.operation_message = Some((success, msg));
                self.mode = AppMode::Normal;
                self.refresh_paths(touched);
            }
        }
    }
}
