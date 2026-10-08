//! File operations started from the explorer: copy, move, delete, rename,
//! create, undo and bulk rename, and folding their results back into the
//! tree.

use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use gravityfile_ops::TransferOptions;

use super::input::InputState;
use super::state::{AppMode, ClipboardMode, DeletionProgress, ScanResult, View};
use super::{App, bulk_edit, deletion, scanning, state};

impl App {
    /// Get ready to start a filesystem operation. A running scan carries on;
    /// analysis, whose results the operation would make stale, is cancelled.
    pub(super) fn prepare_operation(&mut self) {
        if self.analyzing {
            self.cancel_scan();
        }
        self.op_token = CancellationToken::new();
    }

    /// Release dead worker state without interpreting missing completion as success.
    pub(super) fn handle_operation_channel_closed(&mut self) {
        self.op_token.cancel();
        self.op_rx = None;
        self.deletion_progress = None;
        self.operation_progress = None;
        self.pending_conflict = None;
        self.operation_message = None;
        self.deletion_message = None;
        self.active_paste_mode = ClipboardMode::Empty;
        if matches!(
            self.mode,
            AppMode::Copying
                | AppMode::UpdatingEntry
                | AppMode::Moving
                | AppMode::Undoing
                | AppMode::Deleting
                | AppMode::BulkRenaming
                | AppMode::ConflictResolution
        ) {
            self.mode = AppMode::Normal;
        }
        self.error = Some(
            "Background operation ended without completion; outcome unknown. Inspect affected paths before retrying."
                .to_string(),
        );
        // Whatever it changed, show what is there now.
        self.refresh_paths(vec![self.nav.view_root.clone()]);
        self.needs_redraw = true;
    }

    pub(super) fn set_operation_errors(
        &mut self,
        operation_type: gravityfile_ops::OperationType,
        errors: Vec<gravityfile_ops::OperationError>,
    ) {
        self.operation_warnings = errors
            .into_iter()
            .map(|error| {
                gravityfile_core::ScanWarning::new(
                    error.path,
                    gravityfile_core::WarningKind::ReadError,
                    format!("{operation_type}: {}", error.message),
                )
            })
            .collect();
        self.selected_warning = 0;
        if !self.operation_warnings.is_empty() {
            self.view = View::Errors;
        }
    }

    /// Execute deletion of all marked items asynchronously.
    pub(super) fn execute_deletion(&mut self) {
        self.delete_auto_marked = false;
        let paths_with_sizes: Vec<(PathBuf, u64)> = self
            .marked
            .iter()
            .filter(|path| !self.is_protected(path))
            .map(|p| (p.clone(), self.get_path_size(p).unwrap_or(0)))
            .collect();

        let total = paths_with_sizes.len();
        if total == 0 {
            return;
        }

        self.marked.clear();
        self.mode = AppMode::Deleting;
        self.deletion_progress = Some(DeletionProgress {
            total,
            deleted: 0,
            failed: 0,
            bytes_freed: 0,
            current: paths_with_sizes.first().map(|(p, _)| p.clone()),
        });

        // Defer the actual start: the run loop dispatches the async `on_delete_start`
        // plugin hook, and either begins the delete or aborts it if a plugin vetoes.
        self.pending_deletion = Some(paths_with_sizes);
    }

    /// Run the `on_delete_start` plugin veto check for staged items, then either
    /// start the background deletion or abort it if a plugin prevented the default.
    pub(super) async fn finalize_deletion(&mut self, items: Vec<(PathBuf, u64)>) {
        let paths: Vec<PathBuf> = items.iter().map(|(p, _)| p.clone()).collect();
        let results = self
            .dispatch_hook_collect(gravityfile_plugin::Hook::OnDeleteStart {
                items: paths,
                use_trash: self.delete_mode == gravityfile_ops::DeleteMode::Trash,
            })
            .await;

        if gravityfile_plugin::PluginManager::any_prevent_default(&results) {
            // A plugin vetoed the deletion — roll back the optimistic UI state.
            self.deletion_progress = None;
            self.mode = AppMode::Normal;
            self.deletion_message = Some((false, "Deletion cancelled by plugin".to_string()));
            self.needs_redraw = true;
            return;
        }

        self.prepare_operation();
        self.op_rx = Some(deletion::start_deletion(items, self.delete_mode));
    }

    /// Yank (copy) selected items to clipboard.
    pub(super) fn yank_selection(&mut self) {
        let paths: Vec<PathBuf> = if self.marked.is_empty() {
            self.get_selected_path().into_iter().collect()
        } else {
            self.marked.iter().cloned().collect()
        };

        if !paths.is_empty() {
            self.clipboard.yank(paths, self.nav.view_root.clone());
            self.marked.clear();
        }
    }

    /// Cut (move) selected items to clipboard.
    pub(super) fn cut_selection(&mut self) {
        let paths: Vec<PathBuf> = if self.marked.is_empty() {
            self.get_selected_path().into_iter().collect()
        } else {
            self.marked.iter().cloned().collect()
        };

        if !paths.is_empty() {
            self.clipboard.cut(paths, self.nav.view_root.clone());
            self.marked.clear();
        }
    }

    /// Paste from clipboard to appropriate destination.
    /// - If cursor is on a directory → paste INTO that directory
    /// - If cursor is on a file → paste into the same parent directory
    pub(super) fn paste_clipboard(&mut self) {
        self.paste_with_resolution(None);
    }

    /// Start a paste. Conflicts are resolved per item by the operation itself,
    /// which pauses and asks rather than being restarted with a blanket answer.
    pub(super) fn paste_with_resolution(
        &mut self,
        resolution: Option<gravityfile_ops::ConflictResolution>,
    ) {
        if self.clipboard.is_empty() {
            return;
        }

        // Determine destination based on current selection
        let destination = self.get_paste_destination();

        let sources = self.clipboard.paths.clone();
        let mode = self.clipboard.mode;
        self.active_paste_mode = mode;
        self.prepare_operation();

        let options = TransferOptions {
            conflict_resolution: resolution,
            replace_policy: self.replace_policy,
            ..TransferOptions::default()
        };
        match mode {
            ClipboardMode::Copy => {
                self.mode = AppMode::Copying;
                let rx = gravityfile_ops::start_copy(
                    sources,
                    destination,
                    options,
                    self.op_token.clone(),
                );
                self.op_rx = Some(Self::adapt_transfer_rx(rx));
            }
            ClipboardMode::Cut => {
                self.mode = AppMode::Moving;
                let rx = gravityfile_ops::start_move(
                    sources,
                    destination,
                    options,
                    self.op_token.clone(),
                );
                self.op_rx = Some(Self::adapt_transfer_rx(rx));
                self.clipboard.clear();
            }
            ClipboardMode::Empty => {}
        }
    }

    /// Get the appropriate paste destination based on current selection.
    /// - If selected item is a directory → return that directory
    /// - If selected item is a file → return the parent directory
    /// - If nothing selected → return view_root
    pub(super) fn get_paste_destination(&self) -> PathBuf {
        // Try to get the currently selected item
        if let Some(selected_path) = self.get_selected_path() {
            // Check if the selected item is a directory
            if selected_path.is_dir() {
                return selected_path;
            } else {
                // It's a file, use its parent directory
                if let Some(parent) = selected_path.parent() {
                    return parent.to_path_buf();
                }
            }
        }
        // Default to view_root if nothing selected
        self.nav.view_root.clone()
    }

    /// Adapt a copy or move channel to the ScanResult channel.
    pub(super) fn adapt_transfer_rx(
        mut rx: mpsc::Receiver<gravityfile_ops::TransferResult>,
    ) -> mpsc::Receiver<ScanResult> {
        use gravityfile_ops::{OperationType, TransferResult, UndoableOperation};
        let (tx, result_rx) = mpsc::channel(100);
        tokio::spawn(async move {
            loop {
                let result = tokio::select! {
                    biased;
                    _ = tx.closed() => break,
                    result = rx.recv() => match result {
                        Some(result) => result,
                        None => break,
                    },
                };
                let scan_result = match result {
                    TransferResult::Progress(p) => ScanResult::OperationProgress(p),
                    TransferResult::Conflict(c) => ScanResult::OperationConflict(c),
                    TransferResult::Complete(complete) => {
                        let touched = complete
                            .touched_paths(complete.inner.operation_type == OperationType::Move);
                        let inner = complete.inner;
                        let undo_info = (!complete.completed.is_empty()).then(|| {
                            if inner.operation_type == OperationType::Move {
                                UndoableOperation::FilesMoved {
                                    moves: complete.completed,
                                }
                            } else {
                                UndoableOperation::FilesCopied {
                                    created: complete
                                        .completed
                                        .into_iter()
                                        .map(|(_, destination)| destination)
                                        .collect(),
                                }
                            }
                        });
                        ScanResult::OperationComplete {
                            cancelled: inner.cancelled,
                            operation_type: inner.operation_type,
                            succeeded: inner.succeeded,
                            failed: inner.failed,
                            skipped: inner.skipped,
                            bytes_processed: inner.bytes_processed,
                            errors: inner.errors,
                            undo_info,
                            undo_remaining: None,
                            touched,
                        }
                    }
                };
                if tx.send(scan_result).await.is_err() {
                    break;
                }
            }
        });
        result_rx
    }

    /// Handle key press during conflict resolution mode.
    ///
    /// The operation is paused waiting on this answer, so resolving one item
    /// resumes the same operation rather than restarting it. Only `Abort`
    /// stops the batch.
    pub(super) fn handle_conflict_key(&mut self, event: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        use gravityfile_ops::{ConflictKind, ConflictResolution};

        let Some(kind) = self.pending_conflict.as_ref().map(|request| request.kind()) else {
            return;
        };

        // A same-file or self-nesting conflict cannot be satisfied by
        // overwriting or renaming, so those answers are not offered.
        let is_unsatisfiable = matches!(
            kind,
            ConflictKind::SameFile | ConflictKind::SourceIsAncestor
        );

        let resolution = match event.code {
            KeyCode::Char('s') => Some(ConflictResolution::Skip),
            KeyCode::Char('S') if !is_unsatisfiable => Some(ConflictResolution::SkipAll),
            KeyCode::Char('o') if !is_unsatisfiable => Some(ConflictResolution::Overwrite),
            KeyCode::Char('O') if !is_unsatisfiable => Some(ConflictResolution::OverwriteAll),
            KeyCode::Char('r') if !is_unsatisfiable => Some(ConflictResolution::AutoRename),
            KeyCode::Esc => Some(ConflictResolution::Abort),
            // Enter acknowledges a conflict that has only one sensible answer.
            KeyCode::Enter if is_unsatisfiable => Some(ConflictResolution::Skip),
            _ => None,
        };

        let Some(resolution) = resolution else {
            return;
        };

        // Answering unblocks the operation, which then continues with the
        // remaining items and reports completion through the usual channel.
        if let Some(request) = self.pending_conflict.take() {
            request.resolve(resolution);
        }

        if matches!(resolution, ConflictResolution::Abort) {
            self.op_token.cancel();
            self.operation_message =
                Some((false, "Cancelling operation; waiting for completion".into()));
        }

        // Return to the progress view; the operation is still running.
        self.mode = match self.active_paste_mode {
            ClipboardMode::Cut => AppMode::Moving,
            _ => AppMode::Copying,
        };
    }

    /// Start rename operation for current selection.
    pub(super) fn start_rename(&mut self) {
        if let Some(path) = self.get_selected_path() {
            let Some(name) = path.file_name() else {
                self.operation_message =
                    Some((false, "The filesystem root cannot be renamed".into()));
                return;
            };
            let mut input = if let Some(name) = name.to_str() {
                InputState::with_initial(name)
            } else {
                self.operation_message = Some((
                    false,
                    "This filename is not UTF-8. Enter a new name to rename it.".into(),
                ));
                InputState::new()
            };
            input.context_path = Some(path);
            self.input_state = Some(input);
            self.mode = AppMode::Renaming;
        }
    }

    /// Start create file operation.
    pub(super) fn start_create_file(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.nav.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::CreatingFile;
    }

    /// Start create directory operation.
    pub(super) fn start_create_directory(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.nav.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::CreatingDirectory;
    }

    /// Start take operation (create directory and cd into it).
    pub(super) fn start_take(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.nav.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::Taking;
    }

    /// Execute undo of the last operation.
    pub(super) fn execute_undo(&mut self) {
        if self.mode == AppMode::Undoing {
            return;
        }
        let Some(entry) = self.undo_log.pop() else {
            let message = match self.undo_log.peek() {
                Some(entry) => format!("Cannot undo here: {}", entry.operation.undo_description()),
                None => "Nothing to undo".to_owned(),
            };
            self.operation_message = Some((false, message));
            return;
        };
        self.operation_message = Some((true, format!("Undoing: {}", entry.description)));
        self.prepare_operation();
        let mut touched = Some(entry.operation.touched_paths());
        let mut rx = gravityfile_ops::execute_undo(entry, self.op_token.clone());

        let (tx, adapted_rx) = tokio::sync::mpsc::channel(100);
        let token = self.op_token.clone();
        tokio::spawn(async move {
            loop {
                let result = tokio::select! {
                    biased;
                    _ = tx.closed() => { token.cancel(); break; }
                    result = rx.recv() => match result {
                        Some(result) => result,
                        None => break,
                    },
                };
                let scan_result = match result {
                    gravityfile_ops::UndoResult::Progress(p) => ScanResult::OperationProgress(p),
                    gravityfile_ops::UndoResult::Complete(complete) => {
                        let c = complete.inner;
                        ScanResult::OperationComplete {
                            cancelled: c.cancelled,
                            operation_type: c.operation_type,
                            succeeded: c.succeeded,
                            failed: c.failed,
                            skipped: c.skipped,
                            bytes_processed: c.bytes_processed,
                            errors: c.errors,
                            undo_info: None,
                            undo_remaining: complete.remaining,
                            touched: touched.take().unwrap_or_default(),
                        }
                    }
                };
                if tx.send(scan_result).await.is_err() {
                    token.cancel();
                    break;
                }
            }
        });
        self.op_rx = Some(adapted_rx);
        self.mode = AppMode::Undoing;
    }

    /// Execute rename operation.
    pub(super) fn execute_rename(&mut self, source: std::path::PathBuf, new_name: String) {
        use gravityfile_ops::start_rename;

        if !self.accept_name(&new_name) {
            return;
        }

        self.prepare_operation();
        let rx = start_rename(source.clone(), new_name.clone());

        // Adapt the rename result receiver
        let adapted_rx = Self::adapt_rename_rx(source, new_name.clone(), rx);
        self.op_rx = Some(adapted_rx);
        self.mode = AppMode::UpdatingEntry;

        // Show pending message
        self.operation_message = Some((true, format!("Renaming to {}...", new_name)));
    }

    /// Adapt rename receiver to ScanResult.
    pub(super) fn adapt_rename_rx(
        source_path: std::path::PathBuf,
        new_name: String,
        mut rx: mpsc::Receiver<gravityfile_ops::RenameResult>,
    ) -> mpsc::Receiver<ScanResult> {
        let (tx, adapted_rx) = mpsc::channel(100);

        tokio::spawn(async move {
            while let Some(result) = tokio::select! {
                biased;
                _ = tx.closed() => None,
                result = rx.recv() => result,
            } {
                let scan_result = match result {
                    gravityfile_ops::RenameResult::Progress(progress) => {
                        ScanResult::OperationProgress(progress)
                    }
                    gravityfile_ops::RenameResult::Complete(complete) => {
                        let parent = source_path.parent().unwrap_or(std::path::Path::new(""));
                        let renamed = parent.join(&new_name);
                        let undo_info = if complete.succeeded > 0 && complete.is_success() {
                            Some(gravityfile_ops::UndoableOperation::FilesRenamed {
                                renames: vec![(source_path.clone(), renamed.clone())],
                            })
                        } else {
                            None
                        };

                        ScanResult::OperationComplete {
                            cancelled: complete.cancelled,
                            operation_type: complete.operation_type,
                            succeeded: complete.succeeded,
                            failed: complete.failed,
                            skipped: complete.skipped,
                            bytes_processed: complete.bytes_processed,
                            errors: complete.errors,
                            undo_info,
                            undo_remaining: None,
                            touched: vec![source_path.clone(), renamed],
                        }
                    }
                };
                if tx.send(scan_result).await.is_err() {
                    break;
                }
            }
        });

        adapted_rx
    }

    /// Whether `name` is usable for a new or renamed entry. Reports the reason
    /// when it is not.
    pub(super) fn accept_name(&mut self, name: &str) -> bool {
        match gravityfile_ops::validate_filename(name) {
            Ok(()) => true,
            Err(error) => {
                self.operation_message = Some((false, error));
                false
            }
        }
    }

    /// Execute create file operation.
    pub(super) fn execute_create_file(&mut self, parent: std::path::PathBuf, name: String) {
        use gravityfile_ops::start_create_file;

        if !self.accept_name(&name) {
            return;
        }

        let path = parent.join(&name);
        self.prepare_operation();
        let rx = start_create_file(path.clone());

        let adapted_rx = Self::adapt_create_rx(path, rx);
        self.op_rx = Some(adapted_rx);

        self.operation_message = Some((true, format!("Creating file {}...", name)));
        self.mode = AppMode::UpdatingEntry;
    }

    /// Execute create directory operation.
    pub(super) fn execute_create_directory(&mut self, parent: std::path::PathBuf, name: String) {
        use gravityfile_ops::start_create_directory;

        if !self.accept_name(&name) {
            return;
        }

        let path = parent.join(&name);
        self.prepare_operation();
        let rx = start_create_directory(path.clone());

        let adapted_rx = Self::adapt_create_rx(path, rx);
        self.op_rx = Some(adapted_rx);

        self.operation_message = Some((true, format!("Creating directory {}...", name)));
        self.mode = AppMode::UpdatingEntry;
    }

    /// Execute take operation (create directory and navigate into it).
    pub(super) fn execute_take(&mut self, parent: std::path::PathBuf, name: String) {
        if self.take_rx.is_some() {
            self.operation_message = Some((false, "Create-and-enter is still in progress".into()));
            return;
        }
        if !self.accept_name(&name) {
            return;
        }
        let path = parent.join(&name);
        self.prepare_operation();
        self.take_rx = Some(scanning::start_take(path));
        self.operation_message = Some((true, format!("Creating or opening {name}...")));
    }

    pub(super) fn finish_take(&mut self, path: PathBuf, result: Result<bool, String>) {
        self.take_rx = None;
        let created = match result {
            Ok(created) => created,
            Err(error) => {
                self.operation_message =
                    Some((false, format!("Cannot create or enter directory: {error}")));
                return;
            }
        };
        if created {
            self.undo_log.record_create_directory(path.clone());
        }
        if path.parent() != Some(self.nav.view_root.as_path()) || self.mode != AppMode::Normal {
            self.operation_message = Some((true, format!("Directory ready: {}", path.display())));
            return;
        }
        self.operation_message = Some((true, format!("Entered {}", path.display())));

        // The directory is not in the tree until the refresh grafts it in.
        self.enter(path.clone(), None);
        self.refresh_paths(vec![path]);
    }

    /// Adapt create result receiver to ScanResult.
    pub(super) fn adapt_create_rx(
        path: std::path::PathBuf,
        mut rx: mpsc::Receiver<gravityfile_ops::CreateResult>,
    ) -> mpsc::Receiver<ScanResult> {
        let (tx, adapted_rx) = mpsc::channel(100);

        tokio::spawn(async move {
            while let Some(result) = tokio::select! {
                biased;
                _ = tx.closed() => None,
                result = rx.recv() => result,
            } {
                let scan_result = match result {
                    gravityfile_ops::CreateResult::Progress(progress) => {
                        ScanResult::OperationProgress(progress)
                    }
                    gravityfile_ops::CreateResult::Complete(complete) => {
                        let undo_info = if complete.succeeded > 0 && complete.is_success() {
                            match complete.operation_type {
                                gravityfile_ops::OperationType::CreateFile => {
                                    Some(gravityfile_ops::UndoableOperation::FileCreated {
                                        path: path.clone(),
                                    })
                                }
                                gravityfile_ops::OperationType::CreateDirectory => {
                                    Some(gravityfile_ops::UndoableOperation::DirectoryCreated {
                                        path: path.clone(),
                                    })
                                }
                                _ => None,
                            }
                        } else {
                            None
                        };

                        ScanResult::OperationComplete {
                            cancelled: complete.cancelled,
                            operation_type: complete.operation_type,
                            succeeded: complete.succeeded,
                            failed: complete.failed,
                            skipped: complete.skipped,
                            bytes_processed: complete.bytes_processed,
                            errors: complete.errors,
                            undo_info,
                            undo_remaining: None,
                            touched: vec![path.clone()],
                        }
                    }
                };
                if tx.send(scan_result).await.is_err() {
                    break;
                }
            }
        });

        adapted_rx
    }

    /// Start bulk rename by opening editor with marked file names.
    pub(super) fn start_bulk_rename(&mut self) {
        // Collect paths to rename
        let paths: Vec<PathBuf> = if self.marked.is_empty() {
            // If nothing marked, use current selection
            if let Some(path) = self.get_selected_path() {
                vec![path]
            } else {
                return;
            }
        } else {
            self.marked.iter().cloned().collect()
        };

        if paths.is_empty() {
            return;
        }

        let edit = match bulk_edit::BulkRenameEdit::new(paths) {
            Ok(edit) => edit,
            Err(error) => {
                self.error = Some(format!("Cannot prepare bulk rename: {error}"));
                return;
            }
        };

        let cmd = match crate::opener::editor_command(edit.path(), &self.user_settings.editor) {
            Ok(command) => command,
            Err(error) => {
                self.error = Some(format!("Cannot configure bulk rename editor: {error}"));
                return;
            }
        };

        self.bulk_rename_edit = Some(edit);

        // Set pending command to suspend terminal
        self.pending_suspend_command = Some(cmd);
    }

    /// Process bulk rename after editor closes.
    pub(super) fn process_bulk_rename_from_editor(&mut self) {
        let Some(edit) = self.bulk_rename_edit.take() else {
            return;
        };
        let new_names = match edit.read_names() {
            Ok(names) => names,
            Err(error) => {
                self.error = Some(format!("Cannot read edited rename list: {error}"));
                return;
            }
        };
        let original_paths = edit.paths.clone();

        // Create proper state with changes only
        let mut new_state = state::BulkRenameState::new(original_paths, new_names);

        if new_state.is_empty() {
            self.error = Some("No changes to apply".to_string());
            self.bulk_rename_state = None;
            return;
        }

        // Validate for conflicts (sets error field if invalid)
        new_state.validate();

        self.bulk_rename_state = Some(new_state);
        self.mode = AppMode::ConfirmBulkRename;
    }

    /// Execute the bulk rename operations.
    pub(super) fn execute_bulk_rename(&mut self) {
        let Some(bulk_state) = self.bulk_rename_state.take() else {
            return;
        };

        let renames: Vec<_> = bulk_state
            .entries
            .iter()
            .filter_map(|entry| {
                entry
                    .original
                    .parent()
                    .map(|parent| (entry.original.clone(), parent.join(&entry.new_name)))
            })
            .collect();
        self.prepare_operation();
        self.op_rx = Some(scanning::start_bulk_rename(renames, self.op_token.clone()));
        self.mode = AppMode::BulkRenaming;
        self.operation_message = Some((
            true,
            "Bulk rename in progress; waiting for completion".into(),
        ));
        self.needs_redraw = true;
    }
}
