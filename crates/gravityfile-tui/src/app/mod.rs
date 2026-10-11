//! Main application state and logic.

mod archive;
mod background;
mod bulk_edit;
mod commands;
mod constants;
mod deletion;
mod duplicate_filter;
#[cfg(test)]
mod harness;
pub mod input;
#[cfg(test)]
mod journeys;
mod keys;
mod listing;
mod mouse;
mod navigate;
mod operations;
mod plugins;
mod previews;
mod render;
mod scanning;
mod searching;
mod selection;
pub mod state;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::EnableMouseCapture;
use crossterm::execute;
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use gravityfile_analyze::{AgeReport, DuplicateReport};
use gravityfile_core::FileTree;
use gravityfile_ops::{ConflictRequest, OperationProgress, UndoLog};
use gravityfile_scan::ScanProgress;

use crate::TuiConfig;
use crate::preview::{ImageContext, PreviewContent, PreviewMode, PreviewState};
use crate::search::SearchState;
use crate::theme::Theme;

use self::archive::{ArchiveCancellation, ArchiveCompletion};
use self::commands::CommandInput;
use self::constants::TICK_INTERVAL_MS;
use self::input::InputState;
use self::state::{
    AppMode, BookmarkListState, ClipboardMode, ClipboardState, DeletionProgress,
    DuplicatesViewState, LayoutMode, ScanResult, SelectedInfo, SettingsState, SortMode, TabManager,
    UserSettings, View,
};

/// Application result type.
pub type AppResult<T> = color_eyre::Result<T>;

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
    apparent_size: bool,
) -> Result<FileTree, gravityfile_scan::ScanError> {
    let mut config = gravityfile_scan::ScanConfig::new(path);
    config.apparent_size = apparent_size;
    config.include_hidden = settings.show_hidden;
    config.max_entries = settings.max_scan_entries;
    gravityfile_scan::quick_list(path, Some(&config))
}

/// A result from one of the explorer's background tasks; `None` when the task
/// ended without reporting.
enum Background {
    Scan(Option<ScanResult>),
    Operation(Option<ScanResult>),
    Take(Option<ScanResult>),
    Preview(Option<PreviewLoad>),
    Archive(Option<ArchiveCompletion>),
    Refresh(Option<Vec<state::Refreshed>>),
    Listed(Option<state::Listed>),
    History(Option<Option<gravityfile_core::history::Record>>),
    Hooks(Option<Vec<gravityfile_plugin::HookResult>>),
}

/// Main application state.
pub struct App {
    /// Key bindings: defaults with the user's `keymap.toml` applied.
    keymap: crate::keymap::Keymap,
    /// Sizes are file lengths rather than disk usage.
    apparent_size: bool,
    /// The snapshot file being browsed, read-only, instead of the filesystem.
    snapshot: Option<PathBuf>,
    /// Path being analyzed (scan root).
    path: PathBuf,
    /// The active tab's directory, selection and history. Inactive tabs'
    /// navigation is kept by `tab_manager`.
    nav: state::Navigation,
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
    preview_area: std::cell::Cell<ratatui::layout::Rect>,
    /// Scroll offsets computed by the last render (tree, miller). Rendering
    /// is `&self`, so these are adopted into the view states after each frame;
    /// mouse hit-testing depends on them matching what is on screen.
    rendered_offsets: std::cell::Cell<(usize, usize)>,
    /// How far the help is scrolled.
    help_scroll: usize,
    /// The help's largest useful scroll, recorded when it is drawn.
    help_max_scroll: std::cell::Cell<usize>,
    /// Recent clicks, to recognize double clicks.
    clicks: crate::event::ClickTracker,
    /// Scanned file tree.
    ///
    /// Shared rather than owned: `FileTree::clone` is a deep copy of every
    /// node, and this tree is handed to background analysis and stashed in the
    /// navigation cache. Mutation goes through `Arc::make_mut`, which only
    /// copies when a background task still holds a reference.
    tree: Option<Arc<FileTree>>,
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
    /// Disposal of deleted items.
    delete_mode: gravityfile_ops::DeleteMode,
    /// Layout mode (tree vs miller).
    layout_mode: LayoutMode,
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
    /// Results of the running scan or analysis.
    scan_rx: Option<mpsc::Receiver<ScanResult>>,
    take_rx: Option<mpsc::Receiver<ScanResult>>,
    /// Cancels the running scan or analysis.
    scan_token: CancellationToken,
    /// Results of the running filesystem operation, which runs alongside a
    /// scan rather than replacing it.
    op_rx: Option<mpsc::Receiver<ScanResult>>,
    /// Cancels the running filesystem operation.
    op_token: CancellationToken,
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
    /// Completion channel for re-scanning paths changed by an operation.
    refresh_rx: Option<oneshot::Receiver<Vec<state::Refreshed>>>,
    /// Changed paths waiting for the running refresh to finish.
    queued_refresh: Vec<PathBuf>,
    /// Whether to auto-scan on startup.
    scan_on_startup: bool,
    /// User settings (persistent configuration).
    user_settings: UserSettings,
    /// Settings modal state.
    settings_state: Option<SettingsState>,
    /// Bookmark list modal state.
    bookmark_list_state: Option<BookmarkListState>,
    /// Directory listings run off the UI thread report here.
    listing_tx: mpsc::UnboundedSender<state::Listed>,
    listing_rx: mpsc::UnboundedReceiver<state::Listed>,
    /// Listings in flight, so one directory is not listed twice at once.
    listings: HashSet<(state::ListingFor, PathBuf)>,
    /// The scan history to add full scans to; None when not recording.
    history_file: Option<PathBuf>,
    history_rx: Option<oneshot::Receiver<Option<gravityfile_core::history::Record>>>,
    /// The latest earlier scan of this place, for the change shown in the header.
    history_baseline: Option<gravityfile_core::history::Record>,
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
    /// Cached count of treemap rectangles for navigation.
    cached_treemap_len: usize,
    /// Visual selection mode state.
    visual_state: Option<state::VisualState>,
    /// Plugin manager.
    plugin_manager: Option<Arc<RwLock<gravityfile_plugin::PluginManager>>>,
    /// Whether any plugin is loaded; hooks are not fired otherwise.
    plugins_loaded: bool,
    /// Fired hooks report their results here.
    hook_tx: mpsc::UnboundedSender<Vec<gravityfile_plugin::HookResult>>,
    hook_rx: mpsc::UnboundedReceiver<Vec<gravityfile_plugin::HookResult>>,
    /// Hooks fired and not yet reported back.
    hooks_in_flight: usize,
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
        mut config: TuiConfig,
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
        // A snapshot replaces the listing: its tree is the whole explorer.
        let snapshot = config.snapshot.take();
        let snapshot_source = snapshot.as_ref().map(|snapshot| snapshot.source.clone());
        let is_snapshot = snapshot_source.is_some();
        let (quick_tree, listing_error) = match snapshot {
            Some(snapshot) => (Some(snapshot.tree), None),
            None => match quick_list_with_settings(&path, &user_settings, config.apparent_size) {
                Ok(tree) => (Some(tree), None),
                Err(error) => (
                    None,
                    Some(format!("Cannot list {}: {error}", path.display())),
                ),
            },
        };

        // The quick listing resolves relative paths and directory aliases. Use
        // that same identity for every navigation owner; retaining the supplied
        // spelling would make subtree lookup reject the freshly loaded tree.
        let path = quick_tree
            .as_ref()
            .map_or(path, |tree| tree.root_path.clone());

        // Determine scan_on_startup: CLI flag (Some) overrides user settings; None defers to settings.
        let scan_on_startup = !is_snapshot
            && config
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
        let (listing_tx, listing_rx) = mpsc::unbounded_channel();
        let (hook_tx, hook_rx) = mpsc::unbounded_channel();
        // Unit tests must never write to the user's real history.
        let history_file = (!cfg!(test) && !is_snapshot && user_settings.record_history)
            .then(gravityfile_core::paths::Dirs::discover)
            .flatten()
            .map(|dirs| dirs.history_file());
        let (theme, theme_error) = match Theme::named(&user_settings.theme) {
            Some(theme) => (theme, None),
            None => (
                Theme::named("gestalt").unwrap_or_default(),
                Some(format!(
                    "Unknown theme {:?}; use gestalt, latte, nord, dracula or ansi",
                    user_settings.theme
                )),
            ),
        };
        let (keymap, keymap_problems) = crate::keymap::Keymap::load();
        let keymap_error = (!keymap_problems.is_empty())
            .then(|| format!("Ignored keymap entries: {}", keymap_problems.join("; ")));

        let mut app = Self {
            keymap,
            apparent_size: config.apparent_size,
            snapshot: snapshot_source,
            path: path.clone(),
            nav: state::Navigation::new(path.clone()),
            mode: AppMode::default(),
            view: View::default(),
            theme,
            image_picker: None,
            preview_area: std::cell::Cell::new(ratatui::layout::Rect::new(0, 0, 40, 20)),
            rendered_offsets: std::cell::Cell::new((0, 0)),
            help_scroll: 0,
            help_max_scroll: std::cell::Cell::new(0),
            clicks: crate::event::ClickTracker::default(),
            tree: quick_tree.map(Arc::new),
            cached_tree_len: 0,
            duplicates: None,
            duplicate_filter: duplicate_filter::DuplicateFilter::default(),
            age_report: None,
            duplicates_state: DuplicatesViewState::new(),
            selected_stale_dir: 0,
            show_details: true,
            error: [settings_error, theme_error, keymap_error, listing_error]
                .into_iter()
                .flatten()
                .reduce(|first, next| format!("{first}; {next}")),
            marked: HashSet::new(),
            clipboard: ClipboardState::default(),
            active_paste_mode: ClipboardMode::Empty,
            delete_auto_marked: false,
            undo_log: UndoLog::new(100),
            replace_policy: config.replace_policy,
            delete_mode: config.delete_mode,
            layout_mode: user_settings.default_layout,
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
            op_rx: None,
            op_token: CancellationToken::new(),
            warnings: Vec::new(),
            operation_warnings: Vec::new(),
            selected_warning: 0,
            analyzing: false,
            has_full_scan: is_snapshot,
            needs_redraw: true,
            command_input: CommandInput::new(),
            sort_mode: SortMode::default(),
            search_state: SearchState::new(),
            tab_manager: TabManager::new(10),
            preview_state: PreviewState::new(),
            preview_rx: None,
            pending_preview: None,
            archive_rx: None,
            archive_cancel: None,
            refresh_rx: None,
            queued_refresh: Vec::new(),
            scan_on_startup,
            user_settings,
            settings_state: None,
            bookmark_list_state: None,
            history_file,
            history_rx: None,
            history_baseline: None,
            listing_tx,
            listing_rx,
            listings: HashSet::new(),
            cached_parent_tree: None,
            pending_suspend_command: None,
            pending_deletion: None,
            scanned_cache: Some(HashMap::new()),
            bulk_rename_state: None,
            bulk_rename_edit: None,
            cached_treemap_len: 0,
            visual_state: None,
            plugin_manager: Some(plugin_manager),
            plugins_loaded: false,
            hook_tx,
            hook_rx,
            hooks_in_flight: 0,
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
        self.fire(gravityfile_plugin::Hook::OnStartup);

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

            let old_view_root = self.nav.view_root.clone();

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

                background = self.next_background(), if self.has_background() => {
                    self.handle_background(background);
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

            self.after_input(&old_view_root).await;

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

        self.dispatch_and_wait(gravityfile_plugin::Hook::OnShutdown)
            .await;

        self.scan_token.cancel();
        self.op_token.cancel();
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
            last_cwd: self.snapshot.is_none().then_some(self.nav.view_root),
        })
    }

    /// Work that follows each turn of the event loop: navigation hooks, then
    /// the plugin veto check for a staged deletion, which starts or aborts it.
    async fn after_input(&mut self, old_view_root: &Path) {
        self.check_navigation(old_view_root);
        if let Some(items) = self.pending_deletion.take() {
            self.finalize_deletion(items).await;
            self.needs_redraw = true;
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

    // Navigation methods
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

    /// Check if a path is marked for deletion.
    pub fn is_marked(&self, path: &PathBuf) -> bool {
        self.marked.contains(path)
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
            if let Some(theme) = Theme::named(&self.user_settings.theme) {
                self.theme = theme;
            }
        }
        self.settings_state = None;
        self.mode = AppMode::Normal;
        self.needs_redraw = true;
    }

    /// Get duplicate groups filtered to current view_root.
    fn get_filtered_duplicates(&self) -> Option<(Vec<&gravityfile_analyze::DuplicateGroup>, u64)> {
        let report = self.duplicates.as_ref()?;
        let cached = self
            .duplicate_filter
            .get(report, &self.nav.view_root, &self.path);
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
            .get(report, &self.nav.view_root, &self.path)
            .groups
            .get(index)
            .map_or(0, |(_, count)| *count)
    }

    /// Get stale directories filtered to current view_root.
    fn get_filtered_stale_dirs(&self) -> Option<Vec<&gravityfile_analyze::StaleDirectory>> {
        let age = self.age_report.as_ref()?;

        if self.nav.view_root == self.path {
            return Some(age.stale_directories.iter().collect());
        }

        let filtered: Vec<&gravityfile_analyze::StaleDirectory> = age
            .stale_directories
            .iter()
            .filter(|d| d.path.starts_with(&self.nav.view_root))
            .collect();

        Some(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KeyAction, MouseAction};
    use crate::test_support::sample_dir_on_disk;
    use crate::ui::TreeView;
    use gravityfile_core::FileNode;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;
    use state::{AppMode, LayoutMode, ScanResult, SortMode, View};

    #[test]
    fn all_explorer_sort_modes_handle_deep_trees() {
        use gravityfile_core::{FileNode, Timestamps};
        let timestamps = Timestamps::with_modified(std::time::UNIX_EPOCH);
        let mut root = FileNode::new_directory("d", timestamps);
        for _ in 0..20_000 {
            let mut parent = FileNode::new_directory("d", timestamps);
            parent.children.push(crate::test_support::file("z", 10));
            let mut file = crate::test_support::file("z", 10);
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
        let mut a = crate::test_support::file("", 10);
        a.set_file_name(&low);
        let mut b = a.clone();
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
                let mut root = crate::test_support::directory("root", children);
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
        tree.root = crate::test_support::directory("root", vec![b, a]);
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
            app.nav.treemap_state.selected = index;
            let (_, selected) = app.get_selected_treemap_child().unwrap();
            assert_eq!(selected, app.nav.view_root.join(expected));
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
        for name in &names {
            let mut child = crate::test_support::file("", 1);
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
                    .any(|entry| entry.path == tree.root_path.join(name))
            );
        }
        app.layout_mode = LayoutMode::Miller;
        for (index, name) in names.iter().enumerate() {
            app.nav.miller_state.selected = index;
            let path = app.nav.view_root.join(name);
            assert_eq!(app.get_selected_path(), Some(path.clone()));
            let tree = app.tree.as_ref().unwrap();
            assert_eq!(
                App::find_node_at_path(&tree.root, &path, &tree.root_path)
                    .unwrap()
                    .file_name(),
                name
            );
            app.select_entry(Some(&path));
            assert_eq!(app.nav.miller_state.selected, index);
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
            quick_list_with_settings(
                &fixture.path().join("nested"),
                &app.user_settings,
                app.apparent_size,
            ),
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
                "root",
                (1..=count)
                    .map(|index| file(&format!("report-{index:07}.rs"), 1))
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
            touched: Vec::new(),
        });
        assert_eq!(app.undo_log.peek().unwrap().id, 7);
        assert!(app.error.as_deref().unwrap().contains("press u to retry"));
        let (success, message) = app.operation_message.clone().unwrap();
        assert!(!success && message.contains("undo incomplete"));
    }

    #[tokio::test]
    async fn delete_and_marks_never_target_the_scan_root() {
        let (_dir, mut app) = scanned_app();
        app.nav.tree_state.selected = 0;
        assert_eq!(app.get_selected_path().unwrap(), app.nav.view_root);
        app.handle_action(KeyAction::Delete);
        assert!(app.marked.is_empty());
        assert_eq!(app.mode, AppMode::Normal);
        assert!(!app.operation_message.as_ref().unwrap().0);
        app.handle_action(KeyAction::ToggleMark);
        assert!(app.marked.is_empty());
        app.marked.insert(app.path.clone());
        app.marked.insert(app.path.parent().unwrap().to_path_buf());
        app.mode = AppMode::ConfirmDelete;
        app.handle_delete_confirmation(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
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
        app.handle_delete_confirmation(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        ));
        assert!(
            app.marked.contains(&chosen),
            "user marks survive a declined delete"
        );

        app.marked.clear();
        app.nav.tree_state.selected = tree_row(&app, "copy-a.bin");
        app.handle_action(KeyAction::Delete);
        assert_eq!(app.mode, AppMode::ConfirmDelete);
        app.handle_delete_confirmation(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        ));
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
        app.nav.tree_state.selected = src;
        app.toggle_selected();
        let area = Rect::new(0, 0, 80, 12);
        app.nav.tree_state.selected = app.cached_tree_len - 1;
        render(&app, area);
        app.adopt_rendered_offsets();
        let offset = app.nav.tree_state.offset;
        assert!(offset > 0, "the selection forced a scroll");
        let top = app.estimated_list_top_row();
        app.handle_mouse_action(MouseAction::Click { x: 5, y: top });
        assert_eq!(
            app.nav.tree_state.selected, offset,
            "the first visible row was hit"
        );
    }

    #[tokio::test]
    async fn back_reloads_replaced_tree_and_retains_unavailable_history() {
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
            let current = app.nav.view_root.clone();
            let token = app.scan_token.clone();
            app.go_back();
            assert!(token.is_cancelled());
            assert_eq!(app.nav.view_root, origin);
            assert_eq!(app.tree.as_ref().unwrap().root_path, origin);
            assert!(
                app.get_view_root_node()
                    .unwrap()
                    .0
                    .children
                    .iter()
                    .any(|node| &*node.name == "original-file")
            );
            assert!(app.nav.back.is_empty());
            assert_eq!(app.nav.forward.last().unwrap().dir, current);
            app.jump_to_bookmark('d');
            let before = Arc::clone(app.tree.as_ref().unwrap());
            let history = app.nav.back.clone();
            let forward = app.nav.forward.clone();
            std::fs::rename(&origin, fixture.path().join("moved-origin")).unwrap();
            app.go_back();
            assert_eq!(app.nav.view_root, current);
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &before));
            assert_eq!(app.nav.back, history);
            assert_eq!(app.nav.forward, forward);
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
        let root = app.nav.view_root.clone();
        let file = root.join("bookmark-file");
        std::fs::write(&file, "file").unwrap();
        app.user_settings.bookmarks.set('f', file);
        app.user_settings.bookmarks.set('m', root.join("missing"));
        for key in ['z', 'f', 'm'] {
            app.jump_to_bookmark(key);
            assert!(app.error.is_some());
            assert_eq!(app.nav.view_root, root);
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &original));
            assert!(app.nav.back.is_empty());
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
        assert_eq!(app.nav.view_root, destination);
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
        assert_eq!(app.nav.back.len(), 1);
        app.jump_to_bookmark('n');
        assert_eq!(app.nav.back.len(), 1);
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&destination, &alias).unwrap();
            app.user_settings.bookmarks.set('a', alias);
            app.jump_to_bookmark('a');
            assert_eq!(app.nav.view_root, destination);
            assert_eq!(app.nav.back.len(), 1);
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
            "root",
            vec![
                directory("first", vec![]),
                directory("two  日本", vec![]),
                file("file.txt", 1),
            ],
        );
        app.cd_to_path("./first");
        assert_eq!(app.nav.view_root, root.join("first"));
        app.cd_to_path("../two  日本/.");
        assert_eq!(app.nav.view_root, root.join("two  日本"));
        let history_len = app.nav.back.len();
        app.cd_to_path(".");
        assert_eq!(app.nav.back.len(), history_len);
        for (target, message) in [
            ("/missing", "not in the loaded tree"),
            ("/file.txt", "Not a directory"),
            ("../../escape", "leaves the scan root"),
        ] {
            app.cd_to_path(target);
            assert!(app.error.as_ref().unwrap().contains(message));
            assert_eq!(app.nav.view_root, root.join("two  日本"));
            assert_eq!(app.nav.back.len(), history_len);
        }
        app.cd_to_path("/first/../two  日本");
        assert_eq!(app.nav.view_root, root.join("two  日本"));
        assert_eq!(app.nav.back.len(), history_len);
        app.tree = None;
        app.cd_to_path("first");
        assert!(app.error.as_ref().unwrap().contains("no directory tree"));
        assert_eq!(app.nav.back.len(), history_len);
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
            assert_eq!(app.nav.view_root, canonical);
            assert_eq!(app.tree.as_ref().unwrap().root_path, canonical);
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
            "root",
            (1..=1000)
                .map(|index| file(&format!("file-{index:04}"), 1))
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
            "root",
            vec![
                directory(
                    "src",
                    vec![
                        file("日本.rs", 1),
                        directory("nested", vec![file("deep.rs", 1)]),
                    ],
                ),
                directory("src-other", vec![file("outside.rs", 1)]),
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
                    .map(|entry| PathBuf::from(&entry.display))
                    .collect::<Vec<_>>(),
                expected
            );
            for crate::search::SearchEntry { path, display, .. } in paths {
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
        app.nav.view_root = scan_root.join("src/nested");
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
                delete_mode: gravityfile_ops::DeleteMode::Permanent,
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
        // A second tab, with the first one active.
        app.tab_manager
            .open(&mut app.nav, state::Navigation::new(root.join("src")));
        app.tab_manager.switch(&mut app.nav, 0);

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
            .flatten(&app.nav.tree_state)
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
            touched: Vec::new(),
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
        let destination = app.nav.view_root.join("drop.zip");
        app.marked.insert(app.nav.view_root.join("copy-a.bin"));
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
            app.select_entry(Some(&archive));
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
            let source = app.nav.view_root.join("copy-a.bin");
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
            assert!(!app.nav.view_root.join("cancelled.zip").exists());
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
            let root = app.nav.view_root.clone();
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
            let token = app.op_token.clone();
            let message = app.operation_message.clone();
            let (sender, receiver) = oneshot::channel();
            app.archive_rx = Some(receiver);
            drop(sender);
            let completion = app.archive_rx.as_mut().unwrap().await.ok();
            app.finish_archive_task(completion);
            assert!(app.archive_rx.is_none());
            // Unknown outcome: refresh now, or once the creation finishes.
            if active_creation {
                assert!(app.scan_rx.is_none());
                assert!(!app.queued_refresh.is_empty());
            } else {
                assert!(app.scan_rx.is_some());
            }
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
            pump(&mut app, |_| false).await;
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
            assert!(app.queued_refresh.is_empty());
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
            let root = app.nav.view_root.clone();
            let source = root.join("copy-a.bin");
            let archive = root.join("result.zip");
            std::fs::write(&archive, b"leftover extraction output").unwrap();
            app.marked.insert(source.clone());
            if active_creation {
                app.execute_create_file(root.clone(), "created".into());
            }
            let token = app.op_token.clone();
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
            assert!(if active_creation {
                app.op_rx.is_some()
            } else {
                app.scan_rx.is_some()
            });
            if active_creation {
                assert!(!token.is_cancelled());
                assert!(!app.queued_refresh.is_empty());
            }
            pump(&mut app, |_| false).await;
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
            assert!(app.queued_refresh.is_empty());
            assert!(
                app.error
                    .as_deref()
                    .unwrap()
                    .contains("could not be withdrawn")
            );
        }
    }

    #[tokio::test]
    async fn archive_completion_refresh_waits_for_an_active_creation() {
        for compress in [false, true] {
            let (_directory, mut app) = scanned_app();
            let root = app.nav.view_root.clone();
            let archived_source = root.join("copy-a.bin");
            let unrelated_mark = root.join("copy-b.bin");
            app.marked.insert(archived_source.clone());
            app.marked.insert(unrelated_mark.clone());
            app.execute_create_file(root.clone(), "created".into());
            let token = app.op_token.clone();
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
            assert!(app.op_rx.is_some());
            assert!(!token.is_cancelled());
            assert_eq!(app.operation_message, message);
            assert!(!app.queued_refresh.is_empty());
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                pump_until_complete(&mut app),
            )
            .await
            .unwrap();
            assert!(root.join("created").is_file());
            assert_eq!(app.undo_log.len(), 1);
            assert!(app.queued_refresh.is_empty());
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
        assert_eq!(app.nav.view_root, destination);
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
        let root = app.nav.view_root.clone();
        app.user_settings.bookmarks.set('a', root.clone());
        let sort = app.sort_mode;
        for command in ["sort typo", "delmark ab", "go a extra", "unknown"] {
            app.error = None;
            app.execute_command(command);
            assert!(app.error.is_some());
            assert_eq!(app.sort_mode, sort);
            assert_eq!(app.nav.view_root, root);
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
        assert_eq!(app.nav.tree_state.selected, last);
        app.move_down();
        app.page_down();
        assert_eq!(app.nav.tree_state.selected, last);

        app.jump_to_top();
        assert_eq!(app.nav.tree_state.selected, 0);
        app.move_up();
        app.page_up();
        assert_eq!(app.nav.tree_state.selected, 0);
    }

    #[tokio::test]
    async fn expanding_a_directory_grows_the_visible_item_count() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        let collapsed_len = app.cached_tree_len;

        app.nav.tree_state.selected = tree_row(&app, "src");
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
        let original_root = app.nav.view_root.clone();

        let src_index = tree_row(&app, "src");
        app.nav.tree_state.selected = src_index;
        app.enter_selected();
        assert_eq!(app.nav.view_root, original_root.join("src"));

        app.go_back();
        assert_eq!(app.nav.view_root, original_root);
        assert_eq!(
            app.nav.tree_state.selected, src_index,
            "returning restores the row the user drilled from"
        );
    }

    #[tokio::test]
    async fn drilling_in_miller_mode_and_back_restores_the_previous_root() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Miller;
        let original_root = app.nav.view_root.clone();

        let (node, _) = app.get_view_root_node().unwrap();
        let src_index = node
            .children
            .iter()
            .position(|child| child.name.as_str() == "src")
            .expect("src is a child");
        app.nav.miller_state.selected = src_index;

        app.enter_selected();
        assert_eq!(app.nav.view_root, original_root.join("src"));

        app.go_back();
        assert_eq!(app.nav.view_root, original_root);
        assert_eq!(app.nav.miller_state.selected, src_index);
    }

    #[tokio::test]
    async fn drilling_into_a_file_does_nothing() {
        let (_dir, mut app) = scanned_app();
        app.layout_mode = LayoutMode::Tree;
        let root = app.nav.view_root.clone();

        app.nav.tree_state.selected = tree_row(&app, "empty");
        app.enter_selected();
        assert_eq!(
            app.nav.view_root, root,
            "a file is not a directory to enter"
        );
        assert!(app.nav.back.is_empty(), "no history entry was recorded");
    }

    #[tokio::test]
    async fn navigating_back_from_the_scan_root_walks_up_the_filesystem() {
        let (_dir, mut app) = scanned_app();
        let root = app.nav.view_root.clone();
        let parent = root
            .parent()
            .expect("the temp dir has a parent")
            .to_path_buf();

        app.go_back();
        assert_eq!(app.nav.view_root, parent);
    }

    #[tokio::test]
    async fn the_treemap_selection_is_bounded_by_the_rectangle_count() {
        let (_dir, mut app) = scanned_app();
        app.view = View::Treemap;
        app.update_cached_treemap_len();
        let last = app.cached_treemap_len - 1;

        app.jump_to_bottom();
        assert_eq!(app.nav.treemap_state.selected, last);
        app.move_down();
        app.page_down();
        assert_eq!(app.nav.treemap_state.selected, last);

        app.jump_to_top();
        assert_eq!(app.nav.treemap_state.selected, 0);
        app.move_up();
        app.page_up();
        assert_eq!(app.nav.treemap_state.selected, 0);
    }

    #[tokio::test]
    async fn deletion_failures_reach_the_errors_view_and_success_clears_old_details() {
        let (_directory, mut app) = scanned_app();
        let missing = app.path.join("missing-delete-target");
        let removed = app.path.join("temporary-delete-target");
        std::fs::write(&removed, b"temporary").unwrap();
        app.operation_message = Some((true, "Old copy summary".into()));
        let mut rx = deletion::start_deletion(
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
        let mut rx = app.op_rx.take().unwrap();
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
        use gravityfile_core::{FileNode, Timestamps};
        let timestamps = Timestamps::with_modified(std::time::UNIX_EPOCH);
        // The deepest node is the only one with a nonzero size.
        let mut node = FileNode::new_directory("d", timestamps);
        node.size = 42;
        let mut target = PathBuf::from("root");
        for _ in 0..20_000 {
            let mut parent = FileNode::new_directory("d", timestamps);
            parent.children.push(node);
            node = parent;
            target.push("d");
        }
        let actual =
            App::find_node_mut(&mut node, &target, Path::new("root")).map(|found| found.size);
        let mut pending = std::mem::take(&mut node.children);
        while let Some(mut child) = pending.pop() {
            pending.append(&mut child.children);
        }
        assert_eq!(actual, Some(42));
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
        let root: *const FileNode = &tree.root;
        let found = App::find_node_mut(&mut tree.root, &app.path, &app.path).unwrap();
        assert!(std::ptr::eq(found, root));
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
        app.nav.tree_state.selected = tree_row(&app, "src");
        let selected_before = app.get_selected_path();
        assert!(selected_before.is_some());

        app.toggle_layout();
        assert_eq!(app.layout_mode, LayoutMode::Miller);
        assert_eq!(app.get_selected_path(), selected_before);

        app.toggle_layout();
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
        let root = app.nav.view_root.clone();
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
        app.nav.tree_state.selected = tree_row(&app, "copy-a.bin");

        app.handle_action(KeyAction::Delete);
        assert_eq!(app.mode, AppMode::ConfirmDelete);
        assert!(!app.marked.is_empty(), "delete auto-marks the selection");

        // Actions do not reach behind the dialog; only its own keys answer.
        app.handle_action(KeyAction::Cancel);
        assert_eq!(app.mode, AppMode::ConfirmDelete);
        app.handle_delete_confirmation(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('n'),
        ));
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
        assert_eq!(
            app.nav.tree_state.selected, 0,
            "help must not move the cursor"
        );

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
            touched: Vec::new(),
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
            touched: Vec::new(),
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

    /// Manual release-mode measurement of explorer frames over a synthetic
    /// million-file tree with a 200,000-file directory expanded.
    #[tokio::test]
    #[ignore = "manual million-file explorer benchmark; run with --release --ignored --nocapture"]
    async fn explorer_frame_benchmark() {
        use gravityfile_core::{FileNode, FileTree, ScanConfig, Timestamps, TreeStats};
        let directory = tempfile::tempdir().unwrap();
        let root_path = directory.path().canonicalize().unwrap();
        let time = Timestamps::with_modified(std::time::UNIX_EPOCH);
        let mut root = FileNode::new_directory("root", time);
        root.set_file_name(root_path.file_name().unwrap());
        for dir in 0..100 {
            let mut node = FileNode::new_directory(format!("dir-{dir:03}"), time);
            let files = if dir == 0 { 200_000 } else { 8_000 };
            node.children = (0..files)
                .map(|file| {
                    FileNode::new_file(
                        format!("file-{file:07}.bin"),
                        (file as u64 * 7919) % 100_000,
                        8,
                        time,
                        false,
                    )
                })
                .collect();
            root.children.push(node);
        }
        root.update_counts();
        let mut tree = FileTree::new(
            root,
            root_path.clone(),
            ScanConfig::new(&root_path),
            TreeStats::new(),
            std::time::Duration::ZERO,
            Vec::new(),
        );
        tree.remeasure(true, true);
        let mut app = App::with_config(root_path.clone(), crate::TuiConfig::default());
        app.handle_scan_result(ScanResult::ScanComplete(Ok(tree)));
        app.scan_rx = None;
        app.nav.tree_state.expand(&root_path.join("dir-000"));
        app.update_cached_tree_len();
        let area = Rect::new(0, 0, 160, 50);
        let frame = |app: &App| {
            let mut samples: Vec<f64> = (0..21)
                .map(|_| {
                    let start = std::time::Instant::now();
                    std::hint::black_box(render(app, area));
                    start.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            samples.sort_by(f64::total_cmp);
            samples[10]
        };
        let tree_ms = frame(&app);
        for file in 0..100 {
            app.marked
                .insert(root_path.join(format!("dir-001/file-{file:07}.bin")));
        }
        let marked_ms = frame(&app);
        app.marked.clear();
        app.nav.tree_state.selected = 150_000;
        let deep_ms = frame(&app);
        let start = std::time::Instant::now();
        app.handle_action(KeyAction::MoveDown);
        let key_ms = start.elapsed().as_secs_f64() * 1000.0;
        app.layout_mode = LayoutMode::Miller;
        app.update_cached_miller_len();
        let miller_ms = frame(&app);
        app.nav.view_root = root_path.join("dir-000");
        app.update_cached_miller_len();
        app.nav.miller_state.selected = 150_000;
        let miller_large_ms = frame(&app);
        println!(
            "explorer frame (median of 21), 1M files, 200k expanded: tree {tree_ms:.2} ms, \
             with 100 marks {marked_ms:.2} ms, selection 150k deep {deep_ms:.2} ms, \
             miller {miller_ms:.2} ms, miller inside the 200k directory {miller_large_ms:.2} ms; \
             j key {key_ms:.2} ms"
        );
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
                &app.nav.view_root,
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
                let root = app.nav.view_root.clone();
                app.layout_mode = layout;
                app.nav.tree_state.selected = 1;
                app.nav.miller_state.selected = 2;
                let expected_cursor = app.get_selected_path();
                assert!(expected_cursor.is_some());
                app.nav.forward.push(state::Visit {
                    dir: root.join("obsolete"),
                    selected: None,
                    expanded: HashSet::new(),
                });
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
                assert_eq!(app.nav.view_root, destination, "{action}");
                assert!(app.nav.forward.is_empty(), "{action}");
                let origin = app.nav.back.last().unwrap();
                assert_eq!(origin.dir, root, "{action}");
                assert_eq!(origin.selected, expected_cursor, "{action}");
                if action == "cd" && layout == LayoutMode::Miller {
                    assert_eq!(app.nav.miller_state.selected, 0);
                    assert_eq!(app.cached_miller_len, 2);
                }
            }
        }
    }

    #[tokio::test]
    async fn create_and_enter_waits_for_success_and_records_only_new_directories() {
        for existing in [false, true] {
            let (_directory, mut app) = scanned_app();
            let root = app.nav.view_root.clone();
            let destination = root.join("take-target");
            if existing {
                std::fs::create_dir(&destination).unwrap();
            }
            app.execute_take(root.clone(), "take-target".into());
            assert_eq!(
                app.nav.view_root, root,
                "navigation must wait for completion"
            );
            let mut rx = app.take_rx.take().unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(&result, ScanResult::TakeComplete { result: Ok(created), .. } if *created != existing)
            );
            app.handle_scan_result(result);
            assert_eq!(app.nav.view_root, destination);
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
        let root = app.nav.view_root.clone();
        app.execute_take(root.clone(), "created-later".into());
        let mut rx = app.take_rx.take().unwrap();
        let destination = root.join("src");
        app.nav.view_root = destination.clone();
        app.start_scan();
        let later_scan = app.scan_token.clone();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        app.handle_scan_result(result);
        assert_eq!(app.nav.view_root, destination);
        assert!(root.join("created-later").is_dir());
        assert_eq!(app.undo_log.len(), 1);
        assert!(!later_scan.is_cancelled());
        assert!(app.scan_rx.is_some());
    }

    #[tokio::test]
    async fn take_result_survives_another_operation_and_rejects_overlapping_take() {
        let (_directory, mut app) = scanned_app();
        let root = app.nav.view_root.clone();
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
        let later_token = app.op_token.clone();
        let mut take = app.take_rx.take().unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), take.recv())
            .await
            .unwrap()
            .unwrap();
        app.handle_scan_result(result);
        assert_eq!(app.mode, AppMode::UpdatingEntry);
        assert_eq!(app.nav.view_root, root);
        assert!(app.op_rx.is_some());
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
        let root = app.nav.view_root.clone();
        for name in ["..", "bad\nname", "name."] {
            app.execute_take(root.clone(), name.into());
            assert!(app.take_rx.is_none());
            assert_eq!(app.nav.view_root, root);
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
            assert_eq!(app.nav.view_root, root);
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
        app.op_token.cancel();
        let mut rx = app.op_rx.take().unwrap();
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
    async fn filesystem_operations_cancel_analysis_but_not_a_scan() {
        let operations = ["rename", "file", "directory", "take", "undo", "paste"];
        for (operation, analyzing) in operations
            .into_iter()
            .flat_map(|operation| [(operation, true), (operation, false)])
        {
            let (_dir, mut app) = scanned_app();
            let tree = Arc::clone(app.tree.as_ref().unwrap());
            let root = app.path.clone();
            let (old_tx, old_rx) = mpsc::channel(1);
            app.scan_rx = Some(old_rx);
            app.scan_progress = (!analyzing).then(ScanProgress::new);
            app.analyzing = analyzing;
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
                    app.nav.view_root = root.join("src");
                    app.nav.tree_state.selected = 0;
                    app.paste_with_resolution(Some(gravityfile_ops::ConflictResolution::Skip));
                }
                _ => unreachable!(),
            }
            if analyzing {
                // Analysis of the tree the operation is about to change.
                assert!(old_token.is_cancelled(), "{operation}");
                assert!(old_tx.is_closed(), "{operation}");
                assert!(!app.scan_token.is_cancelled(), "{operation}");
                assert!(!app.analyzing);
            } else {
                assert!(!old_token.is_cancelled(), "{operation}");
                assert!(app.scan_rx.is_some(), "{operation}");
                assert!(app.scan_progress.is_some(), "{operation}");
            }
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &tree));
            let mut rx = if operation == "take" {
                app.take_rx.take()
            } else {
                app.op_rx.take()
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
                    assert!(app.op_rx.is_some());
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
    async fn closed_operation_channel_releases_operation_modes_without_claiming_success() {
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
            // The previous round's refresh is still running; the run loop
            // selects among ready tasks at random, so let it finish first.
            pump(&mut app, |_| false).await;
            let (tx, rx) = mpsc::channel(1);
            drop(tx);
            app.op_rx = Some(rx);
            app.op_token = CancellationToken::new();
            app.operation_progress = Some(OperationProgress::new(
                gravityfile_ops::OperationType::Copy,
                1,
                0,
            ));
            app.operation_message = Some((true, "old success".into()));
            app.deletion_message = Some((true, "old deletion".into()));
            app.mode = mode;
            let Background::Operation(None) = app.next_background().await else {
                panic!("the closed operation channel reports first");
            };
            app.handle_background(Background::Operation(None));
            let expected = match mode {
                AppMode::Search | AppMode::CreatingFile => mode,
                _ => AppMode::Normal,
            };
            assert_eq!(app.mode, expected);
            assert!(app.op_token.is_cancelled());
            assert!(app.op_rx.is_none());
            assert!(app.operation_progress.is_none());
            assert!(app.operation_message.is_none());
            assert!(app.deletion_message.is_none());
            assert!(app.error.as_ref().unwrap().contains("outcome unknown"));
            // Nothing is known to have changed: the tree stays until the
            // refresh it starts reports back.
            assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &tree));
            assert_eq!(app.undo_log.len(), 1);
            app.cancel_scan();
        }
    }

    #[tokio::test]
    async fn plugin_notices_and_failures_reach_the_status_line() {
        use gravityfile_plugin::{HookResult, Notice, NoticeLevel};
        let (_dir, mut app) = scanned_app();
        // Without loaded plugins nothing is fired at all.
        app.fire(gravityfile_plugin::Hook::OnStartup);
        assert_eq!(app.hooks_in_flight, 0);

        app.hooks_in_flight = 1;
        let mut said = HookResult::ok();
        said.plugin = Some("git".into());
        said.notices = vec![Notice::new(NoticeLevel::Info, "3 untracked files")];
        app.finish_hook(vec![said]);
        assert_eq!(app.hooks_in_flight, 0);
        assert_eq!(
            app.operation_message,
            Some((true, "git: 3 untracked files".into()))
        );

        let mut failed = HookResult::error("attempt to index a nil value");
        failed.plugin = Some("broken".into());
        app.finish_hook(vec![failed]);
        assert_eq!(
            app.operation_message,
            Some((false, "broken failed: attempt to index a nil value".into()))
        );
    }

    #[tokio::test]
    async fn closed_scan_channel_ends_the_scan_and_keeps_the_listing() {
        let (_dir, mut app) = scanned_app();
        let tree = Arc::clone(app.tree.as_ref().unwrap());
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        app.scan_rx = Some(rx);
        app.scan_progress = Some(ScanProgress::new());
        let token = app.scan_token.clone();
        app.handle_background(Background::Scan(None));
        assert!(token.is_cancelled());
        assert!(app.scan_rx.is_none() && app.scan_progress.is_none());
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("Scan ended without a result")
        );
        assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &tree));
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
        let token = app.op_token.clone();
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
            assert!(app.op_rx.is_some());
            assert_eq!(app.mode, AppMode::Undoing);
        }
        let mut rx = app.op_rx.take().unwrap();
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
        assert!(app.op_token.is_cancelled());
        assert_eq!(app.mode, AppMode::BulkRenaming);
        assert!(app.op_rx.is_some());
        let result =
            tokio::time::timeout(Duration::from_secs(5), app.op_rx.as_mut().unwrap().recv())
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
                tokio::time::timeout(Duration::from_secs(5), app.op_rx.as_mut().unwrap().recv())
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
            touched: Vec::new(),
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
        let root = app.nav.view_root.clone();
        let source = root.join("src").join("copy-a.bin");
        std::fs::write(&source, b"replacement contents").expect("write the source");

        app.clipboard.yank([source], root.join("src"));

        // Row 0 of the tree is the view root itself, which is a directory, so
        // the paste destination is the scan root — where `copy-a.bin` already
        // exists.
        app.layout_mode = LayoutMode::Tree;
        app.nav.tree_state.selected = 0;

        root.join("copy-a.bin")
    }

    /// Deliver results until the running operation reports a conflict.
    async fn pump_until_conflict(app: &mut App) {
        pump(app, |background| {
            matches!(
                background,
                Background::Operation(Some(ScanResult::OperationConflict(_)))
            )
        })
        .await;
        assert!(app.pending_conflict.is_some(), "no conflict was reported");
    }

    /// Deliver background results as the run loop does, until `stop` accepts
    /// one (which is handled too) or nothing is left running.
    async fn pump(app: &mut App, stop: impl Fn(&Background) -> bool) {
        while app.has_background() {
            let background =
                tokio::time::timeout(std::time::Duration::from_secs(10), app.next_background())
                    .await
                    .expect("a background task stopped making progress");
            let done = stop(&background);
            app.handle_background(background);
            if done {
                return;
            }
        }
    }

    /// Deliver results until the running operation completes.
    async fn pump_until_complete(app: &mut App) {
        pump(app, |background| {
            matches!(
                background,
                Background::Operation(Some(ScanResult::OperationComplete { .. }))
            )
        })
        .await;
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
                assert!(app.op_token.is_cancelled());
                assert!(app.op_rx.is_some());
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
