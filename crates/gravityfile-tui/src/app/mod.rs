//! Main application state and logic.

mod archive;
mod bulk_edit;
mod commands;
mod constants;
mod deletion;
mod duplicate_filter;
pub mod input;
mod navigation;
mod render;
mod scanning;
pub mod state;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{EnableMouseCapture, Event};
use crossterm::execute;
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use gravityfile_analyze::{AgeReport, DuplicateReport};
use gravityfile_core::{FileNode, FileTree};
use gravityfile_ops::{ConflictRequest, OperationProgress, TransferOptions, UndoLog};
use gravityfile_scan::ScanProgress;

use crate::TuiConfig;
use crate::event::{KeyAction, MouseAction};
use crate::preview::{ImageContext, PreviewContent, PreviewLoader, PreviewMode, PreviewState};
use crate::search::SearchState;
use crate::theme::Theme;
use crate::ui::{TreeState, TreeView};

use self::archive::{ArchiveCancellation, ArchiveCompletion};
use self::commands::{
    CommandAction, CommandInput, CommandKeyResult, LayoutCommand, SortCommand, ThemeCommand,
    parse_command,
};
use self::constants::{PAGE_SIZE, TICK_INTERVAL_MS};
use self::input::{InputResult, InputState};
use self::state::{
    AppMode, BookmarkListState, ClipboardMode, ClipboardState, DeletionProgress,
    DuplicatesViewState, LayoutMode, ScanResult, SelectedInfo, SettingsState, SortMode, TabManager,
    UserSettings, View,
};

/// Application result type.
pub type AppResult<T> = color_eyre::Result<T>;

/// Data collected for synchronizing selection between layout modes.
struct LayoutSyncData {
    /// Current view root path.
    view_path: PathBuf,
    /// Index in miller view (when switching from tree to miller).
    miller_index: Option<usize>,
    /// Index in tree view (when switching from miller to tree).
    tree_index: Option<usize>,
    /// New view root if we need to change it to show the selected item.
    new_view_root: Option<PathBuf>,
}

struct PreviewLoad {
    path: PathBuf,
    mode: PreviewMode,
    result: Result<PreviewContent, crate::preview::PreviewError>,
}

struct PreviewRequest {
    path: PathBuf,
    mode: PreviewMode,
    image_context: Option<ImageContext>,
}

fn quick_list_with_settings(
    path: &Path,
    settings: &UserSettings,
) -> Result<FileTree, gravityfile_scan::ScanError> {
    let mut config = gravityfile_scan::ScanConfig::new(path);
    config.include_hidden = settings.show_hidden;
    config.max_entries = settings.max_scan_entries;
    gravityfile_scan::quick_list(path, Some(&config))
}

/// Main application state.
pub struct App {
    /// Path being analyzed (scan root).
    path: PathBuf,
    /// Current view root for drill-down navigation (can be different from scan root).
    view_root: PathBuf,
    /// Navigation history stack for going back up (path, selected_index, expanded_set).
    view_history: Vec<(PathBuf, usize, HashSet<PathBuf>)>,
    /// Forward navigation history stack (for returning after going back).
    forward_history: Vec<(PathBuf, usize, HashSet<PathBuf>)>,
    /// Current mode.
    mode: AppMode,
    /// Current view (normal mode).
    view: View,
    /// Color theme.
    theme: Theme,
    /// Detected terminal graphics capabilities, queried once at startup.
    ///
    /// `None` when the terminal could not be queried, in which case image
    /// files fall back to the ordinary binary preview.
    image_picker: Option<ratatui_image::picker::Picker>,
    /// Size of the preview pane at the last render, in character cells.
    ///
    /// Images are encoded to fit this, so it is captured during draw and used
    /// by the next preview load. `Cell` because rendering takes `&self`.
    preview_area: std::cell::Cell<ratatui::layout::Size>,
    /// Scroll offsets computed by the last render (tree, miller). Rendering
    /// is `&self`, so these are adopted into the view states after each frame;
    /// mouse hit-testing depends on them matching what is on screen.
    rendered_offsets: std::cell::Cell<(usize, usize)>,
    /// Scanned file tree.
    ///
    /// Shared rather than owned: `FileTree::clone` is a deep copy of every
    /// node, and this tree is handed to background analysis and stashed in the
    /// navigation cache. Mutation goes through `Arc::make_mut`, which only
    /// copies when a background task still holds a reference.
    tree: Option<Arc<FileTree>>,
    /// Tree view state.
    tree_state: TreeState,
    /// Cached count of visible tree items (to avoid flatten on every nav).
    cached_tree_len: usize,
    /// Duplicate analysis report.
    duplicates: Option<Arc<DuplicateReport>>,
    duplicate_filter: duplicate_filter::DuplicateFilter,
    /// Age analysis report.
    age_report: Option<AgeReport>,
    /// Duplicates view state (expanded groups, selected files).
    duplicates_state: DuplicatesViewState,
    /// Selected stale directory index.
    selected_stale_dir: usize,
    /// Show details panel.
    show_details: bool,
    /// Error message to display.
    error: Option<String>,
    /// Paths marked/selected for operations.
    marked: HashSet<PathBuf>,
    /// Clipboard state for yank/cut/paste.
    clipboard: ClipboardState,
    active_paste_mode: ClipboardMode,
    /// Whether the current marks were added by `d` itself (so a declined
    /// confirmation removes them) rather than by the user.
    delete_auto_marked: bool,
    /// Undo log for reversible operations.
    undo_log: UndoLog,
    /// Disposal of items replaced by an approved overwrite.
    replace_policy: gravityfile_ops::ReplacePolicy,
    /// Layout mode (tree vs miller).
    layout_mode: LayoutMode,
    /// Miller columns state.
    miller_state: crate::ui::MillerState,
    /// Cached count of entries in current directory for Miller view.
    cached_miller_len: usize,
    /// Input state for rename/create modes.
    input_state: Option<InputState>,
    /// Current file operation progress (copy/move).
    operation_progress: Option<OperationProgress>,
    /// Pending conflict requiring resolution.
    pending_conflict: Option<ConflictRequest>,
    /// Last operation result message.
    operation_message: Option<(bool, String)>,
    /// Last deletion result message.
    deletion_message: Option<(bool, String)>,
    /// Current deletion progress (during async deletion).
    deletion_progress: Option<DeletionProgress>,
    /// Current scan progress (for display during scanning).
    scan_progress: Option<ScanProgress>,
    /// Channel for receiving scan results.
    scan_rx: Option<mpsc::Receiver<ScanResult>>,
    take_rx: Option<mpsc::Receiver<ScanResult>>,
    /// Token for cancelling active scan.
    scan_token: CancellationToken,
    /// Scan warnings/errors (populated in real-time during scan).
    warnings: Vec<gravityfile_core::ScanWarning>,
    /// Latest operation diagnostics survive the automatic scan refresh.
    operation_warnings: Vec<gravityfile_core::ScanWarning>,
    /// Selected warning index.
    selected_warning: usize,
    /// Whether analysis is running.
    analyzing: bool,
    /// Whether a full recursive scan has been completed (vs just quick_list).
    has_full_scan: bool,
    /// Flag indicating UI needs redraw.
    needs_redraw: bool,
    /// Command palette input state.
    command_input: CommandInput,
    /// Current sort mode for file listings.
    sort_mode: SortMode,
    /// Fuzzy search state.
    search_state: SearchState,
    /// Directory tab manager.
    tab_manager: TabManager,
    /// File preview state.
    preview_state: PreviewState,
    /// Completion channel for the latest off-thread preview load.
    preview_rx: Option<oneshot::Receiver<PreviewLoad>>,
    pending_preview: Option<PreviewRequest>,
    /// Completion channel for a background archive operation.
    archive_rx: Option<oneshot::Receiver<ArchiveCompletion>>,
    archive_cancel: Option<ArchiveCancellation>,
    pending_archive_refresh: bool,
    /// Whether to auto-scan on startup.
    scan_on_startup: bool,
    /// User settings (persistent configuration).
    user_settings: UserSettings,
    /// Settings modal state.
    settings_state: Option<SettingsState>,
    /// Bookmark list modal state.
    bookmark_list_state: Option<BookmarkListState>,
    /// Cached parent tree for Miller columns when at tree root.
    /// This allows showing the parent column even when navigated beyond the original scan root.
    cached_parent_tree: Option<FileTree>,
    /// Pending command that requires terminal suspension (e.g., opening files in editor).
    pending_suspend_command: Option<std::process::Command>,
    /// Items staged for deletion, awaiting the async `on_delete_start` plugin
    /// veto check in the run loop before the background delete is actually started.
    pending_deletion: Option<Vec<(PathBuf, u64)>>,
    /// Cache of scanned directories by path.
    /// This preserves scan data when navigating to parent directories and allows
    /// restoring it when navigating back.
    scanned_cache: Option<HashMap<PathBuf, Arc<FileTree>>>,
    /// Bulk rename state for the confirmation modal.
    bulk_rename_state: Option<state::BulkRenameState>,
    bulk_rename_edit: Option<bulk_edit::BulkRenameEdit>,
    /// Treemap view state.
    treemap_state: crate::ui::TreemapState,
    /// Cached count of treemap rectangles for navigation.
    cached_treemap_len: usize,
    /// Visual selection mode state.
    visual_state: Option<state::VisualState>,
    /// Plugin manager.
    plugin_manager: Option<Arc<RwLock<gravityfile_plugin::PluginManager>>>,
    /// Last known terminal content-area height (rows available for the file list).
    /// Updated on every render; used for scroll/ensure_visible calculations.
    viewport_height: usize,
}

impl App {
    /// Create a new application with default config.
    /// Immediately loads a quick directory listing for instant display.
    pub fn new(path: PathBuf) -> Self {
        Self::with_config(path, TuiConfig::default())
    }

    /// Create a new application with custom config.
    /// Immediately loads a quick directory listing for instant display,
    /// and optionally starts a full recursive scan based on config.
    pub fn with_config(path: PathBuf, config: TuiConfig) -> Self {
        Self::with_loaded_settings(path, config, UserSettings::try_load())
    }

    fn with_loaded_settings(
        path: PathBuf,
        config: TuiConfig,
        loaded: std::io::Result<UserSettings>,
    ) -> Self {
        let (user_settings, settings_error) = match loaded {
            Ok(settings) => (settings, None),
            Err(error) => (
                UserSettings {
                    scan_on_startup: false,
                    ..UserSettings::default()
                },
                Some(format!(
                    "Cannot load settings; automatic scanning disabled unless explicitly requested: {error}"
                )),
            ),
        };

        // Load quick tree immediately for instant display.
        let (quick_tree, listing_error) = match quick_list_with_settings(&path, &user_settings) {
            Ok(tree) => (Some(tree), None),
            Err(error) => (
                None,
                Some(format!("Cannot list {}: {error}", path.display())),
            ),
        };

        // The quick listing resolves relative paths and directory aliases. Use
        // that same identity for every navigation owner; retaining the supplied
        // spelling would make subtree lookup reject the freshly loaded tree.
        let path = quick_tree
            .as_ref()
            .map_or(path, |tree| tree.root_path.clone());

        // Determine scan_on_startup: CLI flag (Some) overrides user settings; None defers to settings.
        let scan_on_startup = config
            .scan_on_startup
            .unwrap_or(user_settings.scan_on_startup);

        // Initialize plugin manager and register runtimes. Scope the plugin
        // filesystem API to the scan root so hook plugins can read scanned files
        // without being granted access to the whole filesystem.
        let plugin_config =
            gravityfile_plugin::PluginConfig::default().with_fs_read_roots(vec![path.clone()]);
        let mut pm_inner = gravityfile_plugin::PluginManager::new(plugin_config);
        if let Some(executable) = &config.plugin_worker_executable
            && let Err(error) = gravityfile_plugin::ProcessRuntime::new(executable.clone())
                .and_then(|runtime| pm_inner.register_runtime(Box::new(runtime)))
        {
            tracing::error!(%error, "Plugin worker registration failed");
        }

        let plugin_manager = Arc::new(RwLock::new(pm_inner));

        let mut app = Self {
            path: path.clone(),
            view_root: path.clone(),
            view_history: Vec::new(),
            forward_history: Vec::new(),
            mode: AppMode::default(),
            view: View::default(),
            theme: Theme::dark(),
            image_picker: None,
            preview_area: std::cell::Cell::new(ratatui::layout::Size::new(40, 20)),
            rendered_offsets: std::cell::Cell::new((0, 0)),
            tree: quick_tree.map(Arc::new),
            tree_state: TreeState::new(path.clone()),
            cached_tree_len: 0,
            duplicates: None,
            duplicate_filter: duplicate_filter::DuplicateFilter::default(),
            age_report: None,
            duplicates_state: DuplicatesViewState::new(),
            selected_stale_dir: 0,
            show_details: true,
            error: match (settings_error, listing_error) {
                (Some(settings), Some(listing)) => Some(format!("{settings}; {listing}")),
                (settings, listing) => settings.or(listing),
            },
            marked: HashSet::new(),
            clipboard: ClipboardState::default(),
            active_paste_mode: ClipboardMode::Empty,
            delete_auto_marked: false,
            undo_log: UndoLog::new(100),
            replace_policy: config.replace_policy,
            layout_mode: LayoutMode::default(),
            miller_state: crate::ui::MillerState::new(),
            cached_miller_len: 0,
            input_state: None,
            operation_progress: None,
            pending_conflict: None,
            operation_message: None,
            deletion_message: None,
            deletion_progress: None,
            scan_progress: None,
            scan_rx: None,
            take_rx: None,
            scan_token: CancellationToken::new(),
            warnings: Vec::new(),
            operation_warnings: Vec::new(),
            selected_warning: 0,
            analyzing: false,
            has_full_scan: false,
            needs_redraw: true,
            command_input: CommandInput::new(),
            sort_mode: SortMode::default(),
            search_state: SearchState::new(),
            tab_manager: TabManager::with_initial_tab(path, 10),
            preview_state: PreviewState::new(),
            preview_rx: None,
            pending_preview: None,
            archive_rx: None,
            archive_cancel: None,
            pending_archive_refresh: false,
            scan_on_startup,
            user_settings,
            settings_state: None,
            bookmark_list_state: None,
            cached_parent_tree: None,
            pending_suspend_command: None,
            pending_deletion: None,
            scanned_cache: Some(HashMap::new()),
            bulk_rename_state: None,
            bulk_rename_edit: None,
            treemap_state: crate::ui::TreemapState::new(),
            cached_treemap_len: 0,
            visual_state: None,
            plugin_manager: Some(plugin_manager),
            viewport_height: 40, // reasonable default before first render
        };

        // Update cached lengths for immediate navigation
        app.update_cached_tree_len();
        app.update_cached_miller_len();
        app.update_cached_treemap_len();
        // Cache parent for Miller columns display
        app.update_cached_parent();

        app
    }

    /// Initialize plugin runtimes and discover plugins.
    async fn init_plugins(&self) {
        if let Some(pm) = &self.plugin_manager {
            let mut pm = pm.write().await;
            if let Err(e) = pm.init_runtimes() {
                tracing::warn!("Plugin runtime init error: {}", e);
            }
            if let Err(e) = pm.discover_plugins().await {
                tracing::warn!("Plugin discovery error: {}", e);
            }
        }
    }

    /// Dispatch a plugin hook, ignoring any results.
    async fn dispatch_hook(&self, hook: gravityfile_plugin::Hook) {
        let _ = self.dispatch_hook_collect(hook).await;
    }

    /// Dispatch a plugin hook and return the per-plugin results.
    ///
    /// Callers that gate a host default action on plugins (e.g. deletion) use
    /// the returned results with [`PluginManager::any_prevent_default`].
    async fn dispatch_hook_collect(
        &self,
        hook: gravityfile_plugin::Hook,
    ) -> Vec<gravityfile_plugin::HookResult> {
        let theme_name = match self.theme.variant {
            crate::theme::ThemeVariant::Dark => "dark",
            crate::theme::ThemeVariant::Light => "light",
        };

        let mut ctx = gravityfile_plugin::HookContext::new()
            .with_cwd(self.path.clone())
            .with_view_root(self.view_root.clone())
            .with_theme(theme_name);

        if let Some(node) = self.get_current_node() {
            // Serialize only the node's own metadata, NOT its children.
            // Children can be enormous after a full scan and serializing
            // them causes multi-second stalls on every hook dispatch.
            let shallow = serde_json::json!({
                "name": node.name,
                "kind": node.kind,
                "size": node.size,
                "blocks": node.blocks,
                "timestamps": node.timestamps,
                "inode": node.inode,
                "content_hash": node.content_hash,
                "git_status": node.git_status,
                "child_count": node.children.len(),
            });
            if let Ok(plugin_val) = serde_json::from_value::<gravityfile_plugin::Value>(shallow) {
                ctx.set("selected_node", plugin_val);
            }
        }

        if let Some(pm) = &self.plugin_manager {
            let pm = pm.read().await;
            pm.dispatch_hook(&hook, &ctx).await
        } else {
            Vec::new()
        }
    }

    /// Get the currently selected node in the current view.
    fn get_current_node(&self) -> Option<&gravityfile_core::FileNode> {
        let tree = self.tree.as_ref()?;
        let path = self.get_selected_path()?;
        Self::find_node_at_path(&tree.root, &path, &tree.root_path)
    }

    /// Check for navigation and dispatch hook if needed.
    async fn check_navigation(&self, old_root: &std::path::Path) {
        if old_root != self.view_root {
            self.dispatch_hook(gravityfile_plugin::Hook::OnNavigate {
                from: old_root.to_path_buf(),
                to: self.view_root.clone(),
            })
            .await;
        }
    }

    pub(crate) fn image_previews_enabled(&self) -> bool {
        self.user_settings.image_previews
    }

    /// Attach the terminal's detected graphics capabilities.
    ///
    /// Queried by the caller after the terminal is in raw mode, since
    /// detection writes control sequences and reads the reply.
    pub fn with_image_picker(mut self, picker: Option<ratatui_image::picker::Picker>) -> Self {
        self.image_picker = picker;
        self
    }

    /// Run the application with async event loop.
    pub async fn run(self, terminal: DefaultTerminal) -> AppResult<()> {
        self.run_and_return_exit_data(terminal).await.map(|_| ())
    }

    /// Run the application and return exit data for shell integration.
    pub async fn run_and_return_exit_data(
        mut self,
        mut terminal: DefaultTerminal,
    ) -> AppResult<crate::ExitData> {
        self.search_state.enable_background();
        self.init_plugins().await;
        self.dispatch_hook(gravityfile_plugin::Hook::OnStartup)
            .await;

        // Start scan if configured to do so
        if self.scan_on_startup {
            self.start_scan();
        }
        // Otherwise quick list is already loaded in new() for immediate display

        let period = Duration::from_millis(TICK_INTERVAL_MS);
        let mut interval = tokio::time::interval(period);
        // Redraw the latest state; missed frames do not need to be replayed.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut events = crate::event_source::EventSource::new()?;

        while self.mode != AppMode::Quit {
            if self.needs_redraw {
                terminal.draw(|frame| self.draw(frame))?;
                self.adopt_rendered_offsets();
                self.needs_redraw = false;
            }

            let old_view_root = self.view_root.clone();

            // Keep Tokio's randomized polling order: fixed input priority can
            // starve completions even when each input drain is bounded.
            tokio::select! {
                event = events.next() => {
                    let event = event
                        .ok_or_else(|| color_eyre::eyre::eyre!("terminal event stream closed"))??;
                    self.handle_terminal_event(event);

                    // Drain a bounded batch. An unbounded drain can starve scan,
                    // operation, and preview completions while input is busy.
                    for _ in 0..32 {
                        if self.mode == AppMode::Quit {
                            break;
                        }
                        match events.try_next() {
                            Ok(Ok(event)) => self.handle_terminal_event(event),
                            Ok(Err(error)) => return Err(error.into()),
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                return Err(color_eyre::eyre::eyre!(
                                    "terminal event stream closed"
                                ));
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        }
                    }
                    self.needs_redraw = true;
                }

                result = async {
                    if let Some(rx) = &mut self.scan_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                }, if self.scan_rx.is_some() => {
                    if let Some(result) = result {
                        self.handle_scan_result(result);
                    } else {
                        self.handle_background_channel_closed();
                    }
                    self.needs_redraw = true;
                }

                take = async {
                    self.take_rx.as_mut().expect("guarded take receiver").recv().await
                }, if self.take_rx.is_some() => {
                    self.take_rx = None;
                    if let Some(ScanResult::TakeComplete { path, result }) = take {
                        self.finish_take(path, result);
                    } else {
                        self.error = Some("Create-and-enter ended without completion; outcome unknown".into());
                    }
                    self.needs_redraw = true;
                }

                preview = async {
                    if let Some(rx) = &mut self.preview_rx {
                        rx.await.ok()
                    } else {
                        std::future::pending().await
                    }
                }, if self.preview_rx.is_some() => {
                    self.finish_preview(preview);
                    self.needs_redraw = true;
                }

                archive = async {
                    if let Some(rx) = &mut self.archive_rx {
                        rx.await.ok()
                    } else {
                        std::future::pending().await
                    }
                }, if self.archive_rx.is_some() => {
                    self.finish_archive_task(archive);
                    self.needs_redraw = true;
                }

                _ = interval.tick(), if self.scan_progress.is_some() || self.deletion_progress.is_some() || self.search_state.has_background_work() => {
                    // Poll search completions without blocking input or accumulating queries.
                    self.search_state.poll_background();
                    self.needs_redraw = true;
                }
            }

            // Skip post-event work when quitting for instant exit
            if self.mode == AppMode::Quit {
                break;
            }

            self.check_navigation(&old_view_root).await;

            // Run the plugin veto check for a staged deletion, then start or abort it.
            if let Some(items) = self.pending_deletion.take() {
                self.finalize_deletion(items).await;
                self.needs_redraw = true;
            }

            // Handle pending suspend command (opening files in external apps)
            if let Some(mut cmd) = self.pending_suspend_command.take() {
                // Check if this is a bulk rename operation
                let is_bulk_rename = self.bulk_rename_edit.is_some();

                // Join the only terminal reader before handing stdin to the editor.
                // Dropping also discards queued browser input; do not drain an
                // unbounded live terminal stream while a producer can refill it.
                drop(events);
                crate::terminal_session::restore_terminal()?;

                // Run the command and wait for it to complete
                let command_result = cmd.status();

                // Reinitialize terminal
                terminal = ratatui::try_init()?;
                execute!(std::io::stdout(), EnableMouseCapture)?;
                events = crate::event_source::EventSource::new()?;

                let command_succeeded =
                    command_result.as_ref().is_ok_and(|status| status.success());
                match command_result {
                    Ok(status) if status.success() => {}
                    Ok(status) => {
                        self.error = Some(format!("External command exited with status {status}"));
                    }
                    Err(error) => {
                        self.error = Some(format!("Failed to run external command: {error}"));
                    }
                }

                // If this was bulk rename, process the edited file
                if is_bulk_rename {
                    if command_succeeded {
                        self.process_bulk_rename_from_editor();
                    } else {
                        self.bulk_rename_edit = None;
                    }
                }

                self.needs_redraw = true;
            }
        }

        self.dispatch_hook(gravityfile_plugin::Hook::OnShutdown)
            .await;

        self.scan_token.cancel();
        self.cancel_archive_operation();

        // Take heavy structures — they'll be dropped when the runtime shuts down
        // in the background, avoiding any delay on the main thread.
        let tree = self.tree.take();
        let cache = self.scanned_cache.take();
        let duplicates = self.duplicates.take();
        let age_report = self.age_report.take();
        let pm = self.plugin_manager.take();
        let parent_tree = self.cached_parent_tree.take();

        tokio::spawn(async move {
            drop(tree);
            drop(cache);
            drop(duplicates);
            drop(age_report);
            drop(pm);
            drop(parent_tree);
        });

        Ok(crate::ExitData {
            last_cwd: self.view_root,
        })
    }

    /// Start a background scan of the current view_root directory.
    /// Note: Scanning runs in the background while the app remains in Normal mode.
    /// The UI will detect scanning state via `scan_progress.is_some()`.
    /// If we have a quick tree, it remains visible during scanning.
    fn start_scan(&mut self) {
        self.pending_archive_refresh = false;
        // Cancel any existing scan
        self.scan_token.cancel();
        self.scan_token = CancellationToken::new();

        // Update the scan root to the current view_root
        self.path = self.view_root.clone();

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
            self.view_root.clone(),
            self.user_settings.show_hidden,
            self.user_settings.max_scan_entries,
            self.scan_token.clone(),
        ));
    }

    /// Transfer the background result slot to a filesystem operation.
    fn prepare_operation(&mut self) {
        self.scan_token.cancel();
        self.scan_token = CancellationToken::new();
        self.scan_rx = None;
        self.scan_progress = None;
        self.analyzing = false;
    }

    /// Cancel the active scan or post-scan analysis without leaving the explorer.
    fn cancel_background_work(&mut self) {
        self.scan_token.cancel();
        self.scan_token = CancellationToken::new();
        self.scan_rx = None;

        let was_scanning = self.scan_progress.is_some();
        let was_analyzing = self.analyzing;

        self.scan_progress = None;
        self.analyzing = false;

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
    fn maybe_start_analysis_for_view(&mut self) {
        if !matches!(self.view, View::Duplicates | View::Age) {
            return;
        }
        self.start_analysis_for_current_tree();
    }

    fn start_analysis_for_current_tree(&mut self) {
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
        self.scan_rx = Some(scanning::start_analysis(tree, self.scan_token.clone()));
    }

    /// Merge final warnings without quadratic path scans or loss of distinct
    /// warning kinds/messages. Borrow keys to avoid duplicating stored strings.
    fn merge_scan_warnings(&mut self, warnings: &[gravityfile_core::ScanWarning]) {
        let mut seen: HashSet<&gravityfile_core::ScanWarning> = self.warnings.iter().collect();
        let additional: Vec<_> = warnings
            .iter()
            .filter(|warning| seen.insert(warning))
            .cloned()
            .collect();
        drop(seen);
        self.warnings.extend(additional);
    }

    /// Release dead worker state without interpreting missing completion as success.
    fn handle_background_channel_closed(&mut self) {
        self.scan_token.cancel();
        self.scan_rx = None;
        self.scan_progress = None;
        self.deletion_progress = None;
        self.operation_progress = None;
        self.analyzing = false;
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
        self.needs_redraw = true;
    }

    /// Handle a scan result from the background task.
    fn handle_scan_result(&mut self, result: ScanResult) {
        match result {
            ScanResult::BulkRenameComplete(result) => {
                self.scan_rx = None;
                self.operation_message = Some(match result {
                    Ok(renames) => {
                        self.marked.clear();
                        let count = renames.len();
                        self.undo_log.record_renames(renames);
                        (true, format!("Renamed {count} items"))
                    }
                    Err(error) => (false, format!("Bulk rename failed: {error}")),
                });
                self.mode = AppMode::Normal;
                self.start_scan();
            }
            ScanResult::Progress(progress) => {
                self.scan_progress = Some(progress);
            }
            ScanResult::Warning(warning) => {
                self.warnings.push(warning);
            }
            ScanResult::PartialTree(tree) => {
                // Update tree with partial results during scanning
                // This allows the UI to show progressive updates as directories are scanned
                let root_path = tree.root_path.clone();
                self.tree = Some(Arc::new(tree));

                // Preserve selection position if possible
                if self.tree_state.expanded.is_empty() {
                    self.tree_state = TreeState::new(root_path.clone());
                }
                self.tree_state.expand(&root_path);
                self.update_cached_tree_len();
                self.update_cached_miller_len();
                // Keep treemap selection bounded if the user is watching the
                // treemap while partial results stream in.
                self.update_cached_treemap_len();

                // Clamp selection to valid range
                if self.tree_state.selected >= self.cached_tree_len && self.cached_tree_len > 0 {
                    self.tree_state.selected = self.cached_tree_len - 1;
                }
            }
            ScanResult::ScanComplete(Ok(tree)) => {
                let root_path = tree.root_path.clone();
                let scan_path = root_path.clone();
                let total_files = tree.stats.total_files;
                let total_dirs = tree.stats.total_dirs;
                let total_size = tree.stats.total_size;

                self.merge_scan_warnings(&tree.warnings);

                self.tree = Some(Arc::new(tree));

                // Preserve tree state (selection, expanded) across rescans
                // Only reset if this is a different root path
                if self.tree_state.expanded.is_empty()
                    || !self
                        .tree_state
                        .expanded
                        .iter()
                        .any(|p| p.starts_with(&root_path))
                {
                    self.tree_state = TreeState::new(root_path);
                } else {
                    // Just ensure root is expanded
                    self.tree_state.expand(&root_path);
                }
                self.update_cached_tree_len();
                self.update_cached_miller_len();
                self.update_cached_treemap_len();

                // Update cached parent tree for Miller columns display
                self.update_cached_parent();

                // Clamp selection to valid range
                if self.tree_state.selected >= self.cached_tree_len && self.cached_tree_len > 0 {
                    self.tree_state.selected = self.cached_tree_len - 1;
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
                    let view_root = self.view_root.clone();
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
                self.scan_rx = None;

                let (success, msg) =
                    deletion::format_deletion_result(mode, deleted.len(), failed, bytes_freed);
                if mode == gravityfile_ops::DeleteMode::Trash {
                    self.undo_log.record_trash(deleted);
                }
                self.deletion_message = Some((success, msg));
                self.mode = AppMode::Normal;

                // Refresh scan after deletion
                self.start_scan();
            }
            ScanResult::TakeComplete { path, result } => self.finish_take(path, result),
            ScanResult::OperationProgress(progress) => {
                self.operation_progress = Some(progress);
            }
            ScanResult::OperationConflict(conflict) => {
                if self.scan_token.is_cancelled() {
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
                bytes_processed: _,
                errors,
                undo_info,
                undo_remaining,
            } => {
                self.operation_progress = None;
                self.scan_rx = None;
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

                // Refresh scan after operation
                self.start_scan();
            }
        }
        if self.pending_archive_refresh && self.scan_rx.is_none() {
            self.start_scan();
        }
    }

    fn set_operation_errors(
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

    fn warning_count(&self) -> usize {
        self.operation_warnings.len() + self.warnings.len()
    }

    fn displayed_warning(&self, index: usize) -> Option<&gravityfile_core::ScanWarning> {
        if index < self.operation_warnings.len() {
            self.operation_warnings.get(index)
        } else {
            self.warnings.get(index - self.operation_warnings.len())
        }
    }

    /// Update cached tree item count.
    fn update_cached_tree_len(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            // Use flatten_count to avoid allocating a full Vec just for the length.
            self.cached_tree_len =
                TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                    .flatten_count(&self.tree_state);
        } else {
            self.cached_tree_len = 0;
        }
    }

    /// Update cached miller entry count.
    fn update_cached_miller_len(&mut self) {
        if let Some((node, _)) = self.get_view_root_node() {
            self.cached_miller_len = node.children.len();
        } else {
            self.cached_miller_len = 0;
        }
    }

    /// Update cached treemap rectangle count.
    fn update_cached_treemap_len(&mut self) {
        if let Some((node, _)) = self.get_view_root_node() {
            self.cached_treemap_len = node.children.len();
        } else {
            self.cached_treemap_len = 0;
        }
    }

    /// Get the selected treemap item (child node) if any.
    fn get_selected_treemap_child(&self) -> Option<(&FileNode, PathBuf)> {
        let (node, root_path) = self.get_view_root_node()?;

        // Sort children by size descending (same order as treemap layout)
        let mut children: Vec<&FileNode> = node.children.iter().collect();
        children.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.compare_name(b)));

        let child = children.get(self.treemap_state.selected)?;
        let path = root_path.join(child.file_name());
        Some((*child, path))
    }

    /// Drill down into selected treemap item if it's a directory.
    fn drill_into_treemap_selected(&mut self) {
        // Get selected child info
        let drill_info = {
            let Some((child, path)) = self.get_selected_treemap_child() else {
                return;
            };

            if child.is_dir() {
                Some((path, self.view_root.clone(), self.treemap_state.selected))
            } else {
                None
            }
        };

        if let Some((new_root, old_root, old_selected)) = drill_info {
            // Save current state to history
            self.view_history
                .push((old_root, old_selected, HashSet::new()));
            self.forward_history.clear();

            // Navigate to the new directory
            self.view_root = new_root;
            self.treemap_state.reset();
            self.update_cached_treemap_len();
        }
    }

    /// Navigate back in treemap view.
    fn navigate_treemap_back(&mut self) {
        if let Some((prev_root, prev_selected, _)) = self.view_history.pop() {
            // Save current state to forward history
            self.forward_history.push((
                self.view_root.clone(),
                self.treemap_state.selected,
                HashSet::new(),
            ));

            // Restore previous state
            self.view_root = prev_root;
            self.treemap_state.selected = prev_selected;
            self.update_cached_treemap_len();
        } else if self.view_root != self.path {
            // No history, but we can go to parent
            if let Some(parent) = self.view_root.parent() {
                let old_selected = self.treemap_state.selected;
                self.forward_history
                    .push((self.view_root.clone(), old_selected, HashSet::new()));
                self.view_root = parent.to_path_buf();
                self.treemap_state.reset();
                self.update_cached_treemap_len();
            }
        }
    }

    /// Synchronize selection state when toggling between Tree and Miller layouts.
    /// Maintains the same selected item across layout switches.
    fn sync_layout_selection(&mut self) {
        // Collect all data needed before mutating self
        let sync_data = self.collect_layout_sync_data();

        let Some(data) = sync_data else {
            // No view node, just toggle and reset
            self.layout_mode = self.layout_mode.toggle();
            if self.layout_mode == LayoutMode::Miller {
                self.miller_state.reset();
                self.update_cached_miller_len();
                self.update_preview();
            } else {
                self.update_cached_tree_len();
                self.clear_preview();
            }
            return;
        };

        match self.layout_mode {
            LayoutMode::Tree => {
                // Switching Tree → Miller
                self.layout_mode = LayoutMode::Miller;

                // If we need to change view_root to show the selected item
                if let Some(new_view_root) = data.new_view_root {
                    self.view_root = new_view_root;
                    self.tree_state.expand(&self.view_root);
                }

                if let Some(miller_idx) = data.miller_index {
                    self.miller_state.selected = miller_idx;
                    self.miller_state.offset = 0;
                    self.update_cached_miller_len();
                    self.miller_state.ensure_visible(self.viewport_height);
                    self.update_preview();
                } else {
                    // Fallback: reset miller state
                    self.miller_state.reset();
                    self.update_cached_miller_len();
                    self.update_preview();
                }
            }
            LayoutMode::Miller => {
                // Switching Miller → Tree
                self.layout_mode = LayoutMode::Tree;
                self.clear_preview();

                // Ensure the view_root is expanded in tree
                self.tree_state.expand(&data.view_path);

                if let Some(tree_idx) = data.tree_index {
                    self.tree_state.selected = tree_idx;
                    self.tree_state.offset = 0;
                    self.tree_state.ensure_visible(self.viewport_height);
                }
                self.update_cached_tree_len();
            }
        }
    }

    /// Collect data needed for layout synchronization without holding borrows.
    fn collect_layout_sync_data(&self) -> Option<LayoutSyncData> {
        let (view_node, view_path) = self.get_view_root_node()?;

        match self.layout_mode {
            LayoutMode::Tree => {
                // Get the currently selected path in tree view
                let items = TreeView::new(
                    view_node,
                    &view_path,
                    &self.theme,
                    &self.marked,
                    &self.clipboard,
                )
                .flatten(&self.tree_state);

                let selected_path = items
                    .get(self.tree_state.selected)
                    .map(|item| item.path.clone());

                let Some(selected) = selected_path else {
                    return Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index: None,
                        tree_index: None,
                        new_view_root: None,
                    });
                };

                // If we're on the root itself, preserve current miller selection
                if selected == view_path {
                    let miller_index = if self.miller_state.selected < view_node.children.len() {
                        Some(self.miller_state.selected)
                    } else if view_node.children.is_empty() {
                        None
                    } else {
                        Some(0)
                    };
                    return Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index,
                        tree_index: None,
                        new_view_root: None,
                    });
                }

                // Check if selected is a direct child of view_root
                let is_direct_child = selected.parent() == Some(view_path.as_path());

                if is_direct_child {
                    // Direct child - find it in miller
                    let name = selected.file_name()?;
                    let miller_index = view_node
                        .children
                        .iter()
                        .position(|c| c.file_name() == name);
                    Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index,
                        tree_index: None,
                        new_view_root: None,
                    })
                } else {
                    // Nested item - need to change view_root to the parent of selected
                    // so that Miller can display the selected item
                    let new_view_root = selected.parent()?.to_path_buf();

                    // Find the selected item's name to locate it in the new view
                    let selected_name = selected.file_name()?;

                    // We need to find the node at new_view_root to get its children
                    let tree = self.tree.as_ref()?;
                    let new_view_node =
                        Self::find_node_at_path(&tree.root, &new_view_root, &tree.root_path)?;
                    let miller_index = new_view_node
                        .children
                        .iter()
                        .position(|c| c.file_name() == selected_name);

                    Some(LayoutSyncData {
                        view_path: view_path.clone(),
                        miller_index,
                        tree_index: None,
                        new_view_root: Some(new_view_root),
                    })
                }
            }
            LayoutMode::Miller => {
                // Get the target path from miller selection
                let target_path = view_node
                    .children
                    .get(self.miller_state.selected)
                    .map(|c| view_path.join(c.file_name()));

                // Use existing tree_state expansion to find the index
                // First ensure view_path is expanded
                let mut tree_state_for_search = self.tree_state.clone();
                tree_state_for_search.expand(&view_path);

                let items = TreeView::new(
                    view_node,
                    &view_path,
                    &self.theme,
                    &self.marked,
                    &self.clipboard,
                )
                .flatten(&tree_state_for_search);

                let tree_index = target_path
                    .as_ref()
                    .and_then(|target| items.iter().position(|item| &item.path == target));

                Some(LayoutSyncData {
                    view_path: view_path.clone(),
                    miller_index: None,
                    tree_index,
                    new_view_root: None,
                })
            }
        }
    }

    /// Update the file preview for the currently selected item in Miller mode.
    fn update_preview(&mut self) {
        // Only update preview in Miller mode for non-directory items
        if self.layout_mode != LayoutMode::Miller {
            self.clear_preview();
            return;
        }

        let Some((view_node, _)) = self.get_view_root_node() else {
            self.clear_preview();
            return;
        };

        let Some(selected_child) = view_node.children.get(self.miller_state.selected) else {
            self.clear_preview();
            return;
        };

        // Only show file preview for non-directories
        if selected_child.is_dir() {
            // For directories, we show child list directly via miller columns
            self.clear_preview();
            return;
        }

        // Get the full path to the selected file
        let file_path = self.view_root.join(selected_child.file_name());

        self.schedule_preview(file_path, false);
    }

    fn schedule_preview(&mut self, path: PathBuf, force: bool) {
        if !self.preview_state.begin_update(&path, force) {
            return;
        }

        let mode = self.preview_state.mode;
        // Decoding and protocol encoding run in the background task with the
        // rest of the preview work, never inside `terminal.draw`.
        let image_context = self.image_picker.as_ref().map(|picker| ImageContext {
            picker: picker.clone(),
            area: self.preview_area.get(),
        });
        let request = PreviewRequest {
            path,
            mode,
            image_context,
        };
        if self.preview_rx.is_some() {
            self.pending_preview = Some(request);
        } else {
            self.start_preview(request);
        }
    }

    fn start_preview(&mut self, request: PreviewRequest) {
        let PreviewRequest {
            path,
            mode,
            image_context,
        } = request;
        let (tx, rx) = oneshot::channel();
        self.preview_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            let result = PreviewLoader::load_with_mode(&path, mode, image_context.as_ref());
            let _ = tx.send(PreviewLoad { path, mode, result });
        });
    }

    fn finish_preview(&mut self, preview: Option<PreviewLoad>) {
        self.preview_rx = None;
        if let Some(request) = self.pending_preview.take() {
            // A newer request supersedes this result even when path/mode match
            // (forced reload, or leaving and returning to the same selection).
            self.start_preview(request);
        } else if let Some(preview) = preview {
            self.preview_state
                .complete_update(&preview.path, preview.mode, preview.result);
        } else if self.preview_state.loading {
            self.preview_state.loading = false;
            self.preview_state.content =
                PreviewContent::Error("Preview worker ended without a result".into());
        }
    }

    fn clear_preview(&mut self) {
        // Keep observing the active load so switching views cannot spawn a
        // second worker while the first is still reading or decoding.
        self.pending_preview = None;
        self.preview_state.clear();
    }

    /// Draw the application into the given frame, updating `viewport_height` first.
    fn draw(&mut self, frame: &mut Frame) {
        // Subtract rows for header(1) + view_tabs(1) + footer(1) = 3, plus dir_tabs(1)
        // when multiple directory tabs are open.
        let chrome_rows: u16 = if self.tab_manager.len() > 1 { 5 } else { 4 };
        let h = frame.area().height.saturating_sub(chrome_rows) as usize;
        if h > 0 {
            self.viewport_height = h;
        }
        frame.render_widget(&*self, frame.area());
    }

    /// Dispatch a key event to the appropriate handler for the current mode.
    ///
    /// This is the single authoritative entry point for all key events, used
    /// by both the primary event loop and the drain loop to avoid duplication
    /// and to ensure all modes (including Visual and ConfirmBulkRename) are
    /// handled consistently everywhere.
    fn dispatch_key_event(&mut self, key_event: crossterm::event::KeyEvent) {
        if self.mode == AppMode::Command {
            self.handle_command_input(key_event);
        } else if self.mode == AppMode::Search {
            self.handle_search_input(key_event);
        } else if matches!(
            self.mode,
            AppMode::Renaming
                | AppMode::CreatingFile
                | AppMode::CreatingDirectory
                | AppMode::Taking
                | AppMode::GoingTo
        ) {
            self.handle_input_event(key_event);
        } else if self.mode == AppMode::ConflictResolution {
            self.handle_conflict_key(key_event);
        } else if matches!(
            self.mode,
            AppMode::SettingBookmark | AppMode::JumpingToBookmark
        ) {
            self.handle_bookmark_key(key_event);
        } else if self.mode == AppMode::BookmarkList {
            self.handle_bookmark_list_input(key_event);
        } else if self.mode == AppMode::ConfirmBulkRename {
            self.handle_bulk_rename_input(key_event);
        } else if self.mode == AppMode::Visual {
            self.handle_visual_mode_input(key_event);
        } else {
            let action = KeyAction::from_key_event(key_event);
            self.handle_action(action);
        }
    }

    fn handle_terminal_event(&mut self, event: Event) {
        match event {
            Event::Key(key_event) if key_event.kind == crossterm::event::KeyEventKind::Press => {
                self.dispatch_key_event(key_event);
            }
            Event::Mouse(mouse_event) if self.mode == AppMode::Normal => {
                let action = MouseAction::from_mouse_event(mouse_event);
                self.handle_mouse_action(action);
            }
            _ => {}
        }
    }

    /// Handle a key action.
    fn handle_action(&mut self, action: KeyAction) {
        // Clear deletion message on any action
        self.deletion_message = None;

        // Handle special modes
        match self.mode {
            AppMode::BulkRenaming => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.scan_token.cancel();
                    self.operation_message =
                        Some((false, "Cancelling bulk rename; waiting for rollback".into()));
                    self.needs_redraw = true;
                }
                // Keep the result channel until rollback and cleanup complete.
                return;
            }
            AppMode::Help => {
                match action {
                    KeyAction::ToggleHelp | KeyAction::Quit | KeyAction::Cancel => {
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            AppMode::Settings => {
                self.handle_settings_input(action);
                return;
            }
            AppMode::ConfirmDelete => {
                match action {
                    // 'y'/Enter both confirm deletion.
                    // KeyAction::Yank covers the 'y' key since that is its normal-mode mapping.
                    KeyAction::Confirm | KeyAction::DrillDown | KeyAction::Yank => {
                        self.execute_deletion();
                    }
                    KeyAction::Quit | KeyAction::Cancel => {
                        // Drop only the marks `d` added itself; hand-made
                        // selections survive a declined confirmation.
                        if std::mem::take(&mut self.delete_auto_marked) {
                            self.marked.clear();
                        }
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            AppMode::Deleting => {
                return;
            }
            AppMode::Command => {
                match action {
                    KeyAction::Quit | KeyAction::Cancel => {
                        self.command_input.clear();
                        self.mode = AppMode::Normal;
                    }
                    _ => {}
                }
                return;
            }
            AppMode::ConflictResolution => {
                // Handled in run loop via handle_conflict_key
                return;
            }
            AppMode::Undoing => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.scan_token.cancel();
                    self.operation_message =
                        Some((false, "Cancelling undo; waiting for completion".into()));
                    self.needs_redraw = true;
                }
                return;
            }
            AppMode::UpdatingEntry => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.operation_message = Some((
                        false,
                        "Waiting for the filesystem operation to finish".into(),
                    ));
                    self.needs_redraw = true;
                }
                return;
            }
            AppMode::Copying | AppMode::Moving => {
                if matches!(
                    action,
                    KeyAction::Cancel | KeyAction::Quit | KeyAction::ForceQuit
                ) {
                    self.scan_token.cancel();
                    self.operation_message =
                        Some((false, "Cancelling operation; waiting for completion".into()));
                    self.needs_redraw = true;
                }
                return;
            }
            _ => {}
        }

        if matches!(action, KeyAction::Cancel | KeyAction::Quit) && self.cancel_archive_operation()
        {
            return;
        }
        if matches!(action, KeyAction::ForceQuit) {
            self.cancel_archive_operation();
        }

        match action {
            KeyAction::Quit => {
                if self.archive_rx.is_some() || self.take_rx.is_some() {
                    self.error =
                        Some("Filesystem operation in progress; wait for it to finish".to_string());
                } else {
                    self.mode = AppMode::Quit;
                }
            }
            KeyAction::ForceQuit => {
                self.mode = AppMode::Quit;
            }
            KeyAction::Cancel => {
                // Acknowledge a visible error without also clearing selection
                // or cancelling work. Subsequent Esc retains normal behavior.
                if self.error.take().is_some() {
                    self.needs_redraw = true;
                } else if !self.clipboard.is_empty() {
                    self.clipboard.clear();
                } else if !self.marked.is_empty() {
                    self.marked.clear();
                } else if self.scan_progress.is_some() || self.analyzing {
                    self.cancel_background_work();
                }
                // If nothing to clear/cancel, Esc does nothing in normal mode
            }

            KeyAction::NextView => {
                self.view = self.view.next();
                if self.view == View::Treemap {
                    self.update_cached_treemap_len();
                    self.treemap_state.reset();
                }
                self.maybe_start_analysis_for_view();
            }
            KeyAction::PrevView => {
                self.view = self.view.prev();
                if self.view == View::Treemap {
                    self.update_cached_treemap_len();
                    self.treemap_state.reset();
                }
                self.maybe_start_analysis_for_view();
            }

            // Directory tabs
            KeyAction::NewDirTab => {
                // Open new tab with current view_root
                self.tab_manager.new_tab(self.view_root.clone());
            }
            KeyAction::CloseDirTab => {
                self.tab_manager.close_active_tab();
            }
            KeyAction::NextDirTab if self.tab_manager.len() > 1 => {
                // Only switch tabs if there are multiple
                self.tab_manager.next_tab();
                self.sync_from_active_tab();
            }
            KeyAction::PrevDirTab if self.tab_manager.len() > 1 => {
                // Only switch tabs if there are multiple
                self.tab_manager.prev_tab();
                self.sync_from_active_tab();
            }
            KeyAction::DirTab(n) if self.tab_manager.len() > 1 => {
                // Only switch tabs if there are multiple
                self.tab_manager.switch_to_number(n as usize);
                self.sync_from_active_tab();
            }
            KeyAction::NextDirTab | KeyAction::PrevDirTab | KeyAction::DirTab(_) => {}

            KeyAction::MoveUp => self.move_up(),
            KeyAction::MoveDown => self.move_down(),
            KeyAction::PageUp => self.page_up(),
            KeyAction::PageDown => self.page_down(),
            KeyAction::JumpToTop => self.jump_to_top(),
            KeyAction::JumpToBottom => self.jump_to_bottom(),

            KeyAction::MoveRight | KeyAction::Expand => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.expand_selected(),
                        LayoutMode::Miller => self.drill_into_miller_selected(),
                    }
                } else if self.view == View::Duplicates {
                    // l expands group in duplicates view
                    if !self
                        .duplicates_state
                        .is_expanded(self.duplicates_state.selected_group)
                    {
                        self.duplicates_state.toggle_expand();
                    }
                } else if self.view == View::Treemap {
                    // Drill into selected directory
                    self.drill_into_treemap_selected();
                }
            }
            KeyAction::MoveLeft | KeyAction::Collapse => {
                if self.view == View::Explorer {
                    match self.layout_mode {
                        LayoutMode::Tree => self.collapse_selected(),
                        LayoutMode::Miller => self.navigate_back(),
                    }
                } else if self.view == View::Duplicates {
                    // h collapses expanded group in duplicates view
                    if self
                        .duplicates_state
                        .is_expanded(self.duplicates_state.selected_group)
                    {
                        self.duplicates_state.toggle_expand();
                    }
                } else if self.view == View::Treemap {
                    // Navigate back
                    self.navigate_treemap_back();
                }
            }
            KeyAction::ToggleExpand => {
                if self.view == View::Explorer && self.layout_mode == LayoutMode::Tree {
                    self.toggle_selected();
                } else if self.view == View::Duplicates {
                    self.duplicates_state.toggle_expand();
                }
            }

            // Selection
            KeyAction::ToggleMark => {
                self.toggle_mark();
            }
            KeyAction::ClearMarks => {
                self.marked.clear();
            }
            KeyAction::EnterVisual if self.view == View::Explorer => {
                // Enter visual selection mode
                // Only supported in Explorer view for now
                let start_index = match self.layout_mode {
                    LayoutMode::Tree => self.tree_state.selected,
                    LayoutMode::Miller => self.miller_state.selected,
                };
                self.visual_state = Some(state::VisualState::new(start_index));
                self.mode = AppMode::Visual;
            }
            KeyAction::EnterVisual => {}

            // Clipboard operations
            KeyAction::Yank => {
                self.yank_selection();
            }
            KeyAction::Cut => {
                self.cut_selection();
            }
            KeyAction::Paste => {
                self.paste_clipboard();
            }

            // File operations
            KeyAction::Delete => {
                // If nothing marked, auto-mark items for deletion based on current view
                self.delete_auto_marked = self.marked.is_empty();
                if self.marked.is_empty() {
                    if self.view == View::Duplicates {
                        // In duplicates view, special handling
                        // Collect paths first to avoid borrow issues
                        let paths_to_mark: Vec<PathBuf> =
                            if let Some((filtered, _)) = self.get_filtered_duplicates() {
                                let is_header = !self
                                    .duplicates_state
                                    .is_expanded(self.duplicates_state.selected_group)
                                    || self
                                        .duplicates_state
                                        .selected_item(self.duplicates_state.selected_group)
                                        == 0;

                                if is_header {
                                    // Mark all duplicates except the first (the "original")
                                    filtered
                                        .get(self.duplicates_state.selected_group)
                                        .map(|g| g.paths.iter().skip(1).cloned().collect())
                                        .unwrap_or_default()
                                } else {
                                    // Mark the selected file
                                    self.duplicates_state
                                        .selected_file_path(&filtered)
                                        .cloned()
                                        .into_iter()
                                        .collect()
                                }
                            } else {
                                Vec::new()
                            };
                        for path in paths_to_mark {
                            self.mark(path);
                        }
                    } else if self.view == View::Errors {
                        // Only broken symlinks are deletable from the Errors view.
                        if let Some(warning) = self.displayed_warning(self.selected_warning)
                            && warning.kind == gravityfile_core::WarningKind::BrokenSymlink
                        {
                            let path = warning.path.clone();
                            self.mark(path);
                        }
                    } else if let Some(path) = self.get_selected_path()
                        && !self.mark(path)
                    {
                        self.operation_message = Some((
                            false,
                            "The scanned or viewed directory cannot be deleted from here"
                                .to_string(),
                        ));
                    }
                }
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                }
            }
            KeyAction::Rename => {
                self.start_rename();
            }
            KeyAction::CreateFile => {
                self.start_create_file();
            }
            KeyAction::CreateDirectory => {
                self.start_create_directory();
            }
            KeyAction::Take => {
                self.start_take();
            }
            KeyAction::GoTo => {
                self.start_goto();
            }
            KeyAction::Undo => {
                self.execute_undo();
            }

            KeyAction::Confirm => {
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                } else if self.view == View::Explorer {
                    self.toggle_selected();
                }
            }

            KeyAction::DrillDown | KeyAction::OpenFile if self.view == View::Explorer => {
                // Check if selected item is a file or directory
                let is_file = self.is_selected_file();
                if is_file {
                    self.open_selected_file();
                } else {
                    match self.layout_mode {
                        LayoutMode::Tree => self.drill_into_selected(),
                        LayoutMode::Miller => self.drill_into_miller_selected(),
                    }
                }
            }
            KeyAction::DrillDown | KeyAction::OpenFile if self.view == View::Treemap => {
                if self.is_selected_file() {
                    self.open_selected_file();
                } else {
                    self.drill_into_treemap_selected();
                }
            }
            KeyAction::DrillDown | KeyAction::OpenFile => {}
            KeyAction::NavigateBack if self.view == View::Explorer => {
                self.navigate_back();
            }
            KeyAction::NavigateBack => {}

            KeyAction::CommandMode => {
                self.command_input.clear();
                self.mode = AppMode::Command;
            }

            KeyAction::ToggleDetails => {
                self.show_details = !self.show_details;
            }
            KeyAction::ToggleHelp => {
                self.mode = AppMode::Help;
            }
            KeyAction::ToggleTheme => {
                self.theme = self.theme.toggle();
            }
            KeyAction::ToggleLayout => {
                self.sync_layout_selection();
            }
            KeyAction::CyclePreviewMode => {
                self.preview_state.cycle_mode();
                if let Some(path) = self.preview_state.path.clone() {
                    self.schedule_preview(path, true);
                } else {
                    self.update_preview();
                }
            }
            KeyAction::OpenSettings => {
                self.settings_state = Some(SettingsState::new(self.user_settings.clone()));
                self.mode = AppMode::Settings;
            }
            KeyAction::Refresh => {
                self.marked.clear();
                self.start_scan();
            }
            KeyAction::Sort => {
                self.cycle_sort_mode();
            }
            KeyAction::ReverseSort => {
                self.reverse_sort_mode();
            }
            KeyAction::Search => {
                self.start_search();
            }

            // Bookmarks
            KeyAction::SetBookmark => {
                self.mode = AppMode::SettingBookmark;
            }
            KeyAction::JumpToBookmark => {
                self.mode = AppMode::JumpingToBookmark;
            }
            KeyAction::ShowBookmarkList => {
                self.bookmark_list_state =
                    Some(BookmarkListState::new(&self.user_settings.bookmarks));
                self.mode = AppMode::BookmarkList;
            }

            _ => {}
        }
    }

    /// Toggle marking the currently selected item.
    fn toggle_mark(&mut self) {
        let path = match self.view {
            View::Explorer => {
                if let Some((node, root_path)) = self.get_view_root_node() {
                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let items = TreeView::new(
                                node,
                                &root_path,
                                &self.theme,
                                &self.marked,
                                &self.clipboard,
                            )
                            .flatten(&self.tree_state);
                            items.get(self.tree_state.selected).map(|i| i.path.clone())
                        }
                        LayoutMode::Miller => node
                            .children
                            .get(self.miller_state.selected)
                            .map(|child| self.view_root.join(child.file_name())),
                    }
                } else {
                    None
                }
            }
            View::Duplicates => {
                // Hold the immutable report independently of App so marking can
                // mutate the selection set without cloning the group's paths.
                let Some(report) = self.duplicates.clone() else {
                    return;
                };
                let Some(group_index) = self
                    .duplicate_filter
                    .get(&report, &self.view_root, &self.path)
                    .groups
                    .get(self.duplicates_state.selected_group)
                    .map(|(index, _)| *index)
                else {
                    return;
                };
                let group = &report.groups[group_index];
                let selected = self
                    .duplicates_state
                    .selected_item(self.duplicates_state.selected_group);
                let is_on_header = !self
                    .duplicates_state
                    .is_expanded(self.duplicates_state.selected_group)
                    || selected == 0;
                if is_on_header {
                    if group.paths.iter().any(|path| self.marked.contains(path)) {
                        for path in &group.paths {
                            self.marked.remove(path);
                        }
                    } else {
                        // Always keep one copy, matching `d` on a header.
                        for path in group.paths.iter().skip(1) {
                            self.mark(path.clone());
                        }
                    }
                    return;
                }
                group.paths.get(selected - 1).cloned()
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let selected = self
                        .selected_stale_dir
                        .min(filtered.len().saturating_sub(1));
                    filtered.get(selected).map(|d| d.path.clone())
                } else {
                    None
                }
            }
            View::Errors => {
                // Only allow marking broken symlinks for deletion
                if let Some(warning) = self.displayed_warning(self.selected_warning) {
                    if warning.kind == gravityfile_core::WarningKind::BrokenSymlink {
                        Some(warning.path.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            View::Treemap => self.get_selected_treemap_child().map(|(_, path)| path),
        };

        if let Some(path) = path
            && !self.marked.remove(&path)
        {
            self.mark(path);
        }
    }

    /// Execute deletion of all marked items asynchronously.
    fn execute_deletion(&mut self) {
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
    async fn finalize_deletion(&mut self, items: Vec<(PathBuf, u64)>) {
        let paths: Vec<PathBuf> = items.iter().map(|(p, _)| p.clone()).collect();
        let results = self
            .dispatch_hook_collect(gravityfile_plugin::Hook::OnDeleteStart {
                items: paths,
                // `start_deletion` uses the recoverable trash by default.
                use_trash: true,
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
        self.scan_rx = Some(deletion::start_deletion(items));
    }

    /// Get the size of a path from the tree.
    fn get_path_size(&self, target: &Path) -> Option<u64> {
        let tree = self.tree.as_ref()?;

        fn find_size(
            node: &gravityfile_core::FileNode,
            target: &Path,
            current: &Path,
        ) -> Option<u64> {
            if current == target {
                return Some(node.size);
            }
            for child in &node.children {
                let child_path = current.join(child.file_name());
                if let Some(size) = find_size(child, target, &child_path) {
                    return Some(size);
                }
            }
            None
        }

        find_size(&tree.root, target, &tree.root_path)
    }

    /// Yank (copy) selected items to clipboard.
    fn yank_selection(&mut self) {
        let paths: Vec<PathBuf> = if self.marked.is_empty() {
            self.get_selected_path().into_iter().collect()
        } else {
            self.marked.iter().cloned().collect()
        };

        if !paths.is_empty() {
            self.clipboard.yank(paths, self.view_root.clone());
            self.marked.clear();
        }
    }

    /// Cut (move) selected items to clipboard.
    fn cut_selection(&mut self) {
        let paths: Vec<PathBuf> = if self.marked.is_empty() {
            self.get_selected_path().into_iter().collect()
        } else {
            self.marked.iter().cloned().collect()
        };

        if !paths.is_empty() {
            self.clipboard.cut(paths, self.view_root.clone());
            self.marked.clear();
        }
    }

    /// Paste from clipboard to appropriate destination.
    /// - If cursor is on a directory → paste INTO that directory
    /// - If cursor is on a file → paste into the same parent directory
    fn paste_clipboard(&mut self) {
        self.paste_with_resolution(None);
    }

    /// Start a paste. Conflicts are resolved per item by the operation itself,
    /// which pauses and asks rather than being restarted with a blanket answer.
    fn paste_with_resolution(&mut self, resolution: Option<gravityfile_ops::ConflictResolution>) {
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
                    self.scan_token.clone(),
                );
                self.scan_rx = Some(Self::adapt_transfer_rx(rx));
            }
            ClipboardMode::Cut => {
                self.mode = AppMode::Moving;
                let rx = gravityfile_ops::start_move(
                    sources,
                    destination,
                    options,
                    self.scan_token.clone(),
                );
                self.scan_rx = Some(Self::adapt_transfer_rx(rx));
                self.clipboard.clear();
            }
            ClipboardMode::Empty => {}
        }
    }

    /// Get the appropriate paste destination based on current selection.
    /// - If selected item is a directory → return that directory
    /// - If selected item is a file → return the parent directory
    /// - If nothing selected → return view_root
    fn get_paste_destination(&self) -> PathBuf {
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
        self.view_root.clone()
    }

    /// Adapt a copy or move channel to the ScanResult channel.
    fn adapt_transfer_rx(
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
    fn handle_conflict_key(&mut self, event: crossterm::event::KeyEvent) {
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
            self.scan_token.cancel();
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
    fn start_rename(&mut self) {
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
    fn start_create_file(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::CreatingFile;
    }

    /// Start create directory operation.
    fn start_create_directory(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::CreatingDirectory;
    }

    /// Start take operation (create directory and cd into it).
    fn start_take(&mut self) {
        let mut input = InputState::new();
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::Taking;
    }

    /// Start go to directory operation.
    fn start_goto(&mut self) {
        // Pre-fill with current view root for convenience
        let current_path = self.view_root.to_string_lossy().to_string();
        let mut input = InputState::with_initial(&current_path);
        input.context_path = Some(self.view_root.clone());
        self.input_state = Some(input);
        self.mode = AppMode::GoingTo;
    }

    /// Execute go to directory.
    fn execute_goto(&mut self, path_str: &str) {
        let path = if let Some(rest) = path_str.strip_prefix('~') {
            // Expand tilde
            if let Some(home) = dirs::home_dir() {
                home.join(rest.trim_start_matches('/'))
            } else {
                std::path::PathBuf::from(path_str)
            }
        } else if path_str.starts_with('/') {
            std::path::PathBuf::from(path_str)
        } else {
            // Relative path from current view root
            self.view_root.join(path_str)
        };

        // Canonicalize to resolve .. and .
        let path = path.canonicalize().unwrap_or(path);

        if path.is_dir() {
            // Check if target path is within our already-scanned tree
            let within_scanned_tree = if let Some(tree) = &self.tree {
                path.starts_with(&tree.root_path) || tree.root_path.starts_with(&path)
            } else {
                false
            };

            if within_scanned_tree {
                // Navigate within the existing tree - no rescan needed
                self.view_root = path.clone();
                self.view_history.clear();
                self.forward_history.clear();
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
                self.miller_state.reset();
                self.update_cached_tree_len();
                self.update_cached_miller_len();
                self.clear_preview();

                // Update the active tab view_root but keep the tree
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.view_root = self.view_root.clone();
                }
            } else {
                // Going to a new location - use quick_list for immediate display
                // Don't auto-scan; user can press R to scan if needed
                self.path = path.clone();
                self.view_root = path.clone();
                self.view_history.clear();
                self.forward_history.clear();
                self.tree_state = TreeState::new(self.path.clone());
                self.miller_state.reset();
                self.marked.clear();
                self.clipboard.clear();
                self.duplicates = None;
                self.age_report = None;
                self.warnings.clear();
                self.clear_preview();
                self.scan_progress = None;

                // Load quick tree for immediate display
                self.tree = self.load_directory_listing(&path).map(Arc::new);
                self.update_cached_tree_len();
                self.update_cached_miller_len();

                // Update the active tab
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.path = self.path.clone();
                    tab.view_root = self.view_root.clone();
                    tab.label = self
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| self.path.to_string_lossy().to_string());
                    tab.history.clear();
                }

                // Don't auto-start scan - user can press R when ready
            }
        } else {
            self.error = Some(format!("Not a directory: {}", path.display()));
        }
    }

    /// Execute undo of the last operation.
    fn execute_undo(&mut self) {
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
        let mut rx = gravityfile_ops::execute_undo(entry, self.scan_token.clone());

        let (tx, adapted_rx) = tokio::sync::mpsc::channel(100);
        let token = self.scan_token.clone();
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
                        }
                    }
                };
                if tx.send(scan_result).await.is_err() {
                    token.cancel();
                    break;
                }
            }
        });
        self.scan_rx = Some(adapted_rx);
        self.mode = AppMode::Undoing;
    }

    /// Handle key events in input modes (Renaming, CreatingFile, CreatingDirectory).
    fn handle_input_event(&mut self, key: crossterm::event::KeyEvent) {
        let current_mode = self.mode;

        if let Some(ref mut input) = self.input_state {
            match input.handle_key(key) {
                InputResult::Continue => {}
                InputResult::Cancel => {
                    self.input_state = None;
                    self.mode = AppMode::Normal;
                }
                InputResult::Submit(value) => {
                    let context_path = input.context_path.clone();
                    self.input_state = None;
                    self.mode = AppMode::Normal;

                    match current_mode {
                        AppMode::Renaming => {
                            if let Some(source) = context_path {
                                self.execute_rename(source, value);
                            }
                        }
                        AppMode::CreatingFile => {
                            if let Some(parent) = context_path {
                                self.execute_create_file(parent, value);
                            }
                        }
                        AppMode::CreatingDirectory => {
                            if let Some(parent) = context_path {
                                self.execute_create_directory(parent, value);
                            }
                        }
                        AppMode::Taking => {
                            if let Some(parent) = context_path {
                                self.execute_take(parent, value);
                            }
                        }
                        AppMode::GoingTo => {
                            self.execute_goto(&value);
                        }
                        _ => {}
                    }
                }
            }
        } else {
            // No input state, just cancel
            self.mode = AppMode::Normal;
        }
    }

    /// Execute rename operation.
    fn execute_rename(&mut self, source: std::path::PathBuf, new_name: String) {
        use gravityfile_ops::start_rename;

        // Validate name
        if new_name.is_empty() || new_name.contains('/') || new_name.contains('\\') {
            self.operation_message = Some((false, "Invalid file name".to_string()));
            return;
        }

        self.prepare_operation();
        let rx = start_rename(source.clone(), new_name.clone());

        // Adapt the rename result receiver
        let adapted_rx = Self::adapt_rename_rx(source, new_name.clone(), rx);
        self.scan_rx = Some(adapted_rx);
        self.mode = AppMode::UpdatingEntry;

        // Show pending message
        self.operation_message = Some((true, format!("Renaming to {}...", new_name)));
    }

    /// Adapt rename receiver to ScanResult.
    fn adapt_rename_rx(
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
                        let undo_info = if complete.succeeded > 0 && complete.is_success() {
                            let parent = source_path.parent().unwrap_or(std::path::Path::new(""));
                            Some(gravityfile_ops::UndoableOperation::FilesRenamed {
                                renames: vec![(source_path.clone(), parent.join(&new_name))],
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

    /// Execute create file operation.
    fn execute_create_file(&mut self, parent: std::path::PathBuf, name: String) {
        use gravityfile_ops::start_create_file;

        // Validate name
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            self.operation_message = Some((false, "Invalid file name".to_string()));
            return;
        }

        let path = parent.join(&name);
        self.prepare_operation();
        let rx = start_create_file(path.clone());

        let adapted_rx = Self::adapt_create_rx(path, rx);
        self.scan_rx = Some(adapted_rx);

        self.operation_message = Some((true, format!("Creating file {}...", name)));
        self.mode = AppMode::UpdatingEntry;
    }

    /// Execute create directory operation.
    fn execute_create_directory(&mut self, parent: std::path::PathBuf, name: String) {
        use gravityfile_ops::start_create_directory;

        // Validate name
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            self.operation_message = Some((false, "Invalid directory name".to_string()));
            return;
        }

        let path = parent.join(&name);
        self.prepare_operation();
        let rx = start_create_directory(path.clone());

        let adapted_rx = Self::adapt_create_rx(path, rx);
        self.scan_rx = Some(adapted_rx);

        self.operation_message = Some((true, format!("Creating directory {}...", name)));
        self.mode = AppMode::UpdatingEntry;
    }

    /// Execute take operation (create directory and navigate into it).
    fn execute_take(&mut self, parent: std::path::PathBuf, name: String) {
        if self.take_rx.is_some() {
            self.operation_message = Some((false, "Create-and-enter is still in progress".into()));
            return;
        }
        if name.contains('/') || name.contains('\\') {
            self.operation_message = Some((false, "Invalid directory name".into()));
            return;
        }
        if let Err(error) = gravityfile_ops::validate_filename(&name) {
            self.operation_message = Some((false, error));
            return;
        }
        let path = parent.join(&name);
        self.prepare_operation();
        self.take_rx = Some(scanning::start_take(path));
        self.operation_message = Some((true, format!("Creating or opening {name}...")));
    }

    /// Start a new history branch using the cursor of the active layout.
    fn record_navigation_origin(&mut self) {
        let selected = match self.layout_mode {
            LayoutMode::Tree => self.tree_state.selected,
            LayoutMode::Miller => self.miller_state.selected,
        };
        self.view_history.push((
            self.view_root.clone(),
            selected,
            self.tree_state.expanded.clone(),
        ));
        self.forward_history.clear();
    }

    fn finish_take(&mut self, path: PathBuf, result: Result<bool, String>) {
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
        if path.parent() != Some(self.view_root.as_path()) || self.mode != AppMode::Normal {
            self.operation_message = Some((true, format!("Directory ready: {}", path.display())));
            return;
        }
        self.operation_message = Some((true, format!("Entered {}", path.display())));

        // Navigate into the new directory
        // Save current state to history
        self.record_navigation_origin();

        // Update view root to the new directory
        self.view_root = path;

        // Reset state for new directory
        match self.layout_mode {
            LayoutMode::Tree => {
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
            }
            LayoutMode::Miller => {
                self.miller_state.reset();
            }
        }

        self.sync_to_active_tab();

        // Refresh to show new directory in tree
        self.start_scan();
    }

    /// Adapt create result receiver to ScanResult.
    fn adapt_create_rx(
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

    // Navigation methods
    fn move_up(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.tree_state.move_up(1),
                LayoutMode::Miller => {
                    self.miller_state.move_up(1);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Get counts first to avoid borrow issues
                let (group_count, file_counts) =
                    if let Some((filtered, _)) = self.get_filtered_duplicates() {
                        let counts: Vec<usize> = filtered.iter().map(|g| g.paths.len()).collect();
                        (filtered.len(), counts)
                    } else {
                        (0, Vec::new())
                    };
                if group_count > 0 {
                    self.duplicates_state.move_up(group_count, |idx| {
                        file_counts.get(idx).copied().unwrap_or(0)
                    });
                }
            }
            View::Age => {
                self.selected_stale_dir = self.selected_stale_dir.saturating_sub(1);
            }
            View::Errors => {
                self.selected_warning = self.selected_warning.saturating_sub(1);
            }
            View::Treemap => {
                self.treemap_state.move_prev();
            }
        }
    }

    fn move_down(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.move_down(1, self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.miller_state.move_down(1, self.cached_miller_len);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Get counts first to avoid borrow issues
                let (group_count, file_counts) =
                    if let Some((filtered, _)) = self.get_filtered_duplicates() {
                        let counts: Vec<usize> = filtered.iter().map(|g| g.paths.len()).collect();
                        (filtered.len(), counts)
                    } else {
                        (0, Vec::new())
                    };
                if group_count > 0 {
                    self.duplicates_state.move_down(group_count, |idx| {
                        file_counts.get(idx).copied().unwrap_or(0)
                    });
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_stale_dir = (self.selected_stale_dir + 1).min(max);
                }
            }
            View::Errors => {
                let max = self.warning_count().saturating_sub(1);
                self.selected_warning = (self.selected_warning + 1).min(max);
            }
            View::Treemap => {
                self.treemap_state.move_next(self.cached_treemap_len);
            }
        }
    }

    fn page_up(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.tree_state.move_up(PAGE_SIZE),
                LayoutMode::Miller => {
                    self.miller_state.move_up(PAGE_SIZE);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Page up - move multiple groups up
                self.duplicates_state.selected_group = self
                    .duplicates_state
                    .selected_group
                    .saturating_sub(PAGE_SIZE);
            }
            View::Age => {
                self.selected_stale_dir = self.selected_stale_dir.saturating_sub(PAGE_SIZE);
            }
            View::Errors => {
                self.selected_warning = self.selected_warning.saturating_sub(PAGE_SIZE);
            }
            View::Treemap => {
                // Page up in treemap - move selection backward by PAGE_SIZE
                for _ in 0..PAGE_SIZE {
                    self.treemap_state.move_prev();
                }
            }
        }
    }

    fn page_down(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.move_down(PAGE_SIZE, self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.miller_state
                        .move_down(PAGE_SIZE, self.cached_miller_len);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                // Page down - move multiple groups down
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    let max = filtered.len().saturating_sub(1);
                    self.duplicates_state.selected_group =
                        (self.duplicates_state.selected_group + PAGE_SIZE).min(max);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    let max = filtered.len().saturating_sub(1);
                    self.selected_stale_dir = (self.selected_stale_dir + PAGE_SIZE).min(max);
                }
            }
            View::Errors => {
                let max = self.warning_count().saturating_sub(1);
                self.selected_warning = (self.selected_warning + PAGE_SIZE).min(max);
            }
            View::Treemap => {
                // Page down in treemap - move selection forward by PAGE_SIZE
                for _ in 0..PAGE_SIZE {
                    self.treemap_state.move_next(self.cached_treemap_len);
                }
            }
        }
    }

    fn jump_to_top(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => self.tree_state.jump_to_top(),
                LayoutMode::Miller => {
                    self.miller_state.jump_to_top();
                    self.update_preview();
                }
            },
            View::Duplicates => self.duplicates_state.reset(),
            View::Age => self.selected_stale_dir = 0,
            View::Errors => self.selected_warning = 0,
            View::Treemap => self.treemap_state.reset(),
        }
    }

    fn jump_to_bottom(&mut self) {
        match self.view {
            View::Explorer => match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.jump_to_bottom(self.cached_tree_len);
                }
                LayoutMode::Miller => {
                    self.miller_state.jump_to_bottom(self.cached_miller_len);
                    self.update_preview();
                }
            },
            View::Duplicates => {
                if let Some((filtered, _)) = self.get_filtered_duplicates() {
                    self.duplicates_state.selected_group = filtered.len().saturating_sub(1);
                }
            }
            View::Age => {
                if let Some(filtered) = self.get_filtered_stale_dirs() {
                    self.selected_stale_dir = filtered.len().saturating_sub(1);
                }
            }
            View::Errors => {
                self.selected_warning = self.warning_count().saturating_sub(1);
            }
            View::Treemap => {
                // Jump to last item in treemap
                self.treemap_state.selected = self.cached_treemap_len.saturating_sub(1);
            }
        }
    }

    fn expand_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                .flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                // Lazy load directory contents if directory has no children
                if matches!(item.node.kind, crate::ui::VisibleNodeKind::Directory { .. }) {
                    let needs_lazy_load = if let Some(tree) = &self.tree {
                        if let Some(node) =
                            Self::find_node_at_path(&tree.root, &item.path, &tree.root_path)
                        {
                            node.children.is_empty()
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if needs_lazy_load {
                        self.lazy_load_directory(&item.path);
                    }
                }
                self.tree_state.expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    /// Collapse the selected item, or navigate back if already collapsed/not a directory.
    /// This provides vim-style navigation where 'h' goes up when there's nothing to collapse.
    fn collapse_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                .flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                // Check if this is an expanded directory
                let is_expanded_dir = matches!(
                    item.node.kind,
                    crate::ui::VisibleNodeKind::Directory { expanded: true }
                );

                if is_expanded_dir {
                    // Collapse the directory
                    self.tree_state.collapse(&item.path);
                    self.update_cached_tree_len();
                } else {
                    // Not expanded or not a directory - navigate back to parent
                    self.navigate_back();
                }
            }
        }
    }

    fn toggle_selected(&mut self) {
        if let Some((node, root_path)) = self.get_view_root_node() {
            let items = TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                .flatten(&self.tree_state);
            if let Some(item) = items.get(self.tree_state.selected) {
                self.tree_state.toggle_expand(&item.path);
                self.update_cached_tree_len();
            }
        }
    }

    fn get_selected_info(&self) -> Option<SelectedInfo> {
        let path = self.get_selected_path()?;
        let tree = self.tree.as_ref()?;
        let node = Self::find_node_at_path(&tree.root, &path, &tree.root_path)?;
        Some(SelectedInfo {
            name: node.display_name().into_owned(),
            path,
            size: node.size,
            file_count: node.file_count(),
            dir_count: node.dir_count(),
            modified: node.timestamps.modified,
            is_dir: node.is_dir(),
        })
    }

    /// Check the active view's selection, never an inactive explorer cursor.
    fn is_selected_file(&self) -> bool {
        self.get_selected_info().is_some_and(|info| !info.is_dir)
    }

    /// Get the path of the currently selected item.
    fn get_selected_path(&self) -> Option<PathBuf> {
        match self.view {
            View::Explorer => {}
            View::Treemap => return self.get_selected_treemap_child().map(|(_, path)| path),
            View::Errors => {
                return self
                    .displayed_warning(self.selected_warning)
                    .map(|warning| warning.path.clone());
            }
            View::Duplicates => {
                let (groups, _) = self.get_filtered_duplicates()?;
                return self.duplicates_state.selected_file_path(&groups).cloned();
            }
            View::Age => {
                let directories = self.get_filtered_stale_dirs()?;
                return directories
                    .get(self.selected_stale_dir)
                    .map(|directory| directory.path.clone());
            }
        }

        match self.layout_mode {
            LayoutMode::Tree => {
                if let Some((view_node, view_path)) = self.get_view_root_node() {
                    let items = TreeView::new(
                        view_node,
                        &view_path,
                        &self.theme,
                        &self.marked,
                        &self.clipboard,
                    )
                    .flatten(&self.tree_state);
                    return items
                        .get(self.tree_state.selected)
                        .map(|item| item.path.clone());
                }
                None
            }
            LayoutMode::Miller => {
                if let Some((view_node, _)) = self.get_view_root_node()
                    && let Some(child) = view_node.children.get(self.miller_state.selected)
                {
                    return Some(self.view_root.join(child.file_name()));
                }
                None
            }
        }
    }

    /// Open the currently selected file with the configured opener.
    fn open_selected_file(&mut self) {
        let Some(path) = self.get_selected_path() else {
            return;
        };

        // Get opener configuration
        let openers = &self.user_settings.openers;
        let editor_config = &self.user_settings.editor;

        match crate::opener::open_file(&path, openers, editor_config) {
            crate::opener::OpenResult::Opened => {
                // File opened with system command (non-blocking)
            }
            crate::opener::OpenResult::NeedsSuspend(cmd) => {
                // Need to suspend terminal and run command
                self.suspend_and_run(cmd);
            }
            crate::opener::OpenResult::NotSupported(msg) => {
                self.error = Some(msg);
            }
            crate::opener::OpenResult::Error(msg) => {
                self.error = Some(msg);
            }
        }
    }

    /// Suspend the terminal, run a command, and restore.
    fn suspend_and_run(&mut self, cmd: std::process::Command) {
        // We need to restore terminal before running the command
        // and reinitialize after. This is handled by setting a flag
        // that the main loop will process.
        self.pending_suspend_command = Some(cmd);
    }

    /// Drill into the currently selected directory.
    fn drill_into_selected(&mut self) {
        let Some((view_node, view_path)) = self.get_view_root_node() else {
            return;
        };

        let items = TreeView::new(
            view_node,
            &view_path,
            &self.theme,
            &self.marked,
            &self.clipboard,
        )
        .flatten(&self.tree_state);
        let Some(item) = items.get(self.tree_state.selected) else {
            return;
        };

        if !matches!(item.node.kind, crate::ui::VisibleNodeKind::Directory { .. }) {
            return;
        }

        // Check if directory needs lazy loading (has no children loaded yet)
        let needs_lazy_load = if let Some(tree) = &self.tree {
            if let Some(node) = Self::find_node_at_path(&tree.root, &item.path, &tree.root_path) {
                node.children.is_empty()
            } else {
                true
            }
        } else {
            true
        };

        // Lazy load directory contents if needed
        if needs_lazy_load {
            self.lazy_load_directory(&item.path);
        }

        if item.path == self.view_root {
            self.tree_state.expand(&item.path);
            self.update_cached_tree_len();
            return;
        }

        let saved_expanded = self.tree_state.expanded.clone();
        let saved_selected = self.tree_state.selected;
        self.view_history
            .push((self.view_root.clone(), saved_selected, saved_expanded));

        let target_path = item.path.clone();

        // Check if forward history has this path - if so, restore that state
        if let Some(forward_idx) = self
            .forward_history
            .iter()
            .rposition(|(p, _, _)| p == &target_path)
        {
            let (_, fwd_selected, fwd_expanded) = self.forward_history.remove(forward_idx);
            // Clear any forward history entries after this one
            self.forward_history.truncate(forward_idx);

            self.view_root = target_path;
            self.tree_state.selected = fwd_selected;
            self.tree_state.offset = 0;
            self.tree_state.expanded = fwd_expanded;
            self.tree_state.expand(&self.view_root);
            self.update_cached_tree_len();
            self.sync_to_active_tab();
            return;
        }

        // Clear forward history - user is navigating to a new location
        self.forward_history.clear();

        self.view_root = target_path;

        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
        self.sync_to_active_tab();
    }

    /// Drill into the selected directory in Miller columns mode.
    fn drill_into_miller_selected(&mut self) {
        let Some((view_node, _)) = self.get_view_root_node() else {
            return;
        };

        let Some(selected_child) = view_node.children.get(self.miller_state.selected) else {
            return;
        };

        // Only drill into directories
        if !selected_child.is_dir() {
            return;
        }

        let target_path = self.view_root.join(selected_child.file_name());

        // Lazy loading: if directory has no children (from quick_list), load them now
        if selected_child.children.is_empty() {
            self.lazy_load_directory(&target_path);
        }

        // Save current state to history
        let saved_expanded = self.tree_state.expanded.clone();
        let saved_selected = self.miller_state.selected;
        self.view_history
            .push((self.view_root.clone(), saved_selected, saved_expanded));

        // Check if forward history has this path - if so, restore that state
        if let Some(forward_idx) = self
            .forward_history
            .iter()
            .rposition(|(p, _, _)| p == &target_path)
        {
            let (_, fwd_selected, fwd_expanded) = self.forward_history.remove(forward_idx);
            // Clear any forward history entries after this one
            self.forward_history.truncate(forward_idx);

            self.view_root = target_path;
            self.miller_state.selected = fwd_selected;
            self.miller_state.offset = 0;
            self.tree_state.expanded = fwd_expanded;
            self.update_cached_miller_len();
            self.miller_state.ensure_visible(self.viewport_height);
            self.update_preview();
            self.sync_to_active_tab();
            return;
        }

        // Clear forward history - user is navigating to a new location
        self.forward_history.clear();

        // Update view root
        self.view_root = target_path;
        self.miller_state.reset();
        self.update_cached_miller_len();
        self.update_preview();
        self.sync_to_active_tab();
    }

    /// Lazy load a directory's children using quick_list.
    /// This updates the tree in-place with the new children.
    fn lazy_load_directory(&mut self, path: &Path) {
        // Captured up front: the mutable borrow taken by `Arc::make_mut` below
        // rules out reading the tree's own root path at the same time.
        let Some(root_path) = self.tree.as_ref().map(|tree| tree.root_path.clone()) else {
            return;
        };

        // First check if we have cached scan data for this directory
        if let Some(cached_tree) = self
            .scanned_cache
            .as_ref()
            .and_then(|c| c.get(path))
            .cloned()
        {
            // Use cached scan data - it has full size/structure information
            if let Some(tree) = &mut self.tree
                && let Some(node) =
                    Self::find_node_mut(&mut Arc::make_mut(tree).root, path, &root_path)
            {
                // Replace with the cached node (preserves all scan data)
                *node = cached_tree.root.clone();
            }
            return;
        }

        // Quick load directory contents for immediate display
        // When the full scan completes, it will replace this with accurate data
        if let Some(mut quick_tree) = self.load_directory_listing(path) {
            // Find the node in our tree and update its children
            if let Some(tree) = &mut self.tree
                && let Some(node) =
                    Self::find_node_mut(&mut Arc::make_mut(tree).root, path, &root_path)
            {
                node.children = std::mem::take(&mut quick_tree.root.children);
            }
        }
    }

    fn load_directory_listing(&mut self, path: &Path) -> Option<gravityfile_core::FileTree> {
        match quick_list_with_settings(path, &self.user_settings) {
            Ok(tree) => Some(tree),
            Err(error) => {
                self.error = Some(format!("Cannot list {}: {error}", path.display()));
                self.needs_redraw = true;
                None
            }
        }
    }

    /// Find a mutable reference to a node by path.
    fn find_node_mut<'a>(
        node: &'a mut gravityfile_core::FileNode,
        target: &Path,
        current_path: &Path,
    ) -> Option<&'a mut gravityfile_core::FileNode> {
        let relative = target.strip_prefix(current_path).ok()?;
        let mut found = node;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            found = found
                .children
                .iter_mut()
                .find(|child| child.file_name() == name)?;
        }
        Some(found)
    }

    /// Navigate back up to the previous view root.
    /// Supports navigation beyond the scan root, all the way to filesystem root.
    fn navigate_back(&mut self) {
        // Prepare current state for forward history (will be pushed if navigation succeeds)
        let current_selected = match self.layout_mode {
            LayoutMode::Tree => self.tree_state.selected,
            LayoutMode::Miller => self.miller_state.selected,
        };
        let current_state = (
            self.view_root.clone(),
            current_selected,
            self.tree_state.expanded.clone(),
        );

        // First, try to use view history if available
        if let Some((prev_root, saved_selected, saved_expanded)) = self.view_history.last().cloned()
        {
            let loaded = self.tree.as_ref().is_some_and(|tree| {
                Self::find_node_at_path(&tree.root, &prev_root, &tree.root_path)
                    .is_some_and(|node| node.is_dir())
            });
            let prev_root = if loaded {
                prev_root
            } else {
                let Some(tree) = self.load_directory_listing(&prev_root) else {
                    return;
                };
                self.prepare_operation();
                self.path = tree.root_path.clone();
                self.tree = Some(Arc::new(tree));
                self.duplicates = None;
                self.age_report = None;
                self.has_full_scan = false;
                self.path.clone()
            };
            self.view_history.pop();
            // Destination is usable; only now commit both history stacks.
            self.forward_history.push(current_state);
            self.view_root = prev_root;
            self.tree_state.expanded = saved_expanded;

            // Restore selection based on layout mode
            match self.layout_mode {
                LayoutMode::Tree => {
                    self.tree_state.selected = saved_selected;
                    self.tree_state.offset = 0;
                    self.update_cached_tree_len();
                }
                LayoutMode::Miller => {
                    self.miller_state.selected = saved_selected;
                    self.miller_state.offset = 0;
                    self.update_cached_miller_len();
                }
            }
            self.sync_to_active_tab();
            return;
        }

        // No history - try to navigate to parent
        let Some(parent) = self.view_root.parent() else {
            return; // Already at root
        };

        let parent = parent.to_path_buf();
        let dir_we_came_from = self.view_root.clone();

        // Check if parent is within our current tree
        let parent_in_tree = parent.starts_with(&self.path) || parent == self.path;

        if parent_in_tree {
            // Navigate within existing tree
            self.tree_state.collapse(&dir_we_came_from);
            self.view_root = parent;
            self.tree_state.expand(&self.view_root);
        } else {
            // Navigate beyond scan root - need to extend our tree
            if self.navigate_to_parent_beyond_scan_root(&parent, &dir_we_came_from) {
                self.forward_history.push(current_state);
            }
            return;
        }
        self.forward_history.push(current_state);

        // Select the directory we just came from
        self.select_child_by_name(&dir_we_came_from);
        self.sync_to_active_tab();
    }

    /// Navigate to a parent directory that's outside the current scan root.
    /// This extends the tree by lazy-loading the parent.
    fn navigate_to_parent_beyond_scan_root(
        &mut self,
        parent: &Path,
        dir_we_came_from: &Path,
    ) -> bool {
        // Cache current tree before navigating away (if we have scan data)
        if let Some(tree) = &self.tree
            && self.has_full_scan
            && let Some(cache) = &mut self.scanned_cache
        {
            cache.insert(tree.root_path.clone(), Arc::clone(tree));
        }

        // Load the parent directory contents
        let Some(mut parent_tree) = self.load_directory_listing(parent) else {
            return false; // Preserve the current tree and histories on failure.
        };

        // Merge any cached scans into the parent tree's children
        self.merge_cached_scans_into_tree(&mut parent_tree.root, parent);

        // Create a new tree rooted at the resolved parent.
        self.prepare_operation();
        self.path = parent_tree.root_path.clone();
        self.tree = Some(Arc::new(parent_tree));
        self.view_root = self.path.clone();
        self.duplicates = None;
        self.age_report = None;
        self.view_history.clear();
        // Note: don't clear forward_history here - we want to allow navigating back

        // The parent tree is quick-listed, not fully scanned
        self.has_full_scan = false;

        // Reset tree state for the new root
        self.tree_state = TreeState::new(self.path.clone());

        // Update cached parent for Miller columns display
        self.update_cached_parent();

        // Select the directory we came from
        self.select_child_by_name(dir_we_came_from);
        self.sync_to_active_tab();
        true
    }

    /// Merge cached scan data into a tree node's children.
    /// For each child directory in the node, if we have cached scan data,
    /// replace the placeholder child with the fully scanned version.
    fn merge_cached_scans_into_tree(
        &self,
        node: &mut gravityfile_core::FileNode,
        node_path: &Path,
    ) {
        for child in &mut node.children {
            if child.is_dir() {
                let child_path = node_path.join(child.file_name());
                if let Some(cached_tree) =
                    self.scanned_cache.as_ref().and_then(|c| c.get(&child_path))
                {
                    // Replace this child with the cached scan data
                    *child = cached_tree.root.clone();
                }
            }
        }
    }

    /// Select a child entry by matching its name.
    fn select_child_by_name(&mut self, target_path: &Path) {
        let dir_name = target_path.file_name().unwrap_or_default();

        match self.layout_mode {
            LayoutMode::Tree => {
                // In tree view, find the index of the target in flattened list
                if let Some((node, root_path)) = self.get_view_root_node() {
                    let items =
                        TreeView::new(node, &root_path, &self.theme, &self.marked, &self.clipboard)
                            .flatten(&self.tree_state);
                    if let Some(idx) = items
                        .iter()
                        .position(|item| item.path.file_name() == Some(dir_name))
                    {
                        self.tree_state.selected = idx;
                    } else {
                        self.tree_state.selected = 0;
                    }
                } else {
                    self.tree_state.selected = 0;
                }
                self.tree_state.offset = 0;
                self.update_cached_tree_len();
            }
            LayoutMode::Miller => {
                // In miller view, find the index in children
                if let Some((node, _)) = self.get_view_root_node() {
                    let idx = node
                        .children
                        .iter()
                        .position(|c| c.file_name() == dir_name)
                        .unwrap_or(0);
                    self.miller_state.selected = idx;
                } else {
                    self.miller_state.selected = 0;
                }
                self.miller_state.offset = 0;
                self.update_cached_miller_len();
            }
        }
    }

    /// Find a node by path in the tree.
    fn find_node_at_path<'a>(
        node: &'a gravityfile_core::FileNode,
        target_path: &Path,
        current_path: &Path,
    ) -> Option<&'a gravityfile_core::FileNode> {
        Self::find_node_at_path_while(node, target_path, current_path, || true)
    }

    fn find_node_at_path_while<'a>(
        node: &'a gravityfile_core::FileNode,
        target_path: &Path,
        current_path: &Path,
        mut keep_going: impl FnMut() -> bool,
    ) -> Option<&'a gravityfile_core::FileNode> {
        if !keep_going() {
            return None;
        }
        let relative = target_path.strip_prefix(current_path).ok()?;
        let mut found = node;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return None;
            };
            let mut matched = None;
            for child in &found.children {
                if !keep_going() {
                    return None;
                }
                if child.file_name() == name {
                    matched = Some(child);
                    break;
                }
            }
            found = matched?;
        }
        Some(found)
    }

    /// Get the current view root node.
    fn get_view_root_node(&self) -> Option<(&gravityfile_core::FileNode, PathBuf)> {
        let tree = self.tree.as_ref()?;
        let node = Self::find_node_at_path(&tree.root, &self.view_root, &tree.root_path)?;
        Some((node, self.view_root.clone()))
    }

    /// Get the parent node of the current view root (for Miller columns).
    fn get_parent_node(&self) -> Option<&gravityfile_core::FileNode> {
        let tree = self.tree.as_ref()?;
        let parent_path = self.view_root.parent()?.to_path_buf();

        // First, check if parent is within the main tree
        if parent_path.starts_with(&tree.root_path) || parent_path == tree.root_path {
            return Self::find_node_at_path(&tree.root, &parent_path, &tree.root_path);
        }

        // If we're at the tree root and have a cached parent, use that
        if self.view_root == tree.root_path
            && let Some(cached) = &self.cached_parent_tree
        {
            return Some(&cached.root);
        }

        None
    }

    /// Update the cached parent tree for Miller columns display.
    /// Called when navigating to ensure the parent column can be shown.
    fn update_cached_parent(&mut self) {
        // Never retain a previous location's parent when refresh fails.
        self.cached_parent_tree = None;
        let Some(tree) = &self.tree else { return };

        // Only need to cache parent if we're at or near the tree root
        if self.view_root == tree.root_path {
            if let Some(parent_path) = self.view_root.parent() {
                let parent_path_buf = parent_path.to_path_buf();
                // Load the parent directory
                if let Ok(mut parent_tree) =
                    quick_list_with_settings(&parent_path_buf, &self.user_settings)
                {
                    // Merge any cached scans into the parent tree's children
                    // This ensures previously scanned directories show their data
                    self.merge_cached_scans_into_tree(&mut parent_tree.root, &parent_path_buf);
                    self.cached_parent_tree = Some(parent_tree);
                }
            }
        } else {
            // Don't need cached parent when navigating within the tree
            self.cached_parent_tree = None;
        }
    }

    /// Get the current directory name for highlighting in parent column.
    fn get_current_dir_name(&self) -> Option<String> {
        self.view_root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }

    /// Handle raw key input for command mode.
    fn handle_command_input(&mut self, key: crossterm::event::KeyEvent) {
        match self.command_input.handle_key(key) {
            CommandKeyResult::Continue => {}
            CommandKeyResult::Cancel => {
                self.mode = AppMode::Normal;
            }
            CommandKeyResult::Execute(cmd) => {
                self.mode = AppMode::Normal;
                self.execute_command(&cmd);
            }
        }
    }

    /// Execute a command from the command palette.
    fn execute_command(&mut self, cmd: &str) {
        match parse_command(cmd) {
            CommandAction::None => {}
            CommandAction::Invalid(message) => {
                self.error = Some(message.to_string());
            }
            CommandAction::Quit => {
                if self.cancel_archive_operation() {
                    return;
                }
                if self.archive_rx.is_some() || self.take_rx.is_some() {
                    self.error =
                        Some("Filesystem operation in progress; wait for it to finish".to_string());
                } else {
                    self.mode = AppMode::Quit;
                }
            }
            CommandAction::Refresh => {
                self.marked.clear();
                self.start_scan();
            }
            CommandAction::Analyze => {
                self.start_analysis_for_current_tree();
            }
            CommandAction::NavigateTo(target) => {
                self.cd_to_path(&target);
            }
            CommandAction::GoToRoot => {
                self.go_to_root();
            }
            CommandAction::NavigateBack => {
                self.navigate_back();
            }
            CommandAction::ShowHelp => {
                self.mode = AppMode::Help;
            }
            CommandAction::SwitchView(view) => {
                self.view = view;
                if self.view == View::Treemap {
                    self.update_cached_treemap_len();
                    self.treemap_state.reset();
                }
                self.maybe_start_analysis_for_view();
            }
            CommandAction::ClearMarks => {
                self.marked.clear();
            }
            CommandAction::ToggleDetails => {
                self.show_details = !self.show_details;
            }
            CommandAction::SetTheme(theme_cmd) => match theme_cmd {
                ThemeCommand::Dark => self.theme = Theme::dark(),
                ThemeCommand::Light => self.theme = Theme::light(),
                ThemeCommand::Toggle => self.theme = self.theme.toggle(),
            },
            CommandAction::SetLayout(layout_cmd) => {
                match layout_cmd {
                    LayoutCommand::Tree => {
                        if self.layout_mode != LayoutMode::Tree {
                            // Use sync to preserve selection when switching to tree
                            self.sync_layout_selection();
                        }
                    }
                    LayoutCommand::Miller => {
                        if self.layout_mode != LayoutMode::Miller {
                            // Use sync to preserve selection when switching to miller
                            self.sync_layout_selection();
                        }
                    }
                    LayoutCommand::Toggle => {
                        self.sync_layout_selection();
                    }
                }
            }
            CommandAction::SetSort(sort_cmd) => {
                match sort_cmd {
                    SortCommand::SizeDesc => self.sort_mode = SortMode::SizeDescending,
                    SortCommand::SizeAsc => self.sort_mode = SortMode::SizeAscending,
                    SortCommand::NameAsc => self.sort_mode = SortMode::NameAscending,
                    SortCommand::NameDesc => self.sort_mode = SortMode::NameDescending,
                    SortCommand::DateDesc => self.sort_mode = SortMode::ModifiedDescending,
                    SortCommand::DateAsc => self.sort_mode = SortMode::ModifiedAscending,
                    SortCommand::CountDesc => self.sort_mode = SortMode::CountDescending,
                    SortCommand::CountAsc => self.sort_mode = SortMode::CountAscending,
                    SortCommand::Cycle => self.sort_mode = self.sort_mode.next(),
                    SortCommand::Reverse => self.sort_mode = self.sort_mode.reverse(),
                }
                self.apply_sort();
            }
            CommandAction::Yank => {
                self.yank_selection();
            }
            CommandAction::Cut => {
                self.cut_selection();
            }
            CommandAction::Paste => {
                self.paste_clipboard();
            }
            CommandAction::Delete => {
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                }
            }
            CommandAction::Rename(name) => {
                if let Some(name) = name {
                    // Direct rename with provided name
                    if let Some(path) = self.get_selected_path() {
                        self.execute_rename(path, name);
                    }
                } else {
                    // Enter rename mode
                    self.start_rename();
                }
            }
            CommandAction::CreateFile(name) => {
                if let Some(name) = name {
                    // Direct create with provided name
                    self.execute_create_file(self.view_root.clone(), name);
                } else {
                    // Enter create file mode
                    self.start_create_file();
                }
            }
            CommandAction::CreateDirectory(name) => {
                if let Some(name) = name {
                    // Direct create with provided name
                    self.execute_create_directory(self.view_root.clone(), name);
                } else {
                    // Enter create directory mode
                    self.start_create_directory();
                }
            }
            CommandAction::Take(name) => {
                if let Some(name) = name {
                    // Direct take with provided name
                    self.execute_take(self.view_root.clone(), name);
                } else {
                    // Enter take mode
                    self.start_take();
                }
            }
            CommandAction::Undo => {
                self.execute_undo();
            }
            CommandAction::ShowBookmarks => {
                self.bookmark_list_state =
                    Some(BookmarkListState::new(&self.user_settings.bookmarks));
                self.mode = AppMode::BookmarkList;
            }
            CommandAction::SetBookmark(key) => {
                if let Some(k) = key {
                    self.set_bookmark(k);
                } else {
                    self.mode = AppMode::SettingBookmark;
                }
            }
            CommandAction::JumpToBookmark(key) => {
                if let Some(k) = key {
                    self.jump_to_bookmark(k);
                } else {
                    self.mode = AppMode::JumpingToBookmark;
                }
            }
            CommandAction::DeleteBookmark(key) => {
                self.user_settings.bookmarks.remove(key);
                if let Err(error) = self.user_settings.save() {
                    self.error = Some(format!("Failed to save bookmarks: {error}"));
                }
            }
            CommandAction::BulkRename => {
                self.start_bulk_rename();
            }
            CommandAction::Extract(destination) => {
                self.extract_archive(destination);
            }
            CommandAction::Compress(archive_name) => {
                self.compress_files(&archive_name);
            }
        }
    }

    /// Validate a user-provided path for safety.
    /// Returns error message if path is invalid.
    fn validate_user_path(path_str: &str) -> Result<PathBuf, String> {
        // Reject empty paths
        if path_str.is_empty() {
            return Err("Path cannot be empty".to_string());
        }

        // Reject paths with null bytes (security)
        if path_str.contains('\0') {
            return Err("Invalid characters in path".to_string());
        }

        let path = PathBuf::from(path_str);

        // Reject paths with parent directory components (prevents escape attempts)
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("Parent directory references (..) not allowed".to_string());
        }

        // Reject paths starting with ~ (could expand to home directory unexpectedly)
        if path_str.starts_with('~') {
            return Err("Tilde expansion not supported, use absolute path".to_string());
        }

        Ok(path)
    }

    /// Start bulk rename by opening editor with marked file names.
    fn start_bulk_rename(&mut self) {
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
    fn process_bulk_rename_from_editor(&mut self) {
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
    fn execute_bulk_rename(&mut self) {
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
        self.scan_rx = Some(scanning::start_bulk_rename(
            renames,
            self.scan_token.clone(),
        ));
        self.mode = AppMode::BulkRenaming;
        self.operation_message = Some((
            true,
            "Bulk rename in progress; waiting for completion".into(),
        ));
        self.needs_redraw = true;
    }

    /// Go to the scan root.
    fn go_to_root(&mut self) {
        self.view_root = self.path.clone();
        self.view_history.clear();
        self.forward_history.clear();
        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
        self.sync_to_active_tab();
    }

    /// Change directory within the loaded scan tree. Leading `/` is scan-root relative.
    fn cd_to_path(&mut self, target: &str) {
        if target == "/" || target == "~" {
            self.go_to_root();
            return;
        }
        if target == ".." {
            self.navigate_back();
            return;
        }
        let Some(tree) = &self.tree else {
            self.error = Some("Cannot change directory: no directory tree is loaded".into());
            return;
        };
        let mut target_path = if target.starts_with('/') {
            self.path.clone()
        } else {
            self.view_root.clone()
        };
        for component in Path::new(target.trim_start_matches('/')).components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::Normal(name) => target_path.push(name),
                std::path::Component::ParentDir => {
                    if target_path == self.path || !target_path.starts_with(&self.path) {
                        self.error =
                            Some("Cannot change directory: path leaves the scan root".into());
                        return;
                    }
                    target_path.pop();
                }
                _ => {
                    self.error = Some("Cannot change directory: unsupported path prefix".into());
                    return;
                }
            }
        }
        let Some(node) = Self::find_node_at_path(&tree.root, &target_path, &tree.root_path) else {
            self.error = Some(format!(
                "Directory is not in the loaded tree: {}",
                target_path.display()
            ));
            return;
        };
        if !node.is_dir() {
            self.error = Some(format!("Not a directory: {}", target_path.display()));
            return;
        }
        if target_path == self.view_root {
            return;
        }
        self.record_navigation_origin();
        self.view_root = target_path;
        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.tree_state.expand(&self.view_root);
        self.miller_state.reset();
        self.update_cached_tree_len();
        self.update_cached_miller_len();
        self.sync_to_active_tab();
    }

    /// Check if a path is marked for deletion.
    pub fn is_marked(&self, path: &PathBuf) -> bool {
        self.marked.contains(path)
    }

    /// Handle a key event when in bookmark mode (setting or jumping).
    fn handle_bookmark_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
            }
            KeyCode::Char(c) if state::Bookmarks::is_valid_key(c) => {
                match self.mode {
                    AppMode::SettingBookmark => {
                        self.set_bookmark(c);
                    }
                    AppMode::JumpingToBookmark => {
                        self.jump_to_bookmark(c);
                    }
                    _ => {}
                }
                self.mode = AppMode::Normal;
            }
            _ => {
                // Invalid key, return to normal mode
                self.mode = AppMode::Normal;
            }
        }
    }

    /// Set a bookmark for the current directory.
    fn set_bookmark(&mut self, key: char) {
        self.user_settings
            .bookmarks
            .set(key, self.view_root.clone());
        if let Err(error) = self.user_settings.save() {
            self.error = Some(format!("Failed to save bookmarks: {error}"));
        }
    }

    /// Resolve and prepare a bookmark destination before changing navigation.
    fn jump_to_bookmark(&mut self, key: char) {
        let Some(saved_path) = self.user_settings.bookmarks.get(key).cloned() else {
            self.error = Some(format!("Bookmark '{key}' is not set"));
            return;
        };
        let path = match saved_path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("Cannot open bookmark '{key}': {error}"));
                return;
            }
        };
        if !path.is_dir() {
            self.error = Some(format!("Bookmark '{key}' is not a directory"));
            return;
        }
        let loaded = self.tree.as_ref().is_some_and(|tree| {
            Self::find_node_at_path(&tree.root, &path, &tree.root_path)
                .is_some_and(|node| node.is_dir())
        });
        // A descendant can be absent from a quick/partial tree even when its
        // path is under the scan root. Prepare a usable listing in that case.
        let replacement = if loaded {
            None
        } else {
            let Some(tree) = self.load_directory_listing(&path) else {
                return;
            };
            Some(tree)
        };
        let destination = replacement
            .as_ref()
            .map_or_else(|| path.clone(), |tree| tree.root_path.clone());
        if loaded && destination == self.view_root {
            return;
        }
        self.record_navigation_origin();
        self.view_root = destination;
        self.tree_state.selected = 0;
        self.tree_state.offset = 0;
        self.miller_state.reset();
        if let Some(tree) = replacement {
            self.prepare_operation();
            self.path = tree.root_path.clone();
            self.tree = Some(Arc::new(tree));
            self.tree_state = TreeState::new(self.path.clone());
            self.duplicates = None;
            self.age_report = None;
            self.has_full_scan = false;
            if self.scan_on_startup {
                self.start_scan();
            }
        }
        self.tree_state.expand(&self.view_root);
        self.update_cached_tree_len();
        self.update_cached_miller_len();
        self.sync_to_active_tab();
    }

    /// Handle input in the bookmark list modal.
    fn handle_bookmark_list_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.bookmark_list_state = None;
                self.mode = AppMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(state) = &mut self.bookmark_list_state {
                    state.update_count(&self.user_settings.bookmarks);
                    state.move_down();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(state) = &mut self.bookmark_list_state {
                    state.move_up();
                }
            }
            KeyCode::Enter => {
                // Jump to selected bookmark
                if let Some(state) = &self.bookmark_list_state {
                    let entries = self.user_settings.bookmarks.sorted_entries();
                    if let Some((key, _)) = entries.get(state.selected) {
                        let key = *key;
                        self.bookmark_list_state = None;
                        self.mode = AppMode::Normal;
                        self.jump_to_bookmark(key);
                    }
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                // Delete selected bookmark
                if let Some(state) = &self.bookmark_list_state {
                    let entries = self.user_settings.bookmarks.sorted_entries();
                    if let Some((key, _)) = entries.get(state.selected) {
                        let key = *key;
                        self.user_settings.bookmarks.remove(key);
                        if let Err(error) = self.user_settings.save() {
                            self.error = Some(format!("Failed to save bookmarks: {error}"));
                        }
                    }
                }
                // Update state after deletion
                if let Some(state) = &mut self.bookmark_list_state {
                    state.update_count(&self.user_settings.bookmarks);
                }
            }
            // Allow jumping directly by pressing the bookmark key
            KeyCode::Char(c)
                if state::Bookmarks::is_valid_key(c)
                    && self.user_settings.bookmarks.get(c).is_some() =>
            {
                self.bookmark_list_state = None;
                self.mode = AppMode::Normal;
                self.jump_to_bookmark(c);
            }
            _ => {}
        }
    }

    /// Handle input in bulk rename confirmation modal.
    fn handle_bulk_rename_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') => {
                // Cancel bulk rename
                self.bulk_rename_state = None;
                self.mode = AppMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(state) = &mut self.bulk_rename_state {
                    state.move_down();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(state) = &mut self.bulk_rename_state {
                    state.move_up();
                }
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                // Check for validation errors before executing
                if let Some(state) = &self.bulk_rename_state
                    && state.error.is_some()
                {
                    // Can't execute with errors
                    return;
                }
                // Execute the bulk rename
                self.execute_bulk_rename();
            }
            KeyCode::Char('e') => {
                // Re-open the pending names in the configured editor through
                // the same private, validated handoff as the first edit.
                let Some(state) = self.bulk_rename_state.take() else {
                    return;
                };
                let (paths, names) = state
                    .entries
                    .into_iter()
                    .map(|entry| (entry.original, entry.new_name))
                    .unzip();
                let prepared = bulk_edit::BulkRenameEdit::with_names(paths, names)
                    .map_err(|error| format!("Cannot prepare bulk rename: {error}"))
                    .and_then(|edit| {
                        crate::opener::editor_command(edit.path(), &self.user_settings.editor)
                            .map(|command| (edit, command))
                            .map_err(|error| {
                                format!("Cannot configure bulk rename editor: {error}")
                            })
                    });
                self.mode = AppMode::Normal;
                match prepared {
                    Ok((edit, command)) => {
                        self.bulk_rename_edit = Some(edit);
                        self.pending_suspend_command = Some(command);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            _ => {}
        }
    }

    /// Handle input in visual selection mode.
    fn handle_visual_mode_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        // Get the max count based on current layout
        let max_count = match self.layout_mode {
            LayoutMode::Tree => self.cached_tree_len,
            LayoutMode::Miller => self.cached_miller_len,
        };

        match (key.code, key.modifiers) {
            // Exit visual mode with Esc - cancel selection
            (KeyCode::Esc, _) => {
                self.visual_state = None;
                self.mode = AppMode::Normal;
            }
            // Exit visual mode with Enter or V - apply selection as marks
            (KeyCode::Enter, _) | (KeyCode::Char('V'), KeyModifiers::SHIFT) => {
                self.apply_visual_selection_as_marks();
                self.visual_state = None;
                self.mode = AppMode::Normal;
            }
            // Navigation - vim style
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.move_down(max_count);
                    // Sync the visual cursor with the actual selection state
                    self.sync_visual_cursor_to_view();
                }
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.move_up();
                    self.sync_visual_cursor_to_view();
                }
            }
            // Page navigation
            (KeyCode::Char('u'), KeyModifiers::CONTROL) | (KeyCode::PageUp, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.page_up(constants::PAGE_SIZE);
                    self.sync_visual_cursor_to_view();
                }
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) | (KeyCode::PageDown, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.page_down(constants::PAGE_SIZE, max_count);
                    self.sync_visual_cursor_to_view();
                }
            }
            // Jump to top/bottom
            (KeyCode::Char('g'), KeyModifiers::NONE) | (KeyCode::Home, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.jump_to_top();
                    self.sync_visual_cursor_to_view();
                }
            }
            (KeyCode::Char('G'), KeyModifiers::SHIFT) | (KeyCode::End, _) => {
                if let Some(state) = &mut self.visual_state {
                    state.jump_to_bottom(max_count);
                    self.sync_visual_cursor_to_view();
                }
            }
            // Quick actions while in visual mode
            (KeyCode::Char('d'), KeyModifiers::NONE)
            | (KeyCode::Char('D'), KeyModifiers::SHIFT) => {
                // Delete selected range; the user chose these marks.
                self.apply_visual_selection_as_marks();
                self.delete_auto_marked = false;
                self.visual_state = None;
                self.mode = AppMode::Normal;
                // Now trigger delete confirmation
                if !self.marked.is_empty() {
                    self.mode = AppMode::ConfirmDelete;
                }
            }
            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                // Yank (copy) selected range
                self.apply_visual_selection_as_marks();
                self.visual_state = None;
                self.mode = AppMode::Normal;
                self.yank_selection();
            }
            (KeyCode::Char('x'), KeyModifiers::NONE) => {
                // Cut selected range
                self.apply_visual_selection_as_marks();
                self.visual_state = None;
                self.mode = AppMode::Normal;
                self.cut_selection();
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    /// Sync the visual cursor position to the actual view selection state.
    fn sync_visual_cursor_to_view(&mut self) {
        if let Some(state) = &self.visual_state {
            match self.layout_mode {
                LayoutMode::Tree => self.tree_state.selected = state.cursor,
                LayoutMode::Miller => self.miller_state.selected = state.cursor,
            }
        }
    }

    /// Apply the visual selection range as marks.
    fn apply_visual_selection_as_marks(&mut self) {
        let Some(state) = &self.visual_state else {
            return;
        };

        let (start, end) = state.selection_range();

        // Collect paths first to avoid borrow issues
        let paths_to_mark: Vec<PathBuf> = match self.layout_mode {
            LayoutMode::Tree => {
                // Get flattened tree items
                if let Some((node, root_path)) = self.get_view_root_node() {
                    let items = crate::ui::TreeView::new(
                        node,
                        &root_path,
                        &self.theme,
                        &self.marked,
                        &self.clipboard,
                    )
                    .flatten(&self.tree_state);

                    (start..=end)
                        .filter_map(|i| items.get(i).map(|item| item.path.clone()))
                        .collect()
                } else {
                    Vec::new()
                }
            }
            LayoutMode::Miller => {
                // Index children in their natural (displayed) order. Miller
                // columns render `node.children` directly without re-sorting, so
                // the visual range must use the same order — otherwise marks land
                // on different files than the highlight (e.g. in quick-listed
                // dirs, which are name-sorted while sort_mode is size-based).
                if let Some((node, _)) = self.get_view_root_node() {
                    let view_root = self.view_root.clone();
                    (start..=end)
                        .filter_map(|i| {
                            node.children
                                .get(i)
                                .map(|child| view_root.join(child.file_name()))
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
        };

        for path in paths_to_mark {
            self.mark(path);
        }
    }

    /// Keep the scroll offsets the last frame actually used.
    fn adopt_rendered_offsets(&mut self) {
        (self.tree_state.offset, self.miller_state.offset) = self.rendered_offsets.get();
    }

    /// Paths the UI never operates on destructively: the scan root, the
    /// directory being viewed, and any ancestor of either.
    fn is_protected(&self, path: &Path) -> bool {
        self.path.starts_with(path) || self.view_root.starts_with(path)
    }

    /// Mark `path` unless it is protected. Returns whether it is now marked.
    fn mark(&mut self, path: PathBuf) -> bool {
        if self.is_protected(&path) {
            return false;
        }
        self.marked.insert(path);
        true
    }

    /// Handle a mouse action.
    fn handle_mouse_action(&mut self, action: MouseAction) {
        // Store the last content area dimensions for hit testing
        // In a real implementation, we would track the rendered areas from the last frame
        // For now, we implement basic scroll wheel and click functionality

        match action {
            MouseAction::ScrollUp { .. } => {
                // Scroll up - move selection up
                match self.view {
                    View::Explorer => {
                        for _ in 0..3 {
                            self.move_up();
                        }
                    }
                    View::Duplicates => {
                        // Collect group info to avoid borrow issues
                        let group_info: Vec<usize> = self
                            .get_filtered_duplicates()
                            .map(|(filtered, _)| filtered.iter().map(|g| g.paths.len()).collect())
                            .unwrap_or_default();
                        let group_count = group_info.len();
                        for _ in 0..3 {
                            self.duplicates_state
                                .move_up(group_count, |i| group_info.get(i).copied().unwrap_or(0));
                        }
                    }
                    View::Age => {
                        for _ in 0..3 {
                            if self.selected_stale_dir > 0 {
                                self.selected_stale_dir -= 1;
                            }
                        }
                    }
                    View::Errors => {
                        for _ in 0..3 {
                            if self.selected_warning > 0 {
                                self.selected_warning -= 1;
                            }
                        }
                    }
                    View::Treemap => {
                        for _ in 0..3 {
                            self.treemap_state.move_prev();
                        }
                    }
                }
            }
            MouseAction::ScrollDown { .. } => {
                // Scroll down - move selection down
                match self.view {
                    View::Explorer => {
                        for _ in 0..3 {
                            self.move_down();
                        }
                    }
                    View::Duplicates => {
                        // Collect group info to avoid borrow issues
                        let group_info: Vec<usize> = self
                            .get_filtered_duplicates()
                            .map(|(filtered, _)| filtered.iter().map(|g| g.paths.len()).collect())
                            .unwrap_or_default();
                        let group_count = group_info.len();
                        for _ in 0..3 {
                            self.duplicates_state.move_down(group_count, |i| {
                                group_info.get(i).copied().unwrap_or(0)
                            });
                        }
                    }
                    View::Age => {
                        let max = self
                            .get_filtered_stale_dirs()
                            .map(|dirs| dirs.len().saturating_sub(1))
                            .unwrap_or(0);
                        for _ in 0..3 {
                            if self.selected_stale_dir < max {
                                self.selected_stale_dir += 1;
                            }
                        }
                    }
                    View::Errors => {
                        let max = self.warning_count().saturating_sub(1);
                        for _ in 0..3 {
                            if self.selected_warning < max {
                                self.selected_warning += 1;
                            }
                        }
                    }
                    View::Treemap => {
                        for _ in 0..3 {
                            self.treemap_state.move_next(self.cached_treemap_len);
                        }
                    }
                }
            }
            MouseAction::Click { y, .. } if self.view == View::Explorer => {
                // For now, simple click-to-select functionality
                // Calculate which item was clicked based on the Y position
                // This is a simplified implementation - a more complete version would
                // track rendered areas from the last frame

                let list_top = self.estimated_list_top_row();
                let _footer_height: u16 = 1; // Reserved for future use in click calculation

                if y >= list_top {
                    let relative_y = (y - list_top) as usize;

                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let new_selected = self.tree_state.offset + relative_y;
                            if new_selected < self.cached_tree_len {
                                self.tree_state.selected = new_selected;
                            }
                        }
                        LayoutMode::Miller => {
                            // In Miller mode, the middle column starts after the left column
                            // This is a simplified version - we just select by row
                            let new_selected = self.miller_state.offset + relative_y;
                            if new_selected < self.cached_miller_len {
                                self.miller_state.selected = new_selected;
                                self.update_preview();
                            }
                        }
                    }
                }
            }
            MouseAction::DoubleClick { y, .. } if self.view == View::Explorer => {
                // Double-click to enter directory
                let list_top = self.estimated_list_top_row();

                if y >= list_top {
                    let relative_y = (y - list_top) as usize;

                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let new_selected = self.tree_state.offset + relative_y;
                            if new_selected < self.cached_tree_len {
                                self.tree_state.selected = new_selected;
                                self.drill_into_selected();
                            }
                        }
                        LayoutMode::Miller => {
                            let new_selected = self.miller_state.offset + relative_y;
                            if new_selected < self.cached_miller_len {
                                self.miller_state.selected = new_selected;
                                self.drill_into_miller_selected();
                            }
                        }
                    }
                }
            }
            MouseAction::RightClick { y, .. } if self.view == View::Explorer => {
                // Right-click to toggle mark
                let list_top = self.estimated_list_top_row();

                if y >= list_top {
                    let relative_y = (y - list_top) as usize;

                    match self.layout_mode {
                        LayoutMode::Tree => {
                            let new_selected = self.tree_state.offset + relative_y;
                            if new_selected < self.cached_tree_len {
                                self.tree_state.selected = new_selected;
                                self.toggle_mark();
                            }
                        }
                        LayoutMode::Miller => {
                            let new_selected = self.miller_state.offset + relative_y;
                            if new_selected < self.cached_miller_len {
                                self.miller_state.selected = new_selected;
                                self.toggle_mark();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Absolute row of the first list item, for click hit testing.
    ///
    /// Mirrors the renderer's vertical stack: the header row, an optional
    /// directory-tab row, the view-tab row, an optional scan/analysis/quick-list
    /// banner row, then the list block's top border (both the Tree and Miller
    /// widgets draw inside a bordered block). Hit testing is lightweight, but this
    /// must match the layout exactly or clicks land on the wrong row.
    fn estimated_list_top_row(&self) -> u16 {
        let chrome_rows = if self.tab_manager.len() > 1 { 3 } else { 2 };
        let banner_rows =
            u16::from(self.scan_progress.is_some() || self.analyzing || !self.has_full_scan);
        let border_row = 1;
        chrome_rows + banner_rows + border_row
    }

    /// Cycle to the next sort mode and apply sorting.
    fn cycle_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.apply_sort();
    }

    /// Reverse the current sort direction.
    fn reverse_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.reverse();
        self.apply_sort();
    }

    /// Sync current tab state to the active tab.
    fn sync_to_active_tab(&mut self) {
        if let Some(tab) = self.tab_manager.active_tab_mut() {
            tab.view_root = self.view_root.clone();
            tab.label = self
                .view_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.view_root.to_string_lossy().to_string());
        }
    }

    /// Sync app state from the active tab.
    fn sync_from_active_tab(&mut self) {
        if let Some(tab) = self.tab_manager.active_tab() {
            // Only sync if view_root is different
            if tab.view_root != self.view_root {
                self.view_root = tab.view_root.clone();
                // Reset tree state for the new view
                self.tree_state.selected = 0;
                self.tree_state.offset = 0;
                self.tree_state.expand(&self.view_root);
                self.miller_state.reset();
                match self.layout_mode {
                    LayoutMode::Tree => self.update_cached_tree_len(),
                    LayoutMode::Miller => self.update_cached_miller_len(),
                }
            }
        }
    }

    /// Start fuzzy search mode.
    fn start_search(&mut self) {
        // Populate search paths from the current tree
        self.search_state.activate();
        self.update_search_paths();
        self.mode = AppMode::Search;
    }

    /// Update search paths from the selected subtree, preserving preorder ranking.
    fn update_search_paths(&mut self) {
        let tree = self.tree.clone();
        let view_root = self.view_root.clone();
        self.search_state.build_paths(move |cancelled| {
            tree.as_ref().map_or_else(Vec::new, |tree| {
                Self::collect_search_paths(tree, &view_root, cancelled)
            })
        });
    }

    fn collect_search_paths(
        tree: &gravityfile_core::FileTree,
        view_root: &std::path::Path,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Vec<(PathBuf, String, bool)> {
        let Some(root) =
            Self::find_node_at_path_while(&tree.root, view_root, &tree.root_path, || {
                !cancelled.load(std::sync::atomic::Ordering::Relaxed)
            })
        else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        let mut current_path = view_root.to_path_buf();
        paths.push((current_path.clone(), ".".into(), root.is_dir()));
        // One iterator per depth avoids recursive calls and a width-sized stack
        // of allocated paths for directories with millions of children.
        let mut stack = vec![root.children.iter()];
        while let Some(children) = stack.last_mut() {
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return Vec::new();
            }
            if let Some(child) = children.next() {
                current_path.push(child.file_name());
                let display = gravityfile_core::escape_filename(
                    current_path
                        .strip_prefix(view_root)
                        .expect("search traversal stays inside its selected subtree")
                        .as_os_str(),
                )
                .into_owned();
                paths.push((current_path.clone(), display, child.is_dir()));
                stack.push(child.children.iter());
            } else {
                stack.pop();
                current_path.pop();
            }
        }
        paths
    }

    /// Handle key input during search mode.
    fn handle_search_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};

        match (key.code, key.modifiers) {
            // Cancel search
            (KeyCode::Esc, _) => {
                self.search_state.deactivate();
                self.mode = AppMode::Normal;
            }
            // Execute search (navigate to selected)
            (KeyCode::Enter, _) => {
                if self.search_state.is_searching() {
                    return;
                }
                if let Some(result) = self.search_state.selected_result() {
                    let target_path = result.path.clone();
                    self.navigate_to_search_result(&target_path);
                }
                self.search_state.deactivate();
                self.mode = AppMode::Normal;
            }
            // Navigation in results
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.search_state.move_up();
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.search_state.move_down();
            }
            // Cycle search mode
            (KeyCode::Tab, _) => {
                self.search_state.cycle_mode();
            }
            // Cursor movement
            (KeyCode::Left, _) => {
                self.search_state.move_cursor_left();
            }
            (KeyCode::Right, _) => {
                self.search_state.move_cursor_right();
            }
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.search_state.move_cursor_start();
            }
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.search_state.move_cursor_end();
            }
            // Delete
            (KeyCode::Backspace, _) => {
                self.search_state.delete_char_before();
            }
            (KeyCode::Delete, _) => {
                self.search_state.delete_char_at();
            }
            // Clear line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.search_state.clear_query();
            }
            // Type character
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.search_state.insert_char(c);
            }
            _ => {}
        }
    }

    /// Navigate to a search result path.
    fn navigate_to_search_result(&mut self, target: &Path) {
        // If it's a directory, drill into it
        // If it's a file, navigate to its parent and select it
        if target.is_dir() {
            // Save current state
            self.record_navigation_origin();

            // Navigate to directory
            self.view_root = target.to_path_buf();
            self.tree_state.selected = 0;
            self.tree_state.offset = 0;
            self.tree_state.expand(&self.view_root);
            self.miller_state.reset();

            match self.layout_mode {
                LayoutMode::Tree => self.update_cached_tree_len(),
                LayoutMode::Miller => self.update_cached_miller_len(),
            }
            self.sync_to_active_tab();
        } else {
            // It's a file - navigate to parent and select it
            if let Some(parent) = target.parent() {
                // Save current state
                self.record_navigation_origin();

                // Navigate to parent
                self.view_root = parent.to_path_buf();
                self.tree_state.expand(&self.view_root);

                // Get file name before borrowing for position search
                let file_name = target.file_name().map(|s| s.to_os_string());
                match self.layout_mode {
                    LayoutMode::Tree => {
                        // For tree view, we need to find the index in flattened list
                        self.tree_state.selected = 0;
                        self.update_cached_tree_len();

                        // Find position in flattened tree
                        if let Some((node, _)) = self.get_view_root_node() {
                            let items = TreeView::new(
                                node,
                                &self.view_root,
                                &self.theme,
                                &self.marked,
                                &self.clipboard,
                            )
                            .flatten(&self.tree_state);
                            if let Some(pos) = items.iter().position(|i| i.path == target) {
                                self.tree_state.selected = pos;
                            }
                        }
                    }
                    LayoutMode::Miller => {
                        // Find the index of the file in the parent's children
                        if let (Some(file_name), Some((node, _))) =
                            (&file_name, self.get_view_root_node())
                        {
                            let idx = node
                                .children
                                .iter()
                                .position(|c| c.file_name() == file_name)
                                .unwrap_or(0);
                            self.miller_state.selected = idx;
                        }
                        self.update_cached_miller_len();
                    }
                }
                self.sync_to_active_tab();
            }
        }
    }

    /// Handle key input during settings mode.
    fn handle_settings_input(&mut self, action: KeyAction) {
        match action {
            KeyAction::Cancel | KeyAction::Quit => {
                // Close settings without saving
                self.settings_state = None;
                self.mode = AppMode::Normal;
            }
            KeyAction::MoveUp => {
                if let Some(ref mut state) = self.settings_state {
                    state.move_up();
                }
            }
            KeyAction::MoveDown => {
                if let Some(ref mut state) = self.settings_state {
                    state.move_down();
                }
            }
            KeyAction::ToggleMark | KeyAction::DrillDown => {
                // Space or Enter toggles the current setting
                if let Some(ref mut state) = self.settings_state {
                    state.toggle_selected();
                }
            }
            KeyAction::Sort => self.save_settings_with(UserSettings::save),
            _ => {}
        }
    }

    /// Commit edited settings only after persistence succeeds; retain edits for retry.
    fn save_settings_with(&mut self, persist: impl FnOnce(&UserSettings) -> std::io::Result<()>) {
        if let Some(state) = &self.settings_state
            && state.dirty
        {
            if let Err(error) = persist(&state.settings) {
                self.error = Some(format!("Failed to save settings: {error}"));
                self.needs_redraw = true;
                return;
            }
            self.user_settings = state.settings.clone();
            self.scan_on_startup = self.user_settings.scan_on_startup;
        }
        self.settings_state = None;
        self.mode = AppMode::Normal;
        self.needs_redraw = true;
    }

    /// Apply the current sort mode to the file tree.
    fn apply_sort(&mut self) {
        if let Some(ref mut tree) = self.tree {
            Self::sort_node_tree(&mut Arc::make_mut(tree).root, self.sort_mode);
            // Update cached lengths after sorting
            match self.layout_mode {
                LayoutMode::Tree => self.update_cached_tree_len(),
                LayoutMode::Miller => self.update_cached_miller_len(),
            }
            if self.view == View::Treemap {
                self.update_cached_treemap_len();
            }
        }
    }

    /// Sort a tree using one child iterator per depth instead of recursive calls.
    fn sort_node_tree(node: &mut gravityfile_core::FileNode, mode: SortMode) {
        let compare = |a: &gravityfile_core::FileNode, b: &gravityfile_core::FileNode| {
            (match mode {
                SortMode::SizeDescending => b.size.cmp(&a.size),
                SortMode::SizeAscending => a.size.cmp(&b.size),
                SortMode::NameAscending => a.compare_name(b),
                SortMode::NameDescending => b.compare_name(a),
                SortMode::ModifiedDescending => b.timestamps.modified.cmp(&a.timestamps.modified),
                SortMode::ModifiedAscending => a.timestamps.modified.cmp(&b.timestamps.modified),
                SortMode::CountDescending => {
                    let a_count = a.children.len();
                    let b_count = b.children.len();
                    b_count.cmp(&a_count)
                }
                SortMode::CountAscending => {
                    let a_count = a.children.len();
                    let b_count = b.children.len();
                    a_count.cmp(&b_count)
                }
            })
            .then_with(|| a.compare_name(b))
        };
        node.children.sort_by(compare);
        let mut stack = vec![node.children.iter_mut()];
        while let Some(children) = stack.last_mut() {
            let Some(child) = children.next() else {
                stack.pop();
                continue;
            };
            child.children.sort_by(compare);
            stack.push(child.children.iter_mut());
        }
    }

    /// Get duplicate groups filtered to current view_root.
    fn get_filtered_duplicates(&self) -> Option<(Vec<&gravityfile_analyze::DuplicateGroup>, u64)> {
        let report = self.duplicates.as_ref()?;
        let cached = self
            .duplicate_filter
            .get(report, &self.view_root, &self.path);
        Some((
            cached
                .groups
                .iter()
                .map(|(index, _)| &report.groups[*index])
                .collect(),
            cached.wasted,
        ))
    }

    fn filtered_duplicate_path_count(&self, index: usize) -> usize {
        let Some(report) = &self.duplicates else {
            return 0;
        };
        self.duplicate_filter
            .get(report, &self.view_root, &self.path)
            .groups
            .get(index)
            .map_or(0, |(_, count)| *count)
    }

    /// Get stale directories filtered to current view_root.
    fn get_filtered_stale_dirs(&self) -> Option<Vec<&gravityfile_analyze::StaleDirectory>> {
        let age = self.age_report.as_ref()?;

        if self.view_root == self.path {
            return Some(age.stale_directories.iter().collect());
        }

        let filtered: Vec<&gravityfile_analyze::StaleDirectory> = age
            .stale_directories
            .iter()
            .filter(|d| d.path.starts_with(&self.view_root))
            .collect();

        Some(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_dir_on_disk;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    #[test]
    fn all_explorer_sort_modes_handle_deep_trees() {
        use gravityfile_core::{FileNode, NodeId, Timestamps};
        let depth = 20_000u64;
        let mut root = FileNode::new_directory(
            NodeId::new(depth),
            "d",
            Timestamps::with_modified(std::time::UNIX_EPOCH),
        );
        for id in (0..depth).rev() {
            let mut parent = FileNode::new_directory(
                NodeId::new(id),
                "d",
                Timestamps::with_modified(std::time::UNIX_EPOCH),
            );
            parent
                .children
                .push(crate::test_support::file(depth + id + 1, "z", 10));
            let mut file = crate::test_support::file(depth + id + 1, "z", 10);
            file.timestamps.modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
            parent.children.push(file);
            parent.children.push(root);
            root = parent;
        }
        let mut all_sorted = true;
        for mode in [
            SortMode::SizeDescending,
            SortMode::SizeAscending,
            SortMode::NameAscending,
            SortMode::NameDescending,
            SortMode::ModifiedDescending,
            SortMode::ModifiedAscending,
            SortMode::CountDescending,
            SortMode::CountAscending,
        ] {
            App::sort_node_tree(&mut root, mode);
            let mut current = &root;
            while current.children.len() == 2 {
                let directory = current.children.iter().find(|node| node.is_dir()).unwrap();
                let file_first = match mode {
                    SortMode::SizeDescending
                    | SortMode::NameDescending
                    | SortMode::ModifiedDescending => true,
                    SortMode::CountAscending => !directory.children.is_empty(),
                    _ => false,
                };
                all_sorted &= current.children[0].is_file() == file_first;
                current = directory;
            }
        }
        let mut pending = std::mem::take(&mut root.children);
        while let Some(mut node) = pending.pop() {
            pending.append(&mut node.children);
        }
        assert!(all_sorted);
    }

    #[cfg(unix)]
    #[test]
    fn native_name_ties_sort_consistently_across_explorer_modes() {
        use std::os::unix::ffi::OsStringExt;
        let low = std::ffi::OsString::from_vec(b"same-\xfe".to_vec());
        let high = std::ffi::OsString::from_vec(b"same-\xff".to_vec());
        let mut a = crate::test_support::file(1, "", 10);
        a.set_file_name(&low);
        let mut b = a.clone();
        b.id = gravityfile_core::NodeId::new(2);
        b.set_file_name(&high);
        assert_eq!(a.name, b.name);
        for mode in [
            SortMode::SizeDescending,
            SortMode::SizeAscending,
            SortMode::NameAscending,
            SortMode::NameDescending,
            SortMode::ModifiedDescending,
            SortMode::ModifiedAscending,
            SortMode::CountDescending,
            SortMode::CountAscending,
        ] {
            for children in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
                let mut root = crate::test_support::directory(0, "root", children);
                App::sort_node_tree(&mut root, mode);
                let expected = if matches!(mode, SortMode::NameDescending) {
                    &high
                } else {
                    &low
                };
                assert_eq!(root.children[0].file_name(), expected);
                root.sort_children_by_size();
                assert_eq!(root.children[0].file_name(), &low);
            }
        }
        let fixture = tempfile::tempdir().unwrap();
        let mut app = App::with_loaded_settings(
            fixture.path().into(),
            crate::TuiConfig::default(),
            Ok(UserSettings::default()),
        );
        let tree = Arc::make_mut(app.tree.as_mut().unwrap());
        tree.root = crate::test_support::directory(0, "root", vec![b, a]);
        let rects = crate::ui::TreemapView::new(
            &tree.root,
            &tree.root_path,
            &app.theme,
            &app.marked,
            &app.clipboard,
        )
        .calculate_rects(Rect::new(0, 0, 80, 24));
        assert_eq!(rects.len(), 2);
        for (index, expected) in [low, high].iter().enumerate() {
            app.treemap_state.selected = index;
            let (_, selected) = app.get_selected_treemap_child().unwrap();
            assert_eq!(selected, app.view_root.join(expected));
            assert_eq!(rects[index].path, selected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_name_collisions_keep_selection_lookup_and_search_paths_distinct() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = tempfile::tempdir().unwrap();
        let mut app = App::with_loaded_settings(
            fixture.path().into(),
            crate::TuiConfig::default(),
            Ok(UserSettings::default()),
        );
        let names = [
            std::ffi::OsString::from_vec(b"bad-\xff".to_vec()),
            std::ffi::OsString::from_vec(b"bad-\xfe".to_vec()),
        ];
        let tree = Arc::make_mut(app.tree.as_mut().unwrap());
        for (index, name) in names.iter().enumerate() {
            let mut child = crate::test_support::file(index as u64 + 1, "", 1);
            child.set_file_name(name);
            tree.root.children.push(child);
        }
        assert_eq!(tree.root.children[0].name, tree.root.children[1].name);
        assert_ne!(
            tree.root.children[0].display_name(),
            tree.root.children[1].display_name()
        );
        let search = App::collect_search_paths(
            tree,
            &tree.root_path,
            &std::sync::atomic::AtomicBool::new(false),
        );
        for name in &names {
            assert!(
                search
                    .iter()
                    .any(|(path, _, _)| path == &tree.root_path.join(name))
            );
        }
        app.layout_mode = LayoutMode::Miller;
        for (index, name) in names.iter().enumerate() {
            app.miller_state.selected = index;
            let path = app.view_root.join(name);
            assert_eq!(app.get_selected_path(), Some(path.clone()));
            let tree = app.tree.as_ref().unwrap();
            assert_eq!(
                App::find_node_at_path(&tree.root, &path, &tree.root_path)
                    .unwrap()
                    .file_name(),
                name
            );
            app.select_child_by_name(&path);
            assert_eq!(app.miller_state.selected, index);
        }
        let displayed = text(&render(&app, Rect::new(0, 0, 200, 30)));
        assert!(displayed.contains(r"bad-\xff"), "{displayed}");
        assert!(displayed.contains(r"bad-\xfe"), "{displayed}");
    }

    #[tokio::test]
    async fn saved_entry_budget_reaches_startup_navigation_and_rescans() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("nested")).unwrap();
        std::fs::write(fixture.path().join("nested/one"), b"one").unwrap();
        std::fs::write(fixture.path().join("nested/two"), b"two").unwrap();
        let settings = UserSettings {
            max_scan_entries: Some(1),
            ..UserSettings::default()
        };
        let mut app = App::with_loaded_settings(
            fixture.path().into(),
            crate::TuiConfig::default(),
            Ok(settings),
        );
        assert_eq!(app.tree.as_ref().unwrap().config.max_entries, Some(1));
        assert!(matches!(
            quick_list_with_settings(&fixture.path().join("nested"), &app.user_settings),
            Err(gravityfile_scan::ScanError::EntryLimitExceeded { limit: 1 })
        ));
        for _ in 0..2 {
            app.start_scan();
            let mut receiver = app.scan_rx.take().unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    if let ScanResult::ScanComplete(result) = receiver
                        .recv()
                        .await
                        .expect("scan worker closed without a final result")
                    {
                        break result;
                    }
                }
            })
            .await
            .unwrap();
            assert!(matches!(
                result,
                Err(gravityfile_scan::ScanError::EntryLimitExceeded { limit: 1 })
            ));
            app.handle_scan_result(ScanResult::ScanComplete(result));
            assert!(
                app.error
                    .as_ref()
                    .unwrap()
                    .contains("Scan entry limit exceeded")
            );
            assert!(app.scan_progress.is_none());
            assert!(!app.analyzing);
            assert_eq!(app.tree.as_ref().unwrap().config.max_depth, Some(1));
        }
        let over_budget = App::with_loaded_settings(
            fixture.path().join("nested"),
            crate::TuiConfig::default(),
            Ok(app.user_settings.clone()),
        );
        assert!(over_budget.tree.is_none());
        assert!(
            over_budget
                .error
                .as_ref()
                .unwrap()
                .contains("Scan entry limit exceeded")
        );
    }

    #[tokio::test]
    async fn preview_requests_coalesce_and_forced_reload_rejects_old_completion() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("latest.txt");
        std::fs::write(&path, b"fresh").unwrap();
        let mut app = App::with_loaded_settings(
            fixture.path().into(),
            crate::TuiConfig::default(),
            Ok(UserSettings::default()),
        );
        let (sender, receiver) = oneshot::channel();
        app.preview_rx = Some(receiver); // Hold an active load deterministically.
        for index in 0..1000 {
            app.schedule_preview(fixture.path().join(format!("obsolete-{index}")), true);
        }
        app.schedule_preview(path.clone(), true);
        assert_eq!(app.pending_preview.as_ref().unwrap().path, path);
        assert!(!sender.is_closed()); // No schedule replaced the active receiver.
        sender
            .send(PreviewLoad {
                path: path.clone(),
                mode: app.preview_state.mode,
                result: Ok(PreviewContent::Error("stale".into())),
            })
            .unwrap_or_else(|_| panic!("receiver lost"));
        let old = app.preview_rx.take().unwrap().await.unwrap();
        app.finish_preview(Some(old));
        assert!(matches!(app.preview_state.content, PreviewContent::Loading));
        assert!(app.pending_preview.is_none());
        let latest = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.preview_rx.take().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(latest.path, path);
        app.finish_preview(Some(latest));
        let PreviewContent::Text { lines, .. } = &app.preview_state.content else {
            panic!("latest preview not installed")
        };
        assert_eq!(lines[0].to_string(), "fresh");
        assert!(!app.preview_state.loading);
    }

    #[tokio::test]
    async fn clearing_preview_retains_active_slot_and_recovers_from_worker_loss() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("latest.txt");
        std::fs::write(&path, b"fresh").unwrap();
        let mut app = App::with_loaded_settings(
            fixture.path().into(),
            crate::TuiConfig::default(),
            Ok(UserSettings::default()),
        );
        let (sender, receiver) = oneshot::channel();
        app.preview_rx = Some(receiver);
        app.schedule_preview(path.clone(), false);
        app.clear_preview();
        assert!(app.preview_rx.is_some());
        assert!(app.pending_preview.is_none());
        assert!(matches!(app.preview_state.content, PreviewContent::Empty));
        app.schedule_preview(path, false);
        assert!(!sender.is_closed());
        drop(sender);
        assert!(app.preview_rx.take().unwrap().await.is_err());
        app.finish_preview(None);
        let latest = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.preview_rx.take().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        app.finish_preview(Some(latest));
        assert!(matches!(
            app.preview_state.content,
            PreviewContent::Text { .. }
        ));
        app.preview_state.loading = true;
        app.finish_preview(None);
        assert!(matches!(
            app.preview_state.content,
            PreviewContent::Error(_)
        ));
        assert!(!app.preview_state.loading);
    }

    #[test]
    #[ignore = "manual optimized background search measurement"]
    fn background_search_scale_benchmark() {
        use crate::test_support::{directory, file};
        use std::time::Instant;
        let mut measurements = Vec::new();
        for count in [100_000, 1_000_000] {
            let fixture_started = Instant::now();
            let root = sample_dir_on_disk();
            let mut app = App::with_config(
                root.path().canonicalize().unwrap(),
                crate::TuiConfig::default(),
            );
            Arc::make_mut(app.tree.as_mut().unwrap()).root = directory(
                0,
                "root",
                (1..=count)
                    .map(|index| file(index, &format!("report-{index:07}.rs"), 1))
                    .collect(),
            );
            app.search_state.enable_background();
            let fixture_ms = fixture_started.elapsed().as_secs_f64() * 1000.0;
            for sample in 0..3 {
                let started = Instant::now();
                app.start_search();
                let activation_ms = started.elapsed().as_secs_f64() * 1000.0;
                let submit_started = Instant::now();
                app.search_state.set_query("rpt".into());
                let submit_ms = submit_started.elapsed().as_secs_f64() * 1000.0;
                let mut poll_max_ms = 0.0_f64;
                let mut polls = 0;
                while app.search_state.has_background_work() {
                    let poll_started = Instant::now();
                    app.search_state.poll_background();
                    poll_max_ms = poll_max_ms.max(poll_started.elapsed().as_secs_f64() * 1000.0);
                    polls += 1;
                    assert!(started.elapsed() < std::time::Duration::from_secs(30));
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let completion_ms = started.elapsed().as_secs_f64() * 1000.0;
                assert!(app.search_state.error.is_none());
                assert_eq!(app.search_state.results.len(), 1000);
                assert!(
                    app.search_state
                        .results
                        .iter()
                        .all(|result| result.display.starts_with("report-"))
                );
                measurements.push(serde_json::json!({"paths": count, "sample": sample,
                    "fixture_ms": fixture_ms, "activation_ms": activation_ms,
                    "query_submission_ms": submit_ms, "max_poll_ms": poll_max_ms,
                    "completion_ms": completion_ms, "polls": polls}));
            }
        }
        println!(
            "BACKGROUND_SEARCH_JSON={}",
            serde_json::json!({"schema_version": 1,
            "measurements": measurements, "limits": ["Synthetic flat FileNode tree; actual app index collector",
            "1 ms test polling, not the interactive event loop", "No terminal rendering or filesystem scan",
            "Fixture construction excluded from completion time", "Three samples; uncontrolled host load"]})
        );
    }

    #[tokio::test]
    async fn incomplete_undo_requeues_the_remainder_for_retry() {
        let (_dir, mut app) = scanned_app();
        let remaining = gravityfile_ops::UndoEntry::new(
            7,
            gravityfile_ops::UndoableOperation::FilesMoved {
                moves: vec![(PathBuf::from("/a"), PathBuf::from("/b"))],
            },
            "Moved 2 item(s) (undo incomplete)",
        );
        app.handle_scan_result(ScanResult::OperationComplete {
            cancelled: false,
            operation_type: gravityfile_ops::OperationType::Move,
            succeeded: 1,
            failed: 1,
            skipped: 0,
            bytes_processed: 0,
            errors: vec![gravityfile_ops::OperationError::new(
                "/b".into(),
                "occupied",
            )],
            undo_info: None,
            undo_remaining: Some(remaining),
        });
        assert_eq!(app.undo_log.peek().unwrap().id, 7);
        assert!(app.error.as_deref().unwrap().contains("press u to retry"));
        let (success, message) = app.operation_message.clone().unwrap();
        assert!(!success && message.contains("undo incomplete"));
    }

    #[tokio::test]
    async fn delete_and_marks_never_target_the_scan_root() {
        let (_dir, mut app) = scanned_app();
        app.tree_state.selected = 0;
        assert_eq!(app.get_selected_path().unwrap(), app.view_root);
        app.handle_action(KeyAction::Delete);
        assert!(app.marked.is_empty());
        assert_eq!(app.mode, AppMode::Normal);
        assert!(!app.operation_message.as_ref().unwrap().0);
        app.handle_action(KeyAction::ToggleMark);
        assert!(app.marked.is_empty());
        app.marked.insert(app.path.clone());
        app.marked.insert(app.path.parent().unwrap().to_path_buf());
        app.mode = AppMode::ConfirmDelete;
        app.handle_action(KeyAction::Confirm);
        assert!(
            app.pending_deletion.is_none(),
            "protected paths must be filtered out before deletion"
        );
    }

    #[tokio::test]
    async fn declining_a_delete_keeps_hand_made_marks() {
        let (_dir, mut app) = scanned_app();
        let chosen = app.path.join("copy-a.bin");
        app.marked.insert(chosen.clone());
        app.handle_action(KeyAction::Delete);
        assert_eq!(app.mode, AppMode::ConfirmDelete);
        app.handle_action(KeyAction::Cancel);
        assert!(
            app.marked.contains(&chosen),
            "user marks survive a declined delete"
        );

        app.marked.clear();
        app.tree_state.selected = tree_row(&app, "copy-a.bin");
        app.handle_action(KeyAction::Delete);
        assert_eq!(app.mode, AppMode::ConfirmDelete);
        app.handle_action(KeyAction::Cancel);
        assert!(app.marked.is_empty(), "the automatic mark is dropped");
    }

    #[tokio::test]
    async fn marking_a_duplicate_group_keeps_one_copy() {
        use gravityfile_analyze::{AnalysisStatus, DuplicateGroup, DuplicateReport};
        let (_dir, mut app) = scanned_app();
        let paths = vec![app.path.join("copy-a.bin"), app.path.join("copy-b.bin")];
        let report = Arc::new(DuplicateReport {
            status: AnalysisStatus::default(),
            groups: vec![DuplicateGroup {
                hash: gravityfile_core::ContentHash::new([0; 32]),
                size: 8192,
                paths: paths.clone(),
                wasted_bytes: 8192,
            }],
            total_duplicate_size: 16384,
            total_wasted_space: 8192,
            files_analyzed: 2,
            files_with_duplicates: 2,
            group_count: 1,
            groups_omitted: 0,
        });
        let counts =
            duplicate_filter::prepare_counts(&report, &app.path, &CancellationToken::new())
                .unwrap();
        app.duplicate_filter.seed(&report, app.path.clone(), counts);
        app.duplicates = Some(report);
        app.view = View::Duplicates;
        app.handle_action(KeyAction::ToggleMark);
        assert_eq!(app.marked, HashSet::from([paths[1].clone()]));
        app.handle_action(KeyAction::ToggleMark);
        assert!(app.marked.is_empty());
    }

    #[tokio::test]
    async fn clicks_hit_the_row_on_screen_after_scrolling() {
        let (_dir, mut app) = scanned_app();
        let src = tree_row(&app, "src");
        app.tree_state.selected = src;
        app.toggle_selected();
        let area = Rect::new(0, 0, 80, 12);
        app.tree_state.selected = app.cached_tree_len - 1;
        render(&app, area);
        app.adopt_rendered_offsets();
        let offset = app.tree_state.offset;
        assert!(offset > 0, "the selection forced a scroll");
        let top = app.estimated_list_top_row();
        app.handle_mouse_action(MouseAction::Click { x: 5, y: top });
        assert_eq!(
            app.tree_state.selected, offset,
            "the first visible row was hit"
        );
    }

    #[test]
    fn back_reloads_replaced_tree_and_retains_unavailable_history() {
        for layout in [LayoutMode::Tree, LayoutMode::Miller] {
            let fixture = tempfile::tempdir().unwrap();
            let origin = fixture.path().join("origin");
            let destination = fixture.path().join("destination");
            std::fs::create_dir(&origin).unwrap();
            std::fs::create_dir(&destination).unwrap();
            std::fs::write(origin.join("original-file"), "contents").unwrap();
            let mut app = App::with_loaded_settings(
                origin.clone(),
                crate::TuiConfig::default(),
                Ok(UserSettings::default()),
            );
            app.layout_mode = layout;
            app.scan_on_startup = false;
            let origin = app.path.clone();
            app.user_settings.bookmarks.set('d', destination);
            app.jump_to_bookmark('d');
            let current = app.view_root.clone();
            let token = app.scan_token.clone();
            app.navigate_back();
            assert!(token.is_cancelled());
            assert_eq!(app.view_root, origin);
            assert_eq!(app.tree.as_ref().unwrap().root_path, origin);
            assert!(
                app.get_view_root_node()
                    .unwrap()
                    .0
                    .children
                    .iter()
                    .any(|node| &*node.name == "original-file")
            );
            assert!(app.view_history.is_empty());
            assert_eq!(app.forward_history.last().unwrap().0, current);
            app.jump_to_bookmark('d');
            let before = Arc::clone(app.tree.as_ref().unwrap());
            let history = app.view_history.clone();
            let forward = app.forward_history.clone();
            std::fs::rename(&origin, fixture.path().join("moved-origin")).unwrap();
            app.navigate_back();
            assert_eq!(app.view_root, current);
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &before));
            assert_eq!(app.view_history, history);
            assert_eq!(app.forward_history, forward);
            assert!(app.error.as_ref().unwrap().contains("Cannot list"));
        }
    }

    #[test]
    fn bookmark_failures_preserve_view_and_unloaded_destinations_get_a_tree() {
        let fixture = sample_dir_on_disk();
        let mut app = App::with_loaded_settings(
            fixture.path().to_path_buf(),
            crate::TuiConfig::default(),
            Ok(UserSettings::default()),
        );
        app.scan_on_startup = false;
        let original = Arc::clone(app.tree.as_ref().unwrap());
        let root = app.view_root.clone();
        let file = root.join("bookmark-file");
        std::fs::write(&file, "file").unwrap();
        app.user_settings.bookmarks.set('f', file);
        app.user_settings.bookmarks.set('m', root.join("missing"));
        for key in ['z', 'f', 'm'] {
            app.jump_to_bookmark(key);
            assert!(app.error.is_some());
            assert_eq!(app.view_root, root);
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &original));
            assert!(app.view_history.is_empty());
        }
        let destination = root.join("new/nested");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("needle"), "contents").unwrap();
        app.user_settings
            .bookmarks
            .set('n', root.join("new/./nested"));
        let old_token = app.scan_token.clone();
        app.jump_to_bookmark('n');
        assert!(old_token.is_cancelled());
        assert_eq!(app.view_root, destination);
        assert_eq!(app.path, destination);
        assert_eq!(app.tree.as_ref().unwrap().root_path, destination);
        assert!(
            app.get_view_root_node()
                .unwrap()
                .0
                .children
                .iter()
                .any(|child| &*child.name == "needle")
        );
        assert_eq!(app.view_history.len(), 1);
        app.jump_to_bookmark('n');
        assert_eq!(app.view_history.len(), 1);
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&destination, &alias).unwrap();
            app.user_settings.bookmarks.set('a', alias);
            app.jump_to_bookmark('a');
            assert_eq!(app.view_root, destination);
            assert_eq!(app.view_history.len(), 1);
        }
    }

    #[test]
    fn cd_normalizes_components_and_reports_failures_without_navigation() {
        use crate::test_support::{directory, file};
        let fixture = sample_dir_on_disk();
        let mut app = App::with_loaded_settings(
            fixture.path().to_path_buf(),
            crate::TuiConfig::default(),
            Ok(UserSettings::default()),
        );
        let root = app.path.clone();
        Arc::make_mut(app.tree.as_mut().unwrap()).root = directory(
            0,
            "root",
            vec![
                directory(1, "first", vec![]),
                directory(2, "two  日本", vec![]),
                file(3, "file.txt", 1),
            ],
        );
        app.cd_to_path("./first");
        assert_eq!(app.view_root, root.join("first"));
        app.cd_to_path("../two  日本/.");
        assert_eq!(app.view_root, root.join("two  日本"));
        let history_len = app.view_history.len();
        app.cd_to_path(".");
        assert_eq!(app.view_history.len(), history_len);
        for (target, message) in [
            ("/missing", "not in the loaded tree"),
            ("/file.txt", "Not a directory"),
            ("../../escape", "leaves the scan root"),
        ] {
            app.cd_to_path(target);
            assert!(app.error.as_ref().unwrap().contains(message));
            assert_eq!(app.view_root, root.join("two  日本"));
            assert_eq!(app.view_history.len(), history_len);
        }
        app.cd_to_path("/first/../two  日本");
        assert_eq!(app.view_root, root.join("two  日本"));
        assert_eq!(app.view_history.len(), history_len);
        app.tree = None;
        app.cd_to_path("first");
        assert!(app.error.as_ref().unwrap().contains("no directory tree"));
        assert_eq!(app.view_history.len(), history_len);
    }

    #[test]
    fn constructor_uses_listing_root_for_relative_and_aliased_paths() {
        const WORKER: &str = "GRAVITYFILE_TEST_RELATIVE_ROOT_WORKER";
        if std::env::var_os(WORKER).is_none() {
            // Relative paths need a writable working directory, but the checkout
            // may be read-only. Isolate cwd in a child instead of changing the
            // process-wide directory beneath concurrently running tests.
            let workspace = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::constructor_uses_listing_root_for_relative_and_aliased_paths",
                    "--nocapture",
                ])
                .env(WORKER, "1")
                .current_dir(workspace.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        let fixture = tempfile::tempdir_in(".").unwrap();
        let real = fixture.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("needle.txt"), "contents").unwrap();
        let canonical = real.canonicalize().unwrap();
        let mut inputs = vec![real.clone(), real.join("."), real.join("../real")];
        #[cfg(unix)]
        {
            let alias = fixture.path().join("alias");
            std::os::unix::fs::symlink(&canonical, &alias).unwrap();
            inputs.push(alias);
        }
        // Keep the collection mutable on platforms without symlink fixtures.
        inputs.push(canonical.clone());
        for input in inputs {
            let mut app = App::with_loaded_settings(
                input,
                crate::TuiConfig::default(),
                Ok(UserSettings::default()),
            );
            assert_eq!(app.path, canonical);
            assert_eq!(app.view_root, canonical);
            assert_eq!(app.tree.as_ref().unwrap().root_path, canonical);
            assert_eq!(app.tab_manager.active_tab().unwrap().view_root, canonical);
            assert!(app.get_view_root_node().is_some());
            app.search_state.enable_background();
            app.start_search();
            app.search_state.set_query("needle".into());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while app.search_state.has_background_work() {
                app.search_state.poll_background();
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert!(app.search_state.error.is_none());
            assert_eq!(app.search_state.results.len(), 1);
            assert_eq!(
                app.search_state.results[0].path,
                canonical.join("needle.txt")
            );
        }
    }

    #[test]
    fn scope_lookup_cancellation_stops_within_wide_directory() {
        use crate::test_support::{directory, file};
        let root = directory(
            0,
            "root",
            (1..=1000)
                .map(|index| file(index, &format!("file-{index:04}"), 1))
                .collect(),
        );
        let root_path = Path::new("root");
        let target = root_path.join("file-1000");
        let mut checks = 0;
        let result = App::find_node_at_path_while(&root, &target, root_path, || {
            checks += 1;
            checks <= 10
        });
        assert!(result.is_none());
        assert_eq!(checks, 11);
        assert_eq!(
            &*App::find_node_at_path(&root, &target, root_path)
                .unwrap()
                .name,
            "file-1000"
        );
        assert!(App::find_node_at_path_while(&root, root_path, root_path, || false).is_none());
    }

    #[test]
    fn search_index_scopes_nested_directories_and_preserves_preorder() {
        use crate::test_support::{directory, file};
        let root = sample_dir_on_disk();
        let mut app = App::with_config(root.path().to_path_buf(), crate::TuiConfig::default());
        let scan_root = app.tree.as_ref().unwrap().root_path.clone();
        Arc::make_mut(app.tree.as_mut().unwrap()).root = directory(
            0,
            "root",
            vec![
                directory(
                    1,
                    "src",
                    vec![
                        file(2, "日本.rs", 1),
                        directory(3, "nested", vec![file(4, "deep.rs", 1)]),
                    ],
                ),
                directory(5, "src-other", vec![file(6, "outside.rs", 1)]),
            ],
        );
        let tree = app.tree.as_ref().unwrap();
        for (scope, expected) in [
            (
                scan_root.clone(),
                vec![
                    ".",
                    "src",
                    "src/nested",
                    "src/nested/deep.rs",
                    "src/日本.rs",
                    "src-other",
                    "src-other/outside.rs",
                ],
            ),
            (
                scan_root.join("src"),
                vec![".", "nested", "nested/deep.rs", "日本.rs"],
            ),
            (scan_root.join("src/nested"), vec![".", "deep.rs"]),
            (scan_root.join("missing"), vec![]),
            (scan_root.parent().unwrap().to_path_buf(), vec![]),
        ] {
            let paths =
                App::collect_search_paths(tree, &scope, &std::sync::atomic::AtomicBool::new(false));
            let expected: Vec<_> = expected.iter().map(PathBuf::from).collect();
            assert_eq!(
                paths
                    .iter()
                    .map(|(_, display, _)| PathBuf::from(display))
                    .collect::<Vec<_>>(),
                expected
            );
            for (path, display, _) in paths {
                assert!(path.starts_with(&scope));
                assert_eq!(
                    path,
                    if display == "." {
                        scope.clone()
                    } else {
                        scope.join(display)
                    }
                );
            }
        }
        app.view_root = scan_root.join("src/nested");
        app.search_state.enable_background();
        app.start_search();
        app.search_state.set_query("deep".into());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.search_state.has_background_work() {
            app.search_state.poll_background();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(app.search_state.results.len(), 1);
        assert_eq!(
            app.search_state.results[0].path,
            scan_root.join("src/nested/deep.rs")
        );
    }

    #[test]
    fn pending_search_renders_feedback_and_enter_waits_for_current_results() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let root = sample_dir_on_disk();
        let mut app = App::with_config(root.path().to_path_buf(), crate::TuiConfig::default());
        app.mode = AppMode::Search;
        app.search_state.activate();
        app.search_state.enable_background();
        app.search_state.set_query("file".into());
        app.handle_search_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, AppMode::Search);
        assert!(app.search_state.active);
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        app.render(area, &mut buffer);
        let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("Searching…"));
        assert!(!rendered.contains("No matches found"));
        app.handle_search_input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, AppMode::Normal);
        assert!(!app.search_state.is_searching());
    }

    #[test]
    fn failed_initial_listing_is_reported_as_an_error() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let app = App::with_config(missing, crate::TuiConfig::default());
        assert!(app.tree.is_none());
        assert!(
            app.error
                .as_deref()
                .is_some_and(|error| error.contains("Cannot list"))
        );
    }

    #[test]
    fn lazy_listing_failure_preserves_tree_and_invalidates_stale_parent_cache() {
        let root = sample_dir_on_disk();
        let path = root.path().canonicalize().unwrap();
        let mut app = App::with_config(path.clone(), crate::TuiConfig::default());
        let original = Arc::clone(app.tree.as_ref().unwrap());
        app.lazy_load_directory(&path.join("missing"));
        assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &original));
        assert!(
            app.error
                .as_deref()
                .is_some_and(|error| error.contains("Cannot list"))
        );
        app.cached_parent_tree = Some((*original).clone());
        app.tree = None;
        app.update_cached_parent();
        assert!(app.cached_parent_tree.is_none());
    }

    /// Terminal sizes chosen to bracket every layout threshold: below the
    /// minimum width for a details panel, below the width of a right-anchored
    /// size bar, and tall-and-narrow (a short terminal exits the row loop
    /// before the interesting arithmetic ever runs).
    const SIZES: &[(u16, u16)] = &[
        (1, 1),
        (2, 2),
        (3, 20),
        (4, 3),
        (5, 24),
        (6, 30),
        (7, 12),
        (8, 5),
        (8, 30),
        (9, 40),
        (10, 6),
        (10, 30),
        (11, 24),
        (12, 8),
        (12, 40),
        (14, 30),
        (16, 10),
        (18, 24),
        (20, 12),
        (24, 10),
        (24, 30),
        (30, 14),
        (36, 24),
        (40, 16),
        (44, 30),
        (50, 20),
        (60, 24),
        (79, 24),
        (80, 24),
        (81, 24),
        (100, 30),
        (120, 40),
        (200, 60),
    ];

    const VIEWS: &[View] = &[
        View::Explorer,
        View::Duplicates,
        View::Age,
        View::Errors,
        View::Treemap,
    ];

    const MODES: &[AppMode] = &[
        AppMode::Normal,
        AppMode::Help,
        AppMode::Search,
        AppMode::ConfirmDelete,
        AppMode::Deleting,
        AppMode::Copying,
        AppMode::Moving,
        AppMode::Undoing,
        AppMode::UpdatingEntry,
        AppMode::Renaming,
        AppMode::CreatingFile,
        AppMode::CreatingDirectory,
        AppMode::Taking,
        AppMode::GoingTo,
        AppMode::Command,
        AppMode::Settings,
        AppMode::SettingBookmark,
        AppMode::JumpingToBookmark,
        AppMode::BookmarkList,
        AppMode::ConfirmBulkRename,
        AppMode::BulkRenaming,
        AppMode::Visual,
    ];

    #[test]
    fn settings_failure_disables_automatic_scans_but_honors_explicit_override() {
        let directory = tempfile::tempdir().unwrap();
        for override_value in [None, Some(false), Some(true)] {
            let config = crate::TuiConfig {
                scan_on_startup: override_value,
                ..Default::default()
            };
            let app = App::with_loaded_settings(
                directory.path().into(),
                config,
                Err(std::io::Error::other("unreadable settings")),
            );
            assert_eq!(app.scan_on_startup, override_value.unwrap_or(false));
            assert!(!app.user_settings.scan_on_startup);
            assert!(
                app.error
                    .as_ref()
                    .unwrap()
                    .contains("automatic scanning disabled")
            );
            assert!(app.tree.is_some()); // Quick browsing remains available.
        }
        let app = App::with_loaded_settings(
            directory.path().into(),
            crate::TuiConfig::default(),
            Ok(UserSettings::default()),
        );
        assert!(app.scan_on_startup);
        assert!(app.error.is_none());
    }

    #[test]
    fn startup_reports_settings_and_listing_failures_together() {
        let directory = tempfile::tempdir().unwrap();
        let app = App::with_loaded_settings(
            directory.path().join("missing"),
            crate::TuiConfig::default(),
            Err(std::io::Error::other("invalid settings fixture")),
        );
        let error = app.error.as_ref().unwrap();
        assert!(error.contains("invalid settings fixture"));
        assert!(error.contains("Cannot list"));
        assert!(!app.user_settings.scan_on_startup);
        assert!(app.image_previews_enabled());
    }

    #[test]
    fn settings_save_failure_retains_edits_and_successful_retry_applies_them() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        let original_startup = app.user_settings.scan_on_startup;
        let original_runtime_startup = app.scan_on_startup;
        let mut edited = SettingsState::new(app.user_settings.clone());
        edited.settings.scan_on_startup = !original_startup;
        edited.dirty = true;
        app.settings_state = Some(edited);
        app.mode = AppMode::Settings;
        app.needs_redraw = false;

        app.save_settings_with(|_| Err(std::io::Error::other("injected storage failure")));
        assert_eq!(app.user_settings.scan_on_startup, original_startup);
        assert_eq!(app.scan_on_startup, original_runtime_startup);
        assert_eq!(app.mode, AppMode::Settings);
        assert!(app.settings_state.as_ref().unwrap().dirty);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("injected storage failure")
        );
        assert!(app.needs_redraw);

        app.save_settings_with(|settings| {
            assert_eq!(settings.scan_on_startup, !original_startup);
            Ok(())
        });
        assert_eq!(app.user_settings.scan_on_startup, !original_startup);
        assert_eq!(app.scan_on_startup, !original_startup);
        assert!(app.settings_state.is_none());
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn unchanged_settings_close_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        app.settings_state = Some(SettingsState::new(app.user_settings.clone()));
        app.mode = AppMode::Settings;
        app.save_settings_with(|_| panic!("unchanged settings must not write"));
        assert!(app.settings_state.is_none());
        assert_eq!(app.mode, AppMode::Normal);
    }

    /// An app with a completed scan over a real directory.
    ///
    /// The `TempDir` is returned alongside because dropping it would delete the
    /// tree out from under the app.
    fn scanned_app() -> (tempfile::TempDir, App) {
        let dir = sample_dir_on_disk();
        // The scanner stores a canonical root, and on macOS a `TempDir` lives
        // under a `/var` symlink — without this the view root never matches a
        // node and the explorer silently renders nothing.
        let root = dir.path().canonicalize().expect("canonicalize temp dir");

        // Tests must never move fixtures into the user's real Trash.
        let mut app = App::with_config(
            root.clone(),
            crate::TuiConfig {
                replace_policy: gravityfile_ops::ReplacePolicy::Permanent,
                ..crate::TuiConfig::default()
            },
        );

        let config = gravityfile_core::ScanConfig::new(&root);
        let tree = gravityfile_scan::JwalkScanner::new()
            .scan(&config, CancellationToken::new())
            .expect("scan the fixture");
        app.handle_scan_result(ScanResult::ScanComplete(Ok(tree)));
        // `ScanComplete` schedules a background scan-complete hook; the tests
        // never poll for it, and the app state is already final here.
        app.scan_rx = None;

        (dir, app)
    }

    /// The scanned app with every optional piece of state populated, so each
    /// modal and overlay has real content to draw.
    fn populated_app() -> (tempfile::TempDir, App) {
        let (dir, mut app) = scanned_app();
        let root = app.path.clone();
        let tree = Arc::clone(app.tree.as_ref().expect("scan produced a tree"));

        let duplicates = gravityfile_analyze::DuplicateFinder::with_config(
            gravityfile_analyze::DuplicateConfig::builder()
                .min_size(1u64)
                .build()
                .expect("duplicate config uses only defaulted fields"),
        )
        .unwrap()
        .find_duplicates(&tree, CancellationToken::new());
        assert!(
            !duplicates.groups.is_empty(),
            "the fixture must contain a duplicate pair"
        );
        app.duplicates = Some(Arc::new(duplicates));
        app.age_report = Some(
            gravityfile_analyze::AgeAnalyzer::with_config(Default::default())
                .unwrap()
                .analyze(&tree, CancellationToken::new()),
        );

        app.warnings = vec![
            gravityfile_core::ScanWarning::new(
                root.join("denied"),
                gravityfile_core::WarningKind::PermissionDenied,
                "permission denied",
            ),
            gravityfile_core::ScanWarning::new(
                root.join("dangling"),
                gravityfile_core::WarningKind::BrokenSymlink,
                "broken symlink",
            ),
        ];

        app.marked.insert(root.join("empty"));
        app.clipboard.yank([root.join("copy-a.bin")], root.clone());

        app.error = Some("something went wrong".to_string());
        app.operation_message = Some((true, "Copied 3 items, 1 skipped".to_string()));
        app.deletion_message = Some((false, "Deleted 1 item, 1 failed".to_string()));

        app.operation_progress = Some(gravityfile_ops::OperationProgress::new(
            gravityfile_ops::OperationType::Copy,
            10,
            4096,
        ));
        app.deletion_progress = Some(state::DeletionProgress::new(3));
        app.scan_progress = Some(gravityfile_scan::ScanProgress::new());

        app.input_state = Some(InputState::with_initial("some-name.txt"));
        app.settings_state = Some(SettingsState::new(app.user_settings.clone()));
        app.bookmark_list_state = Some(BookmarkListState::new(&app.user_settings.bookmarks));
        app.bulk_rename_state = Some(state::BulkRenameState::new(
            vec![root.join("copy-a.bin"), root.join("copy-b.bin")],
            vec!["one.bin".to_string(), "two.bin".to_string()],
        ));
        app.visual_state = Some(state::VisualState::new(1));
        app.tab_manager.new_tab(root.join("src"));

        (dir, app)
    }

    fn render(app: &App, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        Widget::render(app, area, &mut buf);
        buf
    }

    fn text(buf: &Buffer) -> String {
        buf.content().iter().map(|cell| cell.symbol()).collect()
    }

    /// Index of a named row in the tree view's flattened list.
    fn tree_row(app: &App, name: &str) -> usize {
        let (node, path) = app.get_view_root_node().expect("a view root node");
        TreeView::new(node, &path, &app.theme, &app.marked, &app.clipboard)
            .flatten(&app.tree_state)
            .iter()
            .position(|item| item.node.name == name)
            .unwrap_or_else(|| panic!("{name} is not visible"))
    }

    #[tokio::test]
    async fn completion_errors_override_zero_failure_count_in_ui_summary() {
        let (_dir, mut app) = scanned_app();
        let path = app.path.join("copy-a.bin");
        app.handle_scan_result(ScanResult::OperationComplete {
            operation_type: gravityfile_ops::OperationType::Copy,
            cancelled: false,
            succeeded: 1,
            failed: 0,
            skipped: 0,
            bytes_processed: 0,
            errors: vec![gravityfile_ops::OperationError::new(
                path.clone(),
                "late diagnostic",
            )],
            undo_info: Some(gravityfile_ops::UndoableOperation::FilesCopied {
                created: vec![path],
            }),
            undo_remaining: None,
        });
        let (success, message) = app.operation_message.as_ref().unwrap();
        assert!(!success);
        assert!(message.contains("1 errors"));
        assert_eq!(app.operation_warnings.len(), 1);
        assert_eq!(app.undo_log.len(), 1, "retain undo for the completed item");
        assert_eq!(app.view, View::Errors);
        app.scan_token.cancel();
        app.scan_rx = None;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn archive_default_extension_preserves_native_parent_path() {
        use std::os::unix::ffi::OsStringExt;
        for name in ["bundle", "bundle.zip"] {
            let temporary = tempfile::tempdir().unwrap();
            let parent = temporary
                .path()
                .join(std::ffi::OsString::from_vec(b"native-\xff".to_vec()));
            let decoy = temporary.path().join("native-\u{fffd}");
            std::fs::create_dir(&parent).unwrap();
            std::fs::create_dir(&decoy).unwrap();
            let source = parent.join("source.txt");
            std::fs::write(&source, b"native directory contents").unwrap();
            let mut app = App::with_config(parent.clone(), crate::TuiConfig::default());
            app.marked.insert(source);
            app.compress_files(name);
            let completion = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                app.archive_rx.take().expect("archive task started"),
            )
            .await
            .unwrap()
            .unwrap();
            let ArchiveCompletion::Compress {
                archive_path,
                result,
                ..
            } = completion
            else {
                panic!("expected compression completion");
            };
            result.unwrap();
            assert_eq!(archive_path, parent.join("bundle.zip"));
            assert!(archive_path.is_file());
            assert!(!decoy.join("bundle.zip").exists());
            let extracted = parent.join("extracted");
            gravityfile_ops::extract_archive(&archive_path, &extracted).unwrap();
            assert_eq!(
                std::fs::read(extracted.join("source.txt")).unwrap(),
                b"native directory contents"
            );
        }
    }

    #[tokio::test]
    async fn dropping_app_cancels_archive_work_without_misreporting_publication() {
        let (_directory, mut app) = scanned_app();
        let destination = app.view_root.join("drop.zip");
        app.marked.insert(app.view_root.join("copy-a.bin"));
        app.compress_files("drop.zip");
        let cancel = app.archive_cancel.as_ref().unwrap().0.clone();
        let receiver = app.archive_rx.take().unwrap();
        drop(app);
        assert!(cancel.is_cancelled());
        let completion = tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .unwrap()
            .unwrap();
        let ArchiveCompletion::Compress { result, .. } = completion else {
            panic!("expected creation")
        };
        match result {
            Ok(()) => assert!(
                destination.is_file(),
                "publication won the cancellation race"
            ),
            Err(gravityfile_ops::ArchiveError::Cancelled) => assert!(!destination.exists()),
            other => panic!("unexpected archive result: {other:?}"),
        }
    }

    #[test]
    fn queued_archive_extraction_quit_cancels_and_waits_for_completion() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel();
        let blocker = executor.spawn_blocking(move || {
            ready.send(()).unwrap();
            let _ = wait.recv();
        });
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        executor.block_on(async {
            let temporary = tempfile::tempdir().unwrap();
            let source = temporary.path().join("source");
            std::fs::write(&source, b"contents").unwrap();
            let root = temporary.path().join("view");
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let archive = root.join("archive.zip");
            gravityfile_ops::create_archive(
                &[source],
                &archive,
                gravityfile_ops::ArchiveFormat::Zip,
            )
            .unwrap();
            let mut app = App::with_config(root.clone(), crate::TuiConfig::default());
            let tree = gravityfile_scan::JwalkScanner::new()
                .scan(
                    &gravityfile_core::ScanConfig::new(&root),
                    CancellationToken::new(),
                )
                .unwrap();
            app.handle_scan_result(ScanResult::ScanComplete(Ok(tree)));
            app.scan_rx = None;
            app.layout_mode = LayoutMode::Miller;
            app.select_child_by_name(&archive);
            assert_eq!(app.get_selected_path(), Some(archive.clone()));
            app.marked.insert(archive.clone());
            app.extract_archive(Some("extracted".into()));
            app.handle_action(KeyAction::Quit);
            assert_ne!(app.mode, AppMode::Quit);
            assert!(app.archive_cancel.as_ref().unwrap().0.is_cancelled());
            assert!(app.archive_rx.is_some());
            drop(release);
            blocker.await.unwrap();
            let completion = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                app.archive_rx.as_mut().unwrap(),
            )
            .await
            .unwrap()
            .ok();
            app.finish_archive_task(completion);
            assert!(app.archive_cancel.is_none());
            assert!(app.archive_rx.is_none());
            assert!(!root.join("extracted").exists());
            assert!(app.marked.contains(&archive));
            assert!(
                app.error
                    .as_deref()
                    .unwrap()
                    .contains("extraction cancelled before publication")
            );
        });
    }

    #[test]
    fn queued_archive_creation_can_be_cancelled_without_publication() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel();
        let blocker = executor.spawn_blocking(move || {
            ready.send(()).unwrap();
            let _ = wait.recv();
        });
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        executor.block_on(async {
            let (_directory, mut app) = scanned_app();
            let source = app.view_root.join("copy-a.bin");
            app.marked.insert(source.clone());
            app.compress_files("cancelled.zip");
            app.handle_action(KeyAction::Cancel);
            assert!(app.archive_cancel.as_ref().unwrap().0.is_cancelled());
            assert!(
                app.archive_rx.is_some(),
                "retain admission until the worker completes"
            );
            drop(release);
            blocker.await.unwrap();
            let completion = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                app.archive_rx.as_mut().unwrap(),
            )
            .await
            .unwrap()
            .ok();
            app.finish_archive_task(completion);
            assert!(app.archive_cancel.is_none());
            assert!(app.archive_rx.is_none());
            assert!(!app.view_root.join("cancelled.zip").exists());
            assert!(app.marked.contains(&source));
            assert!(
                app.error
                    .as_deref()
                    .unwrap()
                    .contains("cancelled before publication")
            );
        });
    }

    #[tokio::test]
    async fn lost_archive_worker_refreshes_uncertain_output_and_releases_admission() {
        for active_creation in [false, true] {
            let (_directory, mut app) = scanned_app();
            let root = app.view_root.clone();
            let source = root.join("copy-a.bin");
            app.marked.insert(source.clone());
            app.operation_message = Some((true, "Creating archive…".into()));
            // The worker may have changed the filesystem before losing its reply.
            std::fs::write(
                root.join("uncertain-output"),
                b"completed before reply loss",
            )
            .unwrap();
            if active_creation {
                app.execute_create_file(root.clone(), "created".into());
            }
            let token = app.scan_token.clone();
            let message = app.operation_message.clone();
            let (sender, receiver) = oneshot::channel();
            app.archive_rx = Some(receiver);
            drop(sender);
            let completion = app.archive_rx.as_mut().unwrap().await.ok();
            app.finish_archive_task(completion);
            assert!(app.archive_rx.is_none());
            assert!(
                app.scan_rx.is_some(),
                "unknown outcome must refresh the filesystem view"
            );
            assert!(
                app.error
                    .as_deref()
                    .unwrap()
                    .contains("inspect the destination before retrying")
            );
            if active_creation {
                assert!(!token.is_cancelled());
                assert_eq!(app.operation_message, message);
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while let Some(receiver) = app.scan_rx.as_mut() {
                    let Some(result) = receiver.recv().await else {
                        break;
                    };
                    app.handle_scan_result(result);
                }
            })
            .await
            .unwrap();
            assert!(
                app.tree
                    .as_ref()
                    .unwrap()
                    .root
                    .children
                    .iter()
                    .any(|node| node.name == "uncertain-output")
            );
            assert!(app.marked.contains(&source));
            assert!(!app.pending_archive_refresh);
            assert!(
                !app.operation_message
                    .as_ref()
                    .is_some_and(|(_, message)| message == "Creating archive…")
            );
        }
    }

    #[tokio::test]
    async fn incomplete_extraction_rollback_refreshes_without_clearing_selection() {
        for active_creation in [false, true] {
            let (_directory, mut app) = scanned_app();
            let root = app.view_root.clone();
            let source = root.join("copy-a.bin");
            let archive = root.join("result.zip");
            std::fs::write(&archive, b"leftover extraction output").unwrap();
            app.marked.insert(source.clone());
            if active_creation {
                app.execute_create_file(root.clone(), "created".into());
            }
            let token = app.scan_token.clone();
            app.handle_archive_completion(ArchiveCompletion::Extract {
                archive_path: root.join("input.zip"),
                destination: root.clone(),
                result: Err(gravityfile_ops::ArchiveError::ExtractionRollback {
                    destination: root.clone(),
                    source: Box::new(
                        std::io::Error::other("private/path publication failure").into(),
                    ),
                    left_behind: vec![archive.clone()],
                }),
            });
            let message = app.error.as_deref().unwrap();
            assert!(
                message.starts_with("Archive extraction failed; 1 extracted entries"),
                "{message}"
            );
            assert!(!message.contains("private/path"));
            assert!(app.marked.contains(&source));
            assert!(app.scan_rx.is_some());
            if active_creation {
                assert!(!token.is_cancelled());
                assert!(app.pending_archive_refresh);
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                // Unlike the operation-only helper, allow its follow-up
                // scan to finish so the assertion observes the actual tree.
                while let Some(receiver) = app.scan_rx.as_mut() {
                    let Some(result) = receiver.recv().await else {
                        break;
                    };
                    app.handle_scan_result(result);
                }
            })
            .await
            .unwrap();
            assert!(
                app.tree
                    .as_ref()
                    .unwrap()
                    .root
                    .children
                    .iter()
                    .any(|node| node.name == "result.zip")
            );
            assert!(app.marked.contains(&source));
            assert!(!app.pending_archive_refresh);
            assert!(
                app.error
                    .as_deref()
                    .unwrap()
                    .contains("could not be withdrawn")
            );
        }
    }

    #[tokio::test]
    async fn archive_completion_defers_refresh_until_active_creation_finishes() {
        for compress in [false, true] {
            let (_directory, mut app) = scanned_app();
            let root = app.view_root.clone();
            let archived_source = root.join("copy-a.bin");
            let unrelated_mark = root.join("copy-b.bin");
            app.marked.insert(archived_source.clone());
            app.marked.insert(unrelated_mark.clone());
            app.execute_create_file(root.clone(), "created".into());
            let token = app.scan_token.clone();
            let message = app.operation_message.clone();
            let completion = if compress {
                ArchiveCompletion::Compress {
                    archive_path: root.join("result.zip"),
                    sources: vec![archived_source.clone(), root.join("unmarked-source")],
                    result: Ok(()),
                }
            } else {
                ArchiveCompletion::Extract {
                    archive_path: root.join("source.zip"),
                    destination: root.clone(),
                    result: Ok(vec![root.join("extracted")]),
                }
            };
            app.handle_archive_completion(completion);
            assert_eq!(app.marked.contains(&archived_source), !compress);
            assert!(app.marked.contains(&unrelated_mark));
            assert_eq!(app.mode, AppMode::UpdatingEntry);
            assert!(app.scan_rx.is_some());
            assert!(!token.is_cancelled());
            assert_eq!(app.operation_message, message);
            assert!(app.pending_archive_refresh);
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                pump_until_complete(&mut app),
            )
            .await
            .unwrap();
            assert!(root.join("created").is_file());
            assert_eq!(app.undo_log.len(), 1);
            assert!(!app.pending_archive_refresh);
        }
    }

    #[tokio::test]
    async fn oversized_selection_totals_remain_renderable_and_saturate() {
        let (_directory, mut app) = scanned_app();
        Arc::make_mut(app.tree.as_mut().unwrap()).root.size = u64::MAX;
        app.marked.insert(app.path.clone());
        app.marked.insert(app.path.join("copy-a.bin"));
        let expected = crate::ui::format_size(u64::MAX);
        for mode in [AppMode::Normal, AppMode::ConfirmDelete] {
            app.mode = mode;
            let rendered = text(&render(&app, Rect::new(0, 0, 120, 40)));
            assert!(rendered.contains(&expected));
        }
        let (_directory, mut app) = populated_app();
        let root = app.path.clone();
        let report = app.age_report.as_mut().unwrap();
        report.stale_directories = [root.clone(), root.join("src")]
            .into_iter()
            .map(|path| gravityfile_analyze::StaleDirectory {
                path,
                size: u64::MAX,
                newest_file_age: std::time::Duration::from_secs(86400),
                file_count: 1,
            })
            .collect();
        report.stale_directory_status = gravityfile_analyze::StaleDirectoryStatus::Evaluated;
        assert_eq!(report.total_stale_size(), u64::MAX);
        app.view = View::Age;
        app.mode = AppMode::Normal;
        let rendered = text(&render(&app, Rect::new(0, 0, 120, 40)));
        assert!(rendered.contains(&expected));
    }

    #[tokio::test]
    async fn commands_use_exact_internal_spacing_for_navigation_and_creation() {
        let (_directory, mut app) = scanned_app();
        let root = app.path.clone();
        let destination = root.join("two  spaces");
        std::fs::create_dir(&destination).unwrap();
        std::fs::create_dir(root.join("two spaces")).unwrap();
        app.start_scan();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let result = app.scan_rx.as_mut().unwrap().recv().await.unwrap();
                let complete = matches!(result, ScanResult::ScanComplete(_));
                app.handle_scan_result(result);
                if complete {
                    break;
                }
            }
        })
        .await
        .unwrap();
        app.execute_command("cd two  spaces");
        assert_eq!(app.view_root, destination);
        app.execute_command("touch note  日本.txt");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pump_until_complete(&mut app),
        )
        .await
        .unwrap();
        assert!(destination.join("note  日本.txt").is_file());
        assert!(!destination.join("note 日本.txt").exists());
        assert_eq!(app.undo_log.len(), 1);
    }

    #[tokio::test]
    async fn invalid_palette_commands_preserve_sort_bookmarks_and_navigation() {
        let (_directory, mut app) = scanned_app();
        let root = app.view_root.clone();
        app.user_settings.bookmarks.set('a', root.clone());
        let sort = app.sort_mode;
        for command in ["sort typo", "delmark ab", "go a extra", "unknown"] {
            app.error = None;
            app.execute_command(command);
            assert!(app.error.is_some());
            assert_eq!(app.sort_mode, sort);
            assert_eq!(app.view_root, root);
            assert_eq!(app.user_settings.bookmarks.get('a'), Some(&root));
            assert!(app.scan_rx.is_none());
        }
    }

    #[tokio::test]
    async fn pending_operations_render_before_the_first_progress_update() {
        let (_dir, mut app) = scanned_app();
        for mode in [
            AppMode::Copying,
            AppMode::Moving,
            AppMode::Undoing,
            AppMode::UpdatingEntry,
        ] {
            app.mode = mode;
            app.operation_progress = None;
            for message in [None, Some((false, "Waiting for filesystem work".into()))] {
                app.operation_message = message;
                for &(width, height) in SIZES {
                    let _ = render(&app, Rect::new(3, 2, width, height));
                }
                let rendered = text(&render(&app, Rect::new(0, 0, 80, 24)));
                assert!(rendered.contains("Operation in progress"));
                let expected = if app.operation_message.is_some() {
                    "Waiting for filesystem work"
                } else {
                    "Waiting for worker progress..."
                };
                assert!(rendered.contains(expected));
                app.operation_progress = Some(OperationProgress::new(
                    gravityfile_ops::OperationType::Rename,
                    1,
                    0,
                ));
                let rendered = text(&render(&app, Rect::new(0, 0, 80, 24)));
                assert!(rendered.contains("Renaming..."));
                assert!(!rendered.contains("Operation in progress"));
                app.operation_progress = None;
            }
        }
    }

    #[tokio::test]
    async fn renders_every_view_and_mode_at_every_terminal_size() {
        // A terminal can be resized to anything, including one column wide, and
        // a panic in a widget aborts the whole app. This sweep is the backstop
        // for the right-edge and viewport arithmetic across all the widgets.
        let (_dir, mut app) = populated_app();

        for &layout in &[LayoutMode::Tree, LayoutMode::Miller] {
            for &view in VIEWS {
                for &mode in MODES {
                    for &(width, height) in SIZES {
                        app.layout_mode = layout;
                        app.view = view;
                        app.mode = mode;
                        let _ = render(&app, Rect::new(0, 0, width, height));
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn renders_at_every_size_before_a_scan_has_produced_a_tree() {
        // Startup and scan failure both leave the app with no tree at all.
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::with_config(dir.path().to_path_buf(), crate::TuiConfig::default());
        app.tree = None;
        app.error = Some("scan failed".to_string());

        for &layout in &[LayoutMode::Tree, LayoutMode::Miller] {
            for &view in VIEWS {
                for &(width, height) in SIZES {
                    app.layout_mode = layout;
                    app.view = view;
                    let _ = render(&app, Rect::new(0, 0, width, height));
                }
            }
        }
    }

    #[tokio::test]
    async fn the_explorer_renders_the_scanned_entries() {
        // Guards the sweep above: a render that draws nothing would also never
        // panic, so at least one size has to prove real content reaches the
        // buffer.
        let (_dir, app) = scanned_app();
        let rendered = text(&render(&app, Rect::new(0, 0, 120, 30)));
        assert!(rendered.contains("src"), "explorer is blank: {rendered}");
    }

    #[tokio::test]
    async fn both_layouts_render_the_selected_directory() {
        let (_dir, mut app) = scanned_app();

        app.layout_mode = LayoutMode::Tree;
        assert!(text(&render(&app, Rect::new(0, 0, 120, 30))).contains("src"));

        app.layout_mode = LayoutMode::Miller;
        let rendered = text(&render(&app, Rect::new(0, 0, 120, 30)));
        assert!(rendered.contains("Current"), "miller columns missing");
        assert!(rendered.contains("src"), "miller is blank: {rendered}");
    }

    #[tokio::test]
    async fn tree_navigation_is_clamped_to_the_visible_item_count() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        let last = app.cached_tree_len - 1;
        assert!(last > 0, "the fixture should expand to several rows");

        app.jump_to_bottom();
        assert_eq!(app.tree_state.selected, last);
        app.move_down();
        app.page_down();
        assert_eq!(app.tree_state.selected, last);

        app.jump_to_top();
        assert_eq!(app.tree_state.selected, 0);
        app.move_up();
        app.page_up();
        assert_eq!(app.tree_state.selected, 0);
    }

    #[tokio::test]
    async fn expanding_a_directory_grows_the_visible_item_count() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        let collapsed_len = app.cached_tree_len;

        app.tree_state.selected = tree_row(&app, "src");
        app.expand_selected();
        assert!(
            app.cached_tree_len > collapsed_len,
            "expanding src revealed nothing"
        );

        app.collapse_selected();
        assert_eq!(app.cached_tree_len, collapsed_len);
    }

    #[tokio::test]
    async fn drilling_into_a_directory_and_back_restores_the_previous_root() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        let original_root = app.view_root.clone();

        let src_index = tree_row(&app, "src");
        app.tree_state.selected = src_index;
        app.drill_into_selected();
        assert_eq!(app.view_root, original_root.join("src"));

        app.navigate_back();
        assert_eq!(app.view_root, original_root);
        assert_eq!(
            app.tree_state.selected, src_index,
            "returning restores the row the user drilled from"
        );
    }

    #[tokio::test]
    async fn drilling_in_miller_mode_and_back_restores_the_previous_root() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Miller;
        let original_root = app.view_root.clone();

        let (node, _) = app.get_view_root_node().unwrap();
        let src_index = node
            .children
            .iter()
            .position(|child| child.name.as_str() == "src")
            .expect("src is a child");
        app.miller_state.selected = src_index;

        app.drill_into_miller_selected();
        assert_eq!(app.view_root, original_root.join("src"));

        app.navigate_back();
        assert_eq!(app.view_root, original_root);
        assert_eq!(app.miller_state.selected, src_index);
    }

    #[tokio::test]
    async fn drilling_into_a_file_does_nothing() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        let root = app.view_root.clone();

        app.tree_state.selected = tree_row(&app, "empty");
        app.drill_into_selected();
        assert_eq!(app.view_root, root, "a file is not a directory to enter");
        assert!(app.view_history.is_empty(), "no history entry was recorded");
    }

    #[tokio::test]
    async fn navigating_back_from_the_scan_root_walks_up_the_filesystem() {
        let (_dir, mut app) = scanned_app();
        let root = app.view_root.clone();
        let parent = root
            .parent()
            .expect("the temp dir has a parent")
            .to_path_buf();

        app.navigate_back();
        assert_eq!(app.view_root, parent);
    }

    #[tokio::test]
    async fn the_treemap_selection_is_bounded_by_the_rectangle_count() {
        let (_dir, mut app) = scanned_app();
        app.view = View::Treemap;
        app.update_cached_treemap_len();
        let last = app.cached_treemap_len - 1;

        app.jump_to_bottom();
        assert_eq!(app.treemap_state.selected, last);
        app.move_down();
        app.page_down();
        assert_eq!(app.treemap_state.selected, last);

        app.jump_to_top();
        assert_eq!(app.treemap_state.selected, 0);
        app.move_up();
        app.page_up();
        assert_eq!(app.treemap_state.selected, 0);
    }

    #[tokio::test]
    async fn deletion_failures_reach_the_errors_view_and_success_clears_old_details() {
        let (_directory, mut app) = scanned_app();
        let missing = app.path.join("missing-delete-target");
        let removed = app.path.join("temporary-delete-target");
        std::fs::write(&removed, b"temporary").unwrap();
        app.operation_message = Some((true, "Old copy summary".into()));
        let mut rx = deletion::start_deletion_with_mode(
            vec![(missing.clone(), 0), (removed.clone(), 9)],
            gravityfile_ops::DeleteMode::Permanent,
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let result = rx.recv().await.expect("deletion completion");
                let complete = matches!(result, ScanResult::DeletionComplete { .. });
                app.handle_scan_result(result);
                if complete {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(!removed.exists());
        assert_eq!(app.view, View::Errors);
        assert!(app.operation_message.is_none());
        assert!(!app.deletion_message.as_ref().unwrap().0);
        assert!(
            app.deletion_message
                .as_ref()
                .unwrap()
                .1
                .contains("Permanently deleted")
        );
        assert_eq!(app.operation_warnings.len(), 1);
        assert_eq!(app.operation_warnings[0].path, missing);
        assert!(
            app.operation_warnings[0]
                .message
                .contains("Cannot access path")
        );
        let rendered = text(&render(&app, Rect::new(0, 0, 120, 40)));
        assert!(rendered.contains("missing-delete-target"), "{rendered}");
        assert!(rendered.contains("Cannot access path"), "{rendered}");
        assert!(!rendered.contains("Old copy summary"));
        app.handle_scan_result(ScanResult::DeletionComplete {
            mode: gravityfile_ops::DeleteMode::Trash,
            deleted: vec![removed.clone()],
            failed: 0,
            bytes_freed: 9,
            errors: Vec::new(),
        });
        assert!(app.operation_warnings.is_empty());
        assert!(app.deletion_message.as_ref().unwrap().0);
    }

    #[tokio::test]
    async fn undo_failure_details_survive_refresh_and_render_in_errors_view() {
        let (_directory, mut app) = scanned_app();
        let path = app.path.join("work-in-progress.txt");
        std::fs::write(&path, b"user content").unwrap();
        app.undo_log.record_create_file(path.clone());
        app.execute_undo();
        let mut rx = app.scan_rx.take().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let result = rx.recv().await.expect("undo completion");
                let complete = matches!(result, ScanResult::OperationComplete { .. });
                app.handle_scan_result(result);
                if complete {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(app.view, View::Errors);
        assert_eq!(app.operation_warnings.len(), 1);
        assert_eq!(app.operation_warnings[0].path, path);
        assert!(
            app.operation_warnings[0]
                .message
                .contains("now contains data")
        );
        assert!(
            app.warnings.is_empty(),
            "automatic refresh clears scan warnings only"
        );
        let rendered = text(&render(&app, Rect::new(0, 0, 120, 40)));
        assert!(rendered.contains("work-in-progress.txt"), "{rendered}");
        assert!(rendered.contains("now contains data"), "{rendered}");
        app.warnings.push(gravityfile_core::ScanWarning::new(
            app.path.join("scan-warning"),
            gravityfile_core::WarningKind::ReadError,
            "scan issue",
        ));
        app.jump_to_bottom();
        assert_eq!(app.selected_warning, 1);
        assert_eq!(app.displayed_warning(1).unwrap().message, "scan issue");
        app.jump_to_top();
        assert_eq!(app.displayed_warning(0).unwrap().path, path);
        assert_eq!(std::fs::read(path).unwrap(), b"user content");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rename_does_not_prefill_lossy_filename_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let (_directory, mut app) = scanned_app();
        let path = app
            .path
            .join(std::ffi::OsString::from_vec(b"name-\xff".to_vec()));
        app.operation_warnings
            .push(gravityfile_core::ScanWarning::new(
                &path,
                gravityfile_core::WarningKind::ReadError,
                "diagnostic",
            ));
        app.view = View::Errors;
        app.start_rename();
        let input = app.input_state.as_ref().unwrap();
        assert_eq!(input.context_path, Some(path));
        assert_eq!(input.buffer(), "");
        assert!(input.original().is_none());
        assert!(
            app.operation_message
                .as_ref()
                .unwrap()
                .1
                .contains("not UTF-8")
        );
        // Empty submission is rejected before any rename task is created.
        app.scan_rx = None;
        app.handle_input_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.scan_rx.is_none());
        assert!(!app.operation_message.as_ref().unwrap().0);
    }

    #[tokio::test]
    async fn rename_preserves_unicode_prefill_and_rejects_filesystem_root() {
        let (_directory, mut app) = scanned_app();
        let path = app.path.join("name-é-�");
        app.operation_warnings
            .push(gravityfile_core::ScanWarning::new(
                &path,
                gravityfile_core::WarningKind::ReadError,
                "diagnostic",
            ));
        app.view = View::Errors;
        app.start_rename();
        assert_eq!(app.input_state.as_ref().unwrap().buffer(), "name-é-�");
        app.input_state = None;
        app.mode = AppMode::Normal;
        let root = app.path.ancestors().last().unwrap().to_path_buf();
        app.operation_warnings[0].path = root;
        app.start_rename();
        assert!(app.input_state.is_none());
        assert_eq!(app.mode, AppMode::Normal);
        assert!(
            app.operation_message
                .as_ref()
                .unwrap()
                .1
                .contains("root cannot be renamed")
        );
    }

    #[tokio::test]
    async fn node_lookup_follows_components_and_rejects_outside_paths() {
        let (_dir, app) = scanned_app();
        let tree = app.tree.as_ref().unwrap();
        let deep = app.path.join("src/nested/deep.txt");
        assert_eq!(
            App::find_node_at_path(&tree.root, &deep, &app.path)
                .unwrap()
                .name,
            "deep.txt"
        );
        assert!(std::ptr::eq(
            App::find_node_at_path(&tree.root, &app.path, &app.path).unwrap(),
            &tree.root
        ));
        assert!(
            App::find_node_at_path(
                &tree.root,
                &app.path.join("src/missing/deep.txt"),
                &app.path
            )
            .is_none()
        );
        assert!(
            App::find_node_at_path(&tree.root, &app.path.join("../outside"), &app.path).is_none()
        );
        assert!(
            App::find_node_at_path(&tree.root, &app.path.with_extension("outside"), &app.path)
                .is_none()
        );
    }

    #[test]
    fn mutable_node_lookup_handles_deep_synthetic_paths() {
        use gravityfile_core::{FileNode, NodeId, Timestamps};
        let depth = 20_000u64;
        let mut node = FileNode::new_directory(
            NodeId::new(depth),
            "d",
            Timestamps::with_modified(std::time::UNIX_EPOCH),
        );
        let mut target = PathBuf::from("root");
        for id in (0..depth).rev() {
            let mut parent = FileNode::new_directory(
                NodeId::new(id),
                "d",
                Timestamps::with_modified(std::time::UNIX_EPOCH),
            );
            parent.children.push(node);
            node = parent;
            target.push("d");
        }
        let actual =
            App::find_node_mut(&mut node, &target, Path::new("root")).map(|found| found.id);
        let mut pending = std::mem::take(&mut node.children);
        while let Some(mut child) = pending.pop() {
            pending.append(&mut child.children);
        }
        assert_eq!(actual, Some(NodeId::new(depth)));
    }

    #[tokio::test]
    async fn mutable_node_lookup_uses_the_same_path_scope() {
        let (_dir, mut app) = scanned_app();
        let tree = std::sync::Arc::make_mut(app.tree.as_mut().unwrap());
        let deep = app.path.join("src/nested/deep.txt");
        let found = App::find_node_mut(&mut tree.root, &deep, &app.path).unwrap();
        found.size = 12345;
        assert_eq!(
            App::find_node_at_path(&tree.root, &deep, &app.path)
                .unwrap()
                .size,
            12345
        );
        for path in [
            app.path.join("src/missing/deep.txt"),
            app.path.join("../outside"),
            app.path.with_extension("outside"),
        ] {
            assert!(App::find_node_mut(&mut tree.root, &path, &app.path).is_none());
        }
        let root_id = tree.root.id;
        assert_eq!(
            App::find_node_mut(&mut tree.root, &app.path, &app.path)
                .unwrap()
                .id,
            root_id
        );
    }

    #[tokio::test]
    async fn file_actions_follow_the_active_view_instead_of_the_explorer_cursor() {
        for layout in [LayoutMode::Tree, LayoutMode::Miller] {
            let (_dir, mut app) = populated_app();
            app.layout_mode = layout;
            app.marked.clear();
            app.clipboard.clear();
            app.view = View::Duplicates;
            app.duplicates_state.expanded_groups.insert(0);
            app.duplicates_state.selected_in_group.insert(0, 1);
            let duplicate = app.duplicates.as_ref().unwrap().groups[0].paths[0].clone();
            assert_eq!(app.get_selected_path(), Some(duplicate.clone()));
            assert_eq!(app.get_selected_info().unwrap().path, duplicate);
            app.yank_selection();
            assert_eq!(app.clipboard.paths, vec![duplicate.clone()]);
            app.start_rename();
            assert_eq!(
                app.input_state.as_ref().unwrap().context_path,
                Some(duplicate)
            );
            app.input_state = None;
            app.mode = AppMode::Normal;
            // A collapsed group still remembers its child cursor, but no child
            // is visibly selected and single-file actions must have no target.
            app.duplicates_state.expanded_groups.clear();
            assert!(app.get_selected_path().is_none());
            assert!(app.get_selected_info().is_none());
            app.start_rename();
            assert!(app.input_state.is_none());

            let diagnosed = app.path.join("copy-b.bin");
            app.operation_warnings
                .push(gravityfile_core::ScanWarning::new(
                    &diagnosed,
                    gravityfile_core::WarningKind::ReadError,
                    "operation failed",
                ));
            app.view = View::Errors;
            app.selected_warning = 0;
            assert_eq!(app.get_selected_path(), Some(diagnosed.clone()));
            assert_eq!(app.get_selected_info().unwrap().path, diagnosed);
            app.cut_selection();
            assert_eq!(app.clipboard.paths, vec![diagnosed]);
            app.selected_warning = 1;
            assert_eq!(app.get_selected_path(), Some(app.warnings[0].path.clone()));
            assert!(
                app.get_selected_info().is_none(),
                "unknown diagnostics must not borrow another file's details"
            );

            let stale = app.path.join("src");
            app.age_report.as_mut().unwrap().stale_directories =
                vec![gravityfile_analyze::StaleDirectory {
                    path: stale.clone(),
                    size: 4096,
                    newest_file_age: std::time::Duration::from_secs(86400),
                    file_count: 2,
                }];
            app.view = View::Age;
            app.selected_stale_dir = 0;
            assert_eq!(app.get_selected_path(), Some(stale.clone()));
            assert_eq!(app.get_selected_info().unwrap().path, stale);
            assert!(!app.is_selected_file());
            app.selected_stale_dir = 1;
            assert!(app.get_selected_path().is_none());
        }
    }

    #[tokio::test]
    async fn the_errors_view_selection_is_bounded_by_the_warning_count() {
        let (_dir, mut app) = populated_app();
        app.view = View::Errors;
        let last = app.warnings.len() - 1;

        app.jump_to_bottom();
        assert_eq!(app.selected_warning, last);
        app.move_down();
        app.page_down();
        assert_eq!(app.selected_warning, last);

        app.jump_to_top();
        app.move_up();
        assert_eq!(app.selected_warning, 0);
    }

    #[tokio::test]
    async fn navigation_on_empty_views_does_not_underflow() {
        // Before analysis runs, the duplicates and age views have no rows at
        // all, and a scan with no problems leaves the errors view empty.
        let (_dir, mut app) = scanned_app();
        app.duplicates = None;
        app.age_report = None;
        app.warnings.clear();

        for view in VIEWS {
            app.view = *view;
            app.move_up();
            app.move_down();
            app.page_up();
            app.page_down();
            app.jump_to_top();
            app.jump_to_bottom();
        }
    }

    #[tokio::test]
    async fn switching_layout_keeps_the_same_item_selected() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        app.tree_state.selected = tree_row(&app, "src");
        let selected_before = app.get_selected_path();
        assert!(selected_before.is_some());

        app.sync_layout_selection();
        assert_eq!(app.layout_mode, LayoutMode::Miller);
        assert_eq!(app.get_selected_path(), selected_before);

        app.sync_layout_selection();
        assert_eq!(app.layout_mode, LayoutMode::Tree);
        assert_eq!(app.get_selected_path(), selected_before);
    }

    #[tokio::test]
    async fn view_cycling_wraps_in_both_directions() {
        let (_dir, mut app) = scanned_app();
        let start = app.view;

        let mut seen = vec![start];
        for _ in 1..VIEWS.len() {
            app.handle_action(KeyAction::NextView);
            seen.push(app.view);
        }
        seen.sort_unstable_by_key(|view| *view as usize);
        seen.dedup_by_key(|view| *view as usize);
        assert_eq!(seen.len(), VIEWS.len(), "cycling must visit every view");

        app.handle_action(KeyAction::NextView);
        assert_eq!(app.view, start, "NextView must wrap");

        for _ in 0..VIEWS.len() {
            app.handle_action(KeyAction::PrevView);
        }
        assert_eq!(app.view, start, "PrevView must wrap");
    }

    #[tokio::test]
    async fn escape_clears_the_clipboard_before_the_marks_and_never_quits() {
        let (_dir, mut app) = scanned_app();
        let root = app.view_root.clone();
        app.marked.insert(root.join("empty"));
        app.clipboard.yank([root.join("copy-a.bin")], root.clone());

        app.handle_action(KeyAction::Cancel);
        assert!(app.clipboard.is_empty(), "first Esc clears the clipboard");
        assert!(!app.marked.is_empty(), "marks survive the first Esc");

        app.handle_action(KeyAction::Cancel);
        assert!(app.marked.is_empty(), "second Esc clears the marks");

        app.handle_action(KeyAction::Cancel);
        assert_eq!(app.mode, AppMode::Normal, "Esc must never quit");
    }

    #[tokio::test]
    async fn delete_requires_confirmation_and_cancelling_drops_the_auto_mark() {
        let (_dir, mut app) = scanned_app();
        assert!(app.marked.is_empty());
        app.tree_state.selected = tree_row(&app, "copy-a.bin");

        app.handle_action(KeyAction::Delete);
        assert_eq!(app.mode, AppMode::ConfirmDelete);
        assert!(!app.marked.is_empty(), "delete auto-marks the selection");

        app.handle_action(KeyAction::Cancel);
        assert_eq!(app.mode, AppMode::Normal);
        assert!(
            app.marked.is_empty(),
            "cancelling must not leave the auto-mark behind"
        );
    }

    #[tokio::test]
    async fn help_and_settings_swallow_navigation_keys() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        app.mode = AppMode::Help;

        app.handle_action(KeyAction::MoveDown);
        assert_eq!(app.tree_state.selected, 0, "help must not move the cursor");

        app.handle_action(KeyAction::ToggleHelp);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[tokio::test]
    async fn a_completed_operation_reports_skips_separately_from_failures() {
        let (_dir, mut app) = scanned_app();

        app.handle_scan_result(ScanResult::OperationComplete {
            cancelled: false,
            operation_type: gravityfile_ops::OperationType::Copy,
            succeeded: 3,
            failed: 0,
            skipped: 2,
            bytes_processed: 1024,
            errors: Vec::new(),
            undo_info: None,
            undo_remaining: None,
        });
        let (success, message) = app.operation_message.clone().expect("a message is set");
        assert!(success, "skipping is a user decision, not a failure");
        assert!(message.contains("2 skipped"), "{message}");
        assert!(!message.contains("failed"), "{message}");

        app.handle_scan_result(ScanResult::OperationComplete {
            cancelled: false,
            operation_type: gravityfile_ops::OperationType::Copy,
            succeeded: 1,
            failed: 1,
            skipped: 1,
            bytes_processed: 1024,
            errors: Vec::new(),
            undo_info: None,
            undo_remaining: None,
        });
        let (success, message) = app.operation_message.clone().expect("a message is set");
        assert!(!success, "a failure marks the operation unsuccessful");
        assert!(
            message.contains("1 skipped") && message.contains("1 failed"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn duplicate_marking_preserves_single_file_and_header_semantics() {
        let (_directory, mut app) = populated_app();
        app.marked.clear();
        app.view = View::Duplicates;
        let paths = app.duplicates.as_ref().unwrap().groups[0].paths.clone();
        assert!(paths.len() >= 2);
        app.duplicates_state.selected_group = 0;
        app.duplicates_state.expanded_groups.insert(0);
        app.duplicates_state
            .selected_in_group
            .insert(0, paths.len());
        app.toggle_mark();
        assert_eq!(app.marked, HashSet::from([paths.last().unwrap().clone()]));
        app.toggle_mark();
        assert!(app.marked.is_empty());
        app.duplicates_state.selected_in_group.insert(0, 0);
        app.toggle_mark();
        // A header marks every copy but the first, like `d` on a header.
        assert_eq!(app.marked, paths.iter().skip(1).cloned().collect());
        app.toggle_mark();
        assert!(app.marked.is_empty());
        app.marked.insert(paths[0].clone());
        app.toggle_mark();
        assert!(app.marked.is_empty());
        app.duplicates_state.selected_in_group.insert(0, usize::MAX);
        app.toggle_mark();
        assert!(app.marked.is_empty());
        app.duplicates_state.selected_group = usize::MAX;
        app.toggle_mark();
        assert!(app.marked.is_empty());
    }

    /// Manual release-mode measurement; synthetic paths do not touch disk.
    #[tokio::test]
    #[ignore = "manual million-path render benchmark; run with --release --ignored --nocapture"]
    async fn duplicate_render_benchmark() {
        use gravityfile_analyze::{AnalysisStatus, DuplicateGroup, DuplicateReport};
        let mut measurements = Vec::new();
        for group_count in [1usize, 1_000] {
            let directory = tempfile::tempdir().unwrap();
            let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
            let paths_per_group = 1_000_000 / group_count;
            let groups = (0..group_count)
                .map(|group| DuplicateGroup {
                    hash: gravityfile_core::ContentHash::new([0; 32]),
                    size: 256,
                    paths: (0..paths_per_group)
                        .map(|file| app.path.join(format!("group-{group:04}/file-{file:09}")))
                        .collect(),
                    wasted_bytes: 256 * (paths_per_group as u64 - 1),
                })
                .collect();
            app.duplicates = Some(Arc::new(DuplicateReport {
                status: AnalysisStatus::default(),
                groups,
                total_duplicate_size: 256_000_000,
                total_wasted_space: 256 * (1_000_000 - group_count as u64),
                files_analyzed: 1_000_000,
                files_with_duplicates: 1_000_000,
                group_count,
                groups_omitted: 0,
            }));
            let start = std::time::Instant::now();
            let report = app.duplicates.as_ref().unwrap();
            let counts =
                duplicate_filter::prepare_counts(report, &app.path, &CancellationToken::new())
                    .unwrap();
            assert!(counts.iter().all(|(_, sorted)| *sorted));
            let preparation_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            app.duplicate_filter.seed(report, app.path.clone(), counts);
            let installation_ms = start.elapsed().as_secs_f64() * 1000.0;
            app.view = View::Duplicates;
            app.duplicates_state.expanded_groups.extend(0..group_count);
            app.duplicates_state.selected_group = group_count - 1;
            app.duplicates_state
                .selected_in_group
                .insert(group_count - 1, paths_per_group);
            let area = Rect::new(0, 0, 120, 40);
            let start = std::time::Instant::now();
            std::hint::black_box(render(&app, area));
            let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
            let mut samples = Vec::new();
            for _ in 0..31 {
                let start = std::time::Instant::now();
                std::hint::black_box(render(&app, area));
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            let mut sorted = samples.clone();
            sorted.sort_by(f64::total_cmp);
            let mut scope_samples = Vec::new();
            for iteration in 0..31 {
                let scope = app.path.join(if iteration % 2 == 0 {
                    "group-0000"
                } else {
                    "missing"
                });
                let start = std::time::Instant::now();
                let filtered = app.duplicate_filter.get(report, &scope, &app.path);
                scope_samples.push(start.elapsed().as_secs_f64() * 1000.0);
                let count: usize = filtered.groups.iter().map(|(_, count)| count).sum();
                assert_eq!(
                    count,
                    if iteration % 2 == 0 {
                        paths_per_group
                    } else {
                        0
                    }
                );
            }
            let mut sorted_scope = scope_samples.clone();
            sorted_scope.sort_by(f64::total_cmp);
            let expected_mark = app.duplicates.as_ref().unwrap().groups[group_count - 1]
                .paths
                .last()
                .unwrap()
                .clone();
            // Restore and warm the displayed root after the scope-refresh series.
            drop(app.duplicate_filter.get(
                app.duplicates.as_ref().unwrap(),
                &app.view_root,
                &app.path,
            ));
            let mut mark_samples = Vec::new();
            for iteration in 0..31 {
                let start = std::time::Instant::now();
                app.toggle_mark();
                mark_samples.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(app.marked.contains(&expected_mark), iteration % 2 == 0);
                assert_eq!(app.marked.len(), usize::from(iteration % 2 == 0));
            }
            app.marked.clear();
            let mut sorted_mark = mark_samples.clone();
            sorted_mark.sort_by(f64::total_cmp);
            measurements.push(serde_json::json!({
                "groups": group_count, "paths": 1_000_000,
                "all_groups_expanded": true, "selection": "last file",
                "preparation_ms": preparation_ms, "installation_ms": installation_ms,
                "first_frame_ms": cold_ms, "cached_frame_ms": samples,
                "cached_median_ms": sorted[15], "cached_p95_ms": sorted[29],
                "scope_refresh_ms": scope_samples,
                "scope_refresh_median_ms": sorted_scope[15],
                "scope_refresh_p95_ms": sorted_scope[29],
                "single_mark_ms": mark_samples,
                "single_mark_median_ms": sorted_mark[15],
                "single_mark_p95_ms": sorted_mark[29],
            }));
        }
        println!(
            "DUPLICATE_RENDER_BENCHMARK {}",
            serde_json::json!({
                "schema_version": 1, "debug_assertions": cfg!(debug_assertions),
                "width": 120, "height": 40, "measurements": measurements,
                "scope": "full App widget to in-memory buffer; synthetic paths; excludes terminal I/O, analysis, and report construction"
            })
        );
    }

    #[tokio::test]
    async fn new_navigation_branches_clear_forward_history_and_save_active_cursor() {
        for layout in [LayoutMode::Tree, LayoutMode::Miller] {
            for action in ["take", "cd", "search-directory", "search-file", "bookmark"] {
                let (_directory, mut app) = scanned_app();
                let root = app.view_root.clone();
                app.layout_mode = layout;
                app.tree_state.selected = 1;
                app.miller_state.selected = 2;
                let expected_cursor = if layout == LayoutMode::Tree { 1 } else { 2 };
                app.forward_history
                    .push((root.join("obsolete"), 0, HashSet::new()));
                let destination = if action == "search-file" {
                    root.join("src/nested")
                } else {
                    root.join("src")
                };
                match action {
                    "take" => app.finish_take(destination.clone(), Ok(false)),
                    "cd" => app.cd_to_path("src"),
                    "search-directory" => app.navigate_to_search_result(&destination),
                    "search-file" => app.navigate_to_search_result(&destination.join("deep.txt")),
                    "bookmark" => {
                        app.user_settings.bookmarks.set('a', destination.clone());
                        app.jump_to_bookmark('a');
                    }
                    _ => unreachable!(),
                }
                assert_eq!(app.view_root, destination, "{action}");
                assert!(app.forward_history.is_empty(), "{action}");
                let origin = app.view_history.last().unwrap();
                assert_eq!(origin.0, root, "{action}");
                assert_eq!(origin.1, expected_cursor, "{action}");
                if action == "cd" && layout == LayoutMode::Miller {
                    assert_eq!(app.miller_state.selected, 0);
                    assert_eq!(app.cached_miller_len, 2);
                }
            }
        }
    }

    #[tokio::test]
    async fn create_and_enter_waits_for_success_and_records_only_new_directories() {
        for existing in [false, true] {
            let (_directory, mut app) = scanned_app();
            let root = app.view_root.clone();
            let destination = root.join("take-target");
            if existing {
                std::fs::create_dir(&destination).unwrap();
            }
            app.execute_take(root.clone(), "take-target".into());
            assert_eq!(app.view_root, root, "navigation must wait for completion");
            let mut rx = app.take_rx.take().unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(&result, ScanResult::TakeComplete { result: Ok(created), .. } if *created != existing)
            );
            app.handle_scan_result(result);
            assert_eq!(app.view_root, destination);
            assert!(destination.is_dir());
            assert_eq!(app.undo_log.len(), usize::from(!existing));
            if let Some(entry) = app.undo_log.pop() {
                assert!(
                    matches!(entry.operation, gravityfile_ops::UndoableOperation::DirectoryCreated { path } if path == destination)
                );
            }
        }
    }

    #[tokio::test]
    async fn create_and_enter_completion_does_not_override_later_navigation() {
        let (_directory, mut app) = scanned_app();
        let root = app.view_root.clone();
        app.execute_take(root.clone(), "created-later".into());
        let mut rx = app.take_rx.take().unwrap();
        let destination = root.join("src");
        app.view_root = destination.clone();
        app.start_scan();
        let later_scan = app.scan_token.clone();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        app.handle_scan_result(result);
        assert_eq!(app.view_root, destination);
        assert!(root.join("created-later").is_dir());
        assert_eq!(app.undo_log.len(), 1);
        assert!(!later_scan.is_cancelled());
        assert!(app.scan_rx.is_some());
    }

    #[tokio::test]
    async fn take_result_survives_another_operation_and_rejects_overlapping_take() {
        let (_directory, mut app) = scanned_app();
        let root = app.view_root.clone();
        app.execute_take(root.clone(), "first".into());
        app.handle_action(KeyAction::Quit);
        assert_eq!(app.mode, AppMode::Normal);
        app.execute_command("q");
        assert_eq!(app.mode, AppMode::Normal);
        assert!(app.take_rx.is_some());
        app.execute_take(root.clone(), "second".into());
        assert!(
            app.operation_message
                .as_ref()
                .unwrap()
                .1
                .contains("still in progress")
        );
        app.execute_create_file(root.clone(), "file".into());
        let later_token = app.scan_token.clone();
        let mut take = app.take_rx.take().unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), take.recv())
            .await
            .unwrap()
            .unwrap();
        app.handle_scan_result(result);
        assert_eq!(app.mode, AppMode::UpdatingEntry);
        assert_eq!(app.view_root, root);
        assert!(app.scan_rx.is_some());
        assert!(!later_token.is_cancelled());
        assert_eq!(app.undo_log.len(), 1);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pump_until_complete(&mut app),
        )
        .await
        .unwrap();
        assert_eq!(app.undo_log.len(), 2);
        assert!(root.join("first").is_dir());
        assert!(root.join("file").is_file());
        assert!(!root.join("second").exists());
    }

    #[tokio::test]
    async fn create_and_enter_preserves_navigation_on_invalid_names_and_conflicts() {
        let (_directory, mut app) = scanned_app();
        let root = app.view_root.clone();
        for name in ["..", "bad\nname", "name."] {
            app.execute_take(root.clone(), name.into());
            assert!(app.take_rx.is_none());
            assert_eq!(app.view_root, root);
            assert!(!app.operation_message.as_ref().unwrap().0);
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("src"), root.join("linked-dir")).unwrap();
        }
        let conflicts: &[&str] = if cfg!(unix) {
            &["copy-a.bin", "linked-dir"]
        } else {
            &["copy-a.bin"]
        };
        for &name in conflicts {
            app.execute_take(root.clone(), name.into());
            let mut rx = app.take_rx.take().unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            app.handle_scan_result(result);
            assert_eq!(app.view_root, root);
            assert!(app.undo_log.is_empty());
            assert!(!app.operation_message.as_ref().unwrap().0);
        }
        assert_eq!(
            std::fs::read(root.join("copy-a.bin")).unwrap(),
            vec![7u8; 8192]
        );
    }

    #[tokio::test]
    async fn create_and_rename_ui_adapters_release_idle_workers() {
        let (create_tx, create_rx) = mpsc::channel(1);
        drop(App::adapt_create_rx(PathBuf::from("created"), create_rx));
        tokio::time::timeout(std::time::Duration::from_secs(5), create_tx.closed())
            .await
            .unwrap();
        let (rename_tx, rename_rx) = mpsc::channel(1);
        drop(App::adapt_rename_rx(
            PathBuf::from("original"),
            "renamed".into(),
            rename_rx,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), rename_tx.closed())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_adapter_records_undo_only_for_successful_mutations() {
        use gravityfile_ops::{CreateResult, OperationComplete, OperationType};
        let path = PathBuf::from("created");
        for operation_type in [OperationType::CreateFile, OperationType::CreateDirectory] {
            for (succeeded, failed, cancelled, expected_undo) in [
                (0, 0, false, false),
                (1, 0, false, true),
                (0, 1, false, false),
                (0, 0, true, false),
            ] {
                let (tx, rx) = mpsc::channel(1);
                let mut adapted = App::adapt_create_rx(path.clone(), rx);
                tx.send(CreateResult::Complete(OperationComplete {
                    operation_type,
                    succeeded,
                    failed,
                    cancelled,
                    skipped: 0,
                    bytes_processed: 0,
                    errors: Vec::new(),
                }))
                .await
                .unwrap();
                drop(tx);
                let Some(ScanResult::OperationComplete {
                    undo_info,
                    succeeded: actual_succeeded,
                    cancelled: actual_cancelled,
                    ..
                }) = adapted.recv().await
                else {
                    panic!("completion expected")
                };
                assert_eq!(actual_succeeded, succeeded);
                assert_eq!(actual_cancelled, cancelled);
                assert_eq!(undo_info.is_some(), expected_undo);
                if let Some(undo) = undo_info {
                    match (operation_type, undo) {
                        (
                            OperationType::CreateFile,
                            gravityfile_ops::UndoableOperation::FileCreated { path: actual },
                        )
                        | (
                            OperationType::CreateDirectory,
                            gravityfile_ops::UndoableOperation::DirectoryCreated { path: actual },
                        ) => assert_eq!(actual, path),
                        _ => panic!("wrong undo kind"),
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn cancelled_undo_delivers_completion_through_ui_adapter() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("created");
        std::fs::write(&path, []).unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        app.undo_log.record_create_file(path.clone());
        app.execute_undo();
        app.scan_token.cancel();
        let mut rx = app.scan_rx.take().unwrap();
        let complete = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(result) = rx.recv().await {
                if let ScanResult::OperationComplete {
                    cancelled,
                    succeeded,
                    ..
                } = result
                {
                    return Some((cancelled, succeeded));
                }
            }
            None
        })
        .await
        .unwrap();
        assert_eq!(complete, Some((true, 0)));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn filesystem_operations_cancel_previous_scan_and_clear_its_status() {
        for operation in ["rename", "file", "directory", "take", "undo", "paste"] {
            let (_dir, mut app) = scanned_app();
            let tree = Arc::clone(app.tree.as_ref().unwrap());
            let root = app.path.clone();
            let (old_tx, old_rx) = mpsc::channel(1);
            app.scan_rx = Some(old_rx);
            app.scan_progress = Some(ScanProgress::new());
            app.analyzing = true;
            let old_token = app.scan_token.clone();
            match operation {
                "rename" => app.execute_rename(root.join("copy-a.bin"), "renamed".into()),
                "file" => app.execute_create_file(root, "created".into()),
                "directory" => app.execute_create_directory(root, "created".into()),
                "take" => app.execute_take(root, "created".into()),
                "undo" => {
                    let path = root.join("created");
                    std::fs::write(&path, []).unwrap();
                    app.undo_log.record_create_file(path);
                    app.execute_undo();
                }
                "paste" => {
                    app.clipboard.yank([root.join("copy-a.bin")], root.clone());
                    app.view_root = root.join("src");
                    app.tree_state.selected = 0;
                    app.paste_with_resolution(Some(gravityfile_ops::ConflictResolution::Skip));
                }
                _ => unreachable!(),
            }
            assert!(old_token.is_cancelled(), "{operation}");
            assert!(old_tx.is_closed(), "{operation}");
            assert!(!app.scan_token.is_cancelled(), "{operation}");
            assert!(app.scan_progress.is_none());
            assert!(!app.analyzing);
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &tree));
            let mut rx = if operation == "take" {
                app.take_rx.take()
            } else {
                app.scan_rx.take()
            }
            .expect("operation receiver");
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let mut completed = false;
                while let Some(result) = rx.recv().await {
                    completed |= matches!(
                        result,
                        ScanResult::OperationComplete { .. } | ScanResult::TakeComplete { .. }
                    );
                }
                assert!(completed, "{operation}");
            })
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn submitted_create_and_rename_retain_completion_and_undo() {
        for mode in [
            AppMode::Renaming,
            AppMode::CreatingFile,
            AppMode::CreatingDirectory,
        ] {
            for conflict in [false, true] {
                let (_dir, mut app) = scanned_app();
                let root = app.path.clone();
                let destination = root.join("result");
                if conflict {
                    std::fs::write(&destination, b"existing").unwrap();
                }
                let mut input = InputState::with_initial("result");
                input.context_path = Some(if mode == AppMode::Renaming {
                    root.join("copy-a.bin")
                } else {
                    root
                });
                app.input_state = Some(input);
                app.mode = mode;
                app.dispatch_key_event(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Enter,
                    crossterm::event::KeyModifiers::NONE,
                ));
                assert_eq!(app.mode, AppMode::UpdatingEntry);
                for key in [
                    crossterm::event::KeyCode::Char('R'),
                    crossterm::event::KeyCode::Char('u'),
                    crossterm::event::KeyCode::Esc,
                    crossterm::event::KeyCode::Char('q'),
                ] {
                    app.dispatch_key_event(crossterm::event::KeyEvent::new(
                        key,
                        crossterm::event::KeyModifiers::NONE,
                    ));
                    assert_eq!(app.mode, AppMode::UpdatingEntry);
                    assert!(app.scan_rx.is_some());
                }
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    pump_until_complete(&mut app),
                )
                .await
                .unwrap();
                assert_eq!(app.mode, AppMode::Normal);
                assert_eq!(app.undo_log.len(), usize::from(!conflict));
                assert_eq!(app.operation_message.as_ref().unwrap().0, !conflict);
                assert!(destination.exists());
                if conflict {
                    assert_eq!(std::fs::read(destination).unwrap(), b"existing");
                }
            }
        }
    }

    #[tokio::test]
    async fn closed_background_channel_releases_operation_modes_without_claiming_success() {
        let (_dir, mut app) = scanned_app();
        let tree = Arc::clone(app.tree.as_ref().unwrap());
        app.undo_log.record_create_file(app.path.join("recorded"));
        for mode in [
            AppMode::Copying,
            AppMode::Moving,
            AppMode::Undoing,
            AppMode::Deleting,
            AppMode::BulkRenaming,
            AppMode::ConflictResolution,
            AppMode::Normal,
            AppMode::Search,
            AppMode::CreatingFile,
        ] {
            let (tx, rx) = mpsc::channel(1);
            drop(tx);
            app.scan_rx = Some(rx);
            app.scan_token = CancellationToken::new();
            app.scan_progress = Some(ScanProgress::new());
            app.analyzing = true;
            app.operation_progress = Some(OperationProgress::new(
                gravityfile_ops::OperationType::Copy,
                1,
                0,
            ));
            app.operation_message = Some((true, "old success".into()));
            app.deletion_message = Some((true, "old deletion".into()));
            app.mode = mode;
            assert!(app.scan_rx.as_mut().unwrap().recv().await.is_none());
            app.handle_background_channel_closed();
            let expected = match mode {
                AppMode::Search | AppMode::CreatingFile => mode,
                _ => AppMode::Normal,
            };
            assert_eq!(app.mode, expected);
            assert!(app.scan_token.is_cancelled());
            assert!(app.scan_rx.is_none());
            assert!(app.scan_progress.is_none());
            assert!(!app.analyzing);
            assert!(app.operation_progress.is_none());
            assert!(app.operation_message.is_none());
            assert!(app.deletion_message.is_none());
            assert!(app.error.as_ref().unwrap().contains("outcome unknown"));
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &tree));
            assert_eq!(app.undo_log.len(), 1);
        }
    }

    #[tokio::test]
    async fn undo_retains_exclusive_ui_ownership_until_cancelled_completion() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("created");
        std::fs::write(&path, []).unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        app.undo_log
            .record_create_file(directory.path().join("older"));
        app.undo_log.record_create_file(path.clone());
        app.execute_undo();
        assert_eq!(app.mode, AppMode::Undoing);
        let token = app.scan_token.clone();
        app.execute_undo();
        app.handle_action(KeyAction::Undo);
        app.dispatch_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('R'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.undo_log.len(), 1);
        assert!(!token.is_cancelled());
        assert_eq!(app.mode, AppMode::Undoing);
        for action in [KeyAction::Cancel, KeyAction::Quit, KeyAction::ForceQuit] {
            app.handle_action(action);
            assert!(token.is_cancelled());
            assert!(app.scan_rx.is_some());
            assert_eq!(app.mode, AppMode::Undoing);
        }
        let mut rx = app.scan_rx.take().unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let result = rx.recv().await.expect("undo completion");
                if matches!(result, ScanResult::OperationComplete { .. }) {
                    break result;
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            &result,
            ScanResult::OperationComplete {
                cancelled: true,
                succeeded: 0,
                ..
            }
        ));
        app.handle_scan_result(result);
        assert_eq!(app.mode, AppMode::Normal);
        // Nothing was attempted, so the whole entry is back for a retry.
        assert_eq!(app.undo_log.len(), 2);
        assert_eq!(
            app.undo_log.peek().unwrap().operation,
            gravityfile_ops::UndoableOperation::FileCreated { path: path.clone() }
        );
        assert!(path.exists());
    }

    #[test]
    fn warning_merge_preserves_distinct_details_and_stable_order() {
        use gravityfile_core::{ScanWarning, WarningKind};
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        let path = directory.path().join("file");
        let first = ScanWarning::new(&path, WarningKind::ReadError, "first failure");
        let different_kind = ScanWarning::new(&path, WarningKind::MetadataError, "first failure");
        let different_message = ScanWarning::new(&path, WarningKind::ReadError, "second failure");
        app.warnings.push(first.clone());
        app.merge_scan_warnings(&[
            first.clone(),
            different_kind.clone(),
            different_message.clone(),
            different_kind.clone(),
        ]);
        assert_eq!(app.warnings, vec![first, different_kind, different_message]);
    }

    #[test]
    fn warning_rows_render_in_the_same_order_as_action_selection() {
        use gravityfile_core::{ScanWarning, WarningKind};
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        app.view = View::Errors;
        app.operation_warnings = vec![ScanWarning::new(
            "operation-entry",
            WarningKind::ReadError,
            "operation detail",
        )];
        app.warnings = vec![ScanWarning::new(
            "scan-entry",
            WarningKind::BrokenSymlink,
            "scan detail",
        )];
        for selected in 0..2 {
            app.selected_warning = selected;
            let area = Rect::new(0, 0, 120, 30);
            let mut buffer = Buffer::empty(area);
            app.render(area, &mut buffer);
            let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            let operation = rendered
                .find("operation-entry")
                .expect("operation row visible");
            let scan = rendered.find("scan-entry").expect("scan row visible");
            assert!(operation < scan);
            assert_eq!(rendered.contains("[d to delete]"), selected == 1);
            let expected = ["operation-entry", "scan-entry"][selected];
            assert_eq!(
                app.displayed_warning(selected).unwrap().path,
                PathBuf::from(expected)
            );
            app.mode = AppMode::Normal;
            app.marked.clear();
            app.handle_action(KeyAction::Delete);
            if selected == 1 {
                assert_eq!(app.mode, AppMode::ConfirmDelete);
                assert_eq!(app.marked.len(), 1);
                assert!(app.marked.contains(&PathBuf::from("scan-entry")));
            } else {
                assert_eq!(app.mode, AppMode::Normal);
                assert!(app.marked.is_empty());
            }
        }
    }

    #[test]
    fn warning_merge_handles_large_repeated_final_batches() {
        use gravityfile_core::{ScanWarning, WarningKind};
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        let warnings: Vec<_> = (0..10_000)
            .map(|index| {
                ScanWarning::new(
                    directory.path().join(index.to_string()),
                    WarningKind::PermissionDenied,
                    "denied",
                )
            })
            .collect();
        app.warnings.extend_from_slice(&warnings[..5_000]);
        app.merge_scan_warnings(&warnings);
        app.merge_scan_warnings(&warnings);
        assert_eq!(app.warnings, warnings);
    }

    #[tokio::test]
    async fn scan_completion_preserves_open_dialog_and_unrelated_error() {
        let (_directory, mut app) = scanned_app();
        let tree = app.tree.as_ref().unwrap().as_ref().clone();
        app.mode = AppMode::Settings;
        let mut edited = SettingsState::new(app.user_settings.clone());
        edited.dirty = true;
        edited.settings.scan_on_startup = !edited.settings.scan_on_startup;
        app.settings_state = Some(edited);
        app.error = Some("Cannot load settings; using defaults".into());
        app.handle_scan_result(ScanResult::ScanComplete(Ok(tree)));
        assert_eq!(app.mode, AppMode::Settings);
        assert!(app.settings_state.as_ref().unwrap().dirty);
        assert_eq!(
            app.error.as_deref(),
            Some("Cannot load settings; using defaults")
        );
        app.handle_scan_result(ScanResult::ScanComplete(Err(
            gravityfile_scan::ScanError::Other {
                message: "scan failed".into(),
            },
        )));
        assert_eq!(app.mode, AppMode::Settings);
        assert!(app.settings_state.as_ref().unwrap().dirty);
        assert!(app.error.as_ref().unwrap().contains("scan failed"));
    }

    #[test]
    fn escape_acknowledges_error_before_changing_selection_or_work() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        let marked = directory.path().join("marked");
        app.marked.insert(marked.clone());
        app.error = Some("visible error".into());
        app.scan_progress = Some(ScanProgress::new());
        app.handle_action(KeyAction::Cancel);
        assert!(app.error.is_none());
        assert!(app.marked.contains(&marked));
        assert!(app.scan_progress.is_some());
        assert!(!app.scan_token.is_cancelled());
        app.handle_action(KeyAction::Cancel);
        assert!(app.marked.is_empty());
    }

    #[test]
    fn editor_handoff_preserves_source_mapping_and_rejects_invalid_edits() {
        for valid in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let first = directory.path().join("a");
            let second = directory.path().join("z");
            std::fs::write(&first, b"first").unwrap();
            std::fs::write(&second, b"second").unwrap();
            let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
            app.marked.extend([second.clone(), first.clone()]);
            app.start_bulk_rename();
            let edit_path = app.bulk_rename_edit.as_ref().unwrap().path().to_owned();
            assert_eq!(std::fs::read_to_string(&edit_path).unwrap(), "a\nz\n");
            app.pending_suspend_command = None; // No external editor is launched by this test.
            std::fs::write(&edit_path, if valid { "b\ny\n" } else { "b\n\n" }).unwrap();
            app.process_bulk_rename_from_editor();
            assert!(!edit_path.exists());
            if valid {
                let state = app.bulk_rename_state.as_ref().unwrap();
                assert_eq!(state.entries[0].original, first);
                assert_eq!(state.entries[0].new_name, "b");
                assert_eq!(state.entries[1].original, second);
                assert_eq!(state.entries[1].new_name, "y");
                assert_eq!(app.mode, AppMode::ConfirmBulkRename);
            } else {
                assert!(app.bulk_rename_state.is_none());
                assert!(app.error.as_ref().unwrap().contains("edited rename list"));
            }
            assert_eq!(app.marked.len(), 2);
            assert_eq!(std::fs::read(first).unwrap(), b"first");
            assert_eq!(std::fs::read(second).unwrap(), b"second");
        }
    }

    #[tokio::test]
    async fn cancelling_bulk_rename_waits_for_worker_result_and_preserves_marks() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        std::fs::write(&source, b"payload").unwrap();
        let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
        app.marked.insert(source.clone());
        app.bulk_rename_state = Some(state::BulkRenameState::new(
            vec![source.clone()],
            vec!["target".into()],
        ));
        app.execute_bulk_rename();
        app.handle_action(KeyAction::Cancel);
        assert!(app.scan_token.is_cancelled());
        assert_eq!(app.mode, AppMode::BulkRenaming);
        assert!(app.scan_rx.is_some());
        let result =
            tokio::time::timeout(Duration::from_secs(5), app.scan_rx.as_mut().unwrap().recv())
                .await
                .unwrap()
                .unwrap();
        app.handle_scan_result(result);
        assert_eq!(app.mode, AppMode::Normal);
        assert!(!app.operation_message.as_ref().unwrap().0);
        assert!(
            app.operation_message
                .as_ref()
                .unwrap()
                .1
                .contains("cancelled")
        );
        assert!(app.marked.contains(&source));
        assert_eq!(std::fs::read(source).unwrap(), b"payload");
        assert!(!directory.path().join("target").exists());
    }

    #[tokio::test]
    async fn bulk_rename_runs_in_background_and_reports_success_or_failure() {
        for fail in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            let target = directory.path().join("target");
            std::fs::write(&source, b"payload").unwrap();
            if fail {
                std::fs::write(&target, b"existing").unwrap();
            }
            let mut app = App::with_config(directory.path().into(), crate::TuiConfig::default());
            app.marked.insert(source.clone());
            app.bulk_rename_state = Some(state::BulkRenameState::new(
                vec![source.clone()],
                vec!["target".into()],
            ));
            app.execute_bulk_rename();
            assert_eq!(app.mode, AppMode::BulkRenaming);
            app.handle_action(KeyAction::MoveDown);
            assert_eq!(app.mode, AppMode::BulkRenaming);
            let result =
                tokio::time::timeout(Duration::from_secs(5), app.scan_rx.as_mut().unwrap().recv())
                    .await
                    .unwrap()
                    .unwrap();
            assert!(matches!(result, ScanResult::BulkRenameComplete(_)));
            app.handle_scan_result(result);
            assert_eq!(app.mode, AppMode::Normal);
            assert_eq!(app.operation_message.as_ref().unwrap().0, !fail);
            if fail {
                assert_eq!(std::fs::read(&source).unwrap(), b"payload");
                assert_eq!(std::fs::read(&target).unwrap(), b"existing");
                assert!(app.marked.contains(&source));
            } else {
                assert!(!source.exists());
                assert_eq!(std::fs::read(&target).unwrap(), b"payload");
                assert!(app.marked.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn dropping_tui_operation_adapters_closes_idle_upstream_channels() {
        let (transfer_tx, transfer_rx) = mpsc::channel(1);
        drop(App::adapt_transfer_rx(transfer_rx));
        tokio::time::timeout(Duration::from_secs(5), transfer_tx.closed())
            .await
            .expect("idle transfer adapter must release its upstream receiver");
    }

    #[tokio::test]
    async fn cancelled_completion_is_visible_and_keeps_partial_undo() {
        let (_dir, mut app) = scanned_app();
        app.handle_scan_result(ScanResult::OperationComplete {
            cancelled: true,
            operation_type: gravityfile_ops::OperationType::Copy,
            succeeded: 1,
            failed: 0,
            skipped: 0,
            bytes_processed: 10,
            errors: Vec::new(),
            undo_info: Some(gravityfile_ops::UndoableOperation::FilesCopied {
                created: vec![app.path.join("copied")],
            }),
            undo_remaining: None,
        });
        let (success, message) = app.operation_message.as_ref().unwrap();
        assert!(!success);
        assert!(message.contains("cancelled"));
        assert!(message.contains("1 items"));
        assert!(app.undo_log.pop().is_some());
    }

    /// Set up a paste whose destination already holds a file of the same name.
    ///
    /// Returns the destination path that will collide.
    fn stage_conflicting_paste(app: &mut App) -> PathBuf {
        let root = app.view_root.clone();
        let source = root.join("src").join("copy-a.bin");
        std::fs::write(&source, b"replacement contents").expect("write the source");

        app.clipboard.yank([source], root.join("src"));

        // Row 0 of the tree is the view root itself, which is a directory, so
        // the paste destination is the scan root — where `copy-a.bin` already
        // exists.
        app.layout_mode = LayoutMode::Tree;
        app.tree_state.selected = 0;

        root.join("copy-a.bin")
    }

    /// Drive the operation channel until it reports a conflict.
    async fn pump_until_conflict(app: &mut App) {
        for _ in 0..64 {
            let Some(rx) = app.scan_rx.as_mut() else {
                panic!("the operation channel closed before reporting a conflict");
            };
            let result = rx.recv().await.expect("the operation ended unexpectedly");
            app.handle_scan_result(result);
            if app.pending_conflict.is_some() {
                return;
            }
        }
        panic!("no conflict was reported");
    }

    /// Drive the operation channel until it reports completion.
    async fn pump_until_complete(app: &mut App) {
        for _ in 0..64 {
            let Some(rx) = app.scan_rx.as_mut() else {
                return;
            };
            let Some(result) = rx.recv().await else {
                return;
            };
            let done = matches!(result, ScanResult::OperationComplete { .. });
            app.handle_scan_result(result);
            if done {
                // `OperationComplete` kicks off a refresh scan the test does
                // not need; drop it so nothing is left running.
                app.scan_token.cancel();
                app.scan_rx = None;
                return;
            }
        }
        panic!("the operation never completed");
    }

    #[tokio::test]
    async fn a_paste_conflict_pauses_until_the_user_answers_and_skip_preserves_the_file() {
        let (_dir, mut app) = scanned_app();
        let destination = stage_conflicting_paste(&mut app);
        let before = std::fs::read(&destination).expect("read the destination");

        app.paste_clipboard();
        assert_eq!(app.mode, AppMode::Copying);

        pump_until_conflict(&mut app).await;
        assert_eq!(app.mode, AppMode::ConflictResolution);

        // The operation is paused, not cancelled: the file is still untouched.
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            before,
            "the operation wrote before the user answered"
        );

        // 's' skips this one item.
        app.dispatch_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.pending_conflict.is_none(), "the request was answered");
        assert_eq!(app.mode, AppMode::Copying, "the operation is still running");

        pump_until_complete(&mut app).await;

        assert_eq!(
            std::fs::read(&destination).unwrap(),
            before,
            "skip must leave the existing file alone"
        );
        let (_, message) = app
            .operation_message
            .clone()
            .expect("a summary is reported");
        assert!(message.contains("1 skipped"), "{message}");
    }

    #[tokio::test]
    async fn answering_a_paste_conflict_with_overwrite_replaces_the_file() {
        let (_dir, mut app) = scanned_app();
        let destination = stage_conflicting_paste(&mut app);
        let before = std::fs::read(&destination).expect("read the destination");

        app.paste_clipboard();
        pump_until_conflict(&mut app).await;

        // 'o' overwrites this one item.
        app.dispatch_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('o'),
            crossterm::event::KeyModifiers::NONE,
        ));
        pump_until_complete(&mut app).await;

        let after = std::fs::read(&destination).unwrap();
        assert_ne!(after, before, "overwrite left the old contents in place");
        assert_eq!(after, b"replacement contents");
    }

    #[tokio::test]
    async fn paste_cancellation_retains_results_until_completion() {
        for mode in [ClipboardMode::Copy, ClipboardMode::Cut] {
            for at_conflict in [false, true] {
                let (_dir, mut app) = scanned_app();
                let destination = stage_conflicting_paste(&mut app);
                let source = app.clipboard.paths[0].clone();
                app.clipboard.mode = mode;
                let before = std::fs::read(&destination).unwrap();
                app.paste_clipboard();
                let expected = if mode == ClipboardMode::Cut {
                    AppMode::Moving
                } else {
                    AppMode::Copying
                };
                if at_conflict {
                    pump_until_conflict(&mut app).await;
                }
                app.dispatch_key_event(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE,
                ));
                assert_eq!(app.mode, expected);
                assert!(app.scan_token.is_cancelled());
                assert!(app.scan_rx.is_some());
                app.handle_action(KeyAction::Undo);
                assert_eq!(app.mode, expected);
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    pump_until_complete(&mut app),
                )
                .await
                .unwrap();
                assert_eq!(app.mode, AppMode::Normal);
                assert!(
                    app.operation_message
                        .as_ref()
                        .unwrap()
                        .1
                        .contains("cancelled")
                );
                assert_eq!(std::fs::read(&destination).unwrap(), before);
                assert_eq!(std::fs::read(&source).unwrap(), b"replacement contents");
            }
        }
    }

    #[tokio::test]
    async fn skipped_move_conflict_resumes_move_mode_after_clipboard_clears() {
        let (_dir, mut app) = scanned_app();
        stage_conflicting_paste(&mut app);
        app.clipboard.mode = ClipboardMode::Cut;
        app.paste_clipboard();
        assert!(app.clipboard.is_empty());
        pump_until_conflict(&mut app).await;
        app.dispatch_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.mode, AppMode::Moving);
        pump_until_complete(&mut app).await;
        assert!(
            app.operation_message
                .as_ref()
                .unwrap()
                .1
                .contains("1 skipped")
        );
    }

    #[tokio::test]
    async fn an_unanswered_conflict_modal_renders_at_every_size() {
        let (_dir, mut app) = scanned_app();
        stage_conflicting_paste(&mut app);
        app.paste_clipboard();
        pump_until_conflict(&mut app).await;

        for &(width, height) in SIZES {
            let _ = render(&app, Rect::new(0, 0, width, height));
        }

        app.scan_token.cancel();
        app.scan_rx = None;
    }
}
