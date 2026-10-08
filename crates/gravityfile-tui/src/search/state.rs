//! Search state and fuzzy matching implementation.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use unicode_segmentation::UnicodeSegmentation;

use nucleo::{Config, Matcher, Utf32Str};

const MAX_SEARCH_RESULTS: usize = 1000;

/// Search mode determines how queries are matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Fuzzy matching (default) - finds approximate matches.
    #[default]
    Fuzzy,
    /// Glob pattern matching (e.g., "*.rs", "src/**/*.ts").
    Glob,
    /// Regular expression matching.
    Regex,
}

impl SearchMode {
    /// Cycle to the next search mode.
    pub fn next(self) -> Self {
        match self {
            Self::Fuzzy => Self::Glob,
            Self::Glob => Self::Regex,
            Self::Regex => Self::Fuzzy,
        }
    }

    /// Get a short label for display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Fuzzy => "Fuzzy",
            Self::Glob => "Glob",
            Self::Regex => "Regex",
        }
    }
}

/// A search result with path and match score.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SearchResult {
    /// Path to the matched item.
    pub path: PathBuf,
    /// Display name (filename or relative path).
    pub display: String,
    /// Match score (higher is better).
    pub score: u32,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Indices of matched characters for highlighting.
    pub matched_indices: Vec<usize>,
}

type SearchPaths = Vec<(PathBuf, String, bool)>;
type IndexBuilder = Box<dyn FnOnce(&AtomicBool) -> SearchPaths + Send>;
struct IndexWorker {
    cancelled: Arc<AtomicBool>,
    receiver: mpsc::Receiver<SearchPaths>,
}

type SearchOutput = (Vec<SearchResult>, Option<String>);

struct SearchWorker {
    cancelled: Arc<AtomicBool>,
    receiver: mpsc::Receiver<SearchOutput>,
}

/// State for the search functionality.
pub struct SearchState {
    /// Whether search is currently active.
    pub active: bool,
    /// Current search mode.
    pub mode: SearchMode,
    /// Current search query.
    pub query: String,
    /// Character index at a grapheme boundary in the query.
    pub cursor: usize,
    /// Pattern compilation error, distinct from a valid search with no matches.
    pub error: Option<String>,
    /// Filtered results.
    pub results: Vec<SearchResult>,
    /// Currently selected result index.
    pub selected: usize,
    /// The nucleo matcher for fuzzy search.
    matcher: Matcher,
    /// All paths available for searching.
    all_paths: Arc<SearchPaths>,
    background: bool,
    index_worker: Option<IndexWorker>,
    pending_index: Option<IndexBuilder>,
    index_error: Option<String>,
    queued: bool,
    worker: Option<SearchWorker>,
    cancellation: Option<Arc<AtomicBool>>,
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchState {
    /// Create a new search state.
    pub fn new() -> Self {
        Self {
            active: false,
            mode: SearchMode::default(),
            query: String::new(),
            cursor: 0,
            error: None,
            results: Vec::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            all_paths: Arc::new(Vec::new()),
            background: false,
            index_worker: None,
            pending_index: None,
            index_error: None,
            queued: false,
            worker: None,
            cancellation: None,
        }
    }

    /// Activate search mode.
    pub fn activate(&mut self) {
        self.active = true;
        self.cancel_index();
        self.cancel_search();
        self.query.clear();
        self.cursor = 0;
        self.error = None;
        self.results.clear();
        self.selected = 0;
    }

    /// Deactivate search mode.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.cancel_index();
        self.cancel_search();
        self.query.clear();
        self.cursor = 0;
        self.error = None;
        self.results.clear();
        self.selected = 0;
    }

    /// Update the available paths from the file tree.
    pub fn update_paths(&mut self, paths: Vec<(PathBuf, String, bool)>) {
        self.cancel_index();
        self.index_error = None;
        self.all_paths = Arc::new(paths);
        if self.active && !self.query.is_empty() {
            self.execute_search();
        }
    }

    /// Build an index without blocking the interactive caller. Query edits wait
    /// for the latest index; replacing or dismissing the scope cancels old work.
    pub(crate) fn build_paths(
        &mut self,
        builder: impl FnOnce(&AtomicBool) -> SearchPaths + Send + 'static,
    ) {
        self.cancel_index();
        self.cancel_search();
        self.index_error = None;
        self.error = None;
        self.results.clear();
        self.selected = 0;
        if !self.background {
            self.update_paths(builder(&AtomicBool::new(false)));
            return;
        }
        self.pending_index = Some(Box::new(builder));
        self.queued = !self.query.is_empty();
        self.start_index();
    }

    fn cancel_index(&mut self) {
        self.pending_index = None;
        if let Some(worker) = &self.index_worker {
            worker.cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn start_index(&mut self) {
        if self.index_worker.is_some() {
            return;
        }
        let Some(builder) = self.pending_index.take() else {
            return;
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::clone(&cancelled);
        // Retire the previous index on the worker, avoiding a large UI-thread free.
        let previous = std::mem::replace(&mut self.all_paths, Arc::new(Vec::new()));
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("gravityfile-search-index".into())
            .spawn(move || {
                drop(previous);
                let paths = builder(&cancellation);
                let _ = sender.send(paths);
            }) {
            Ok(_) => {
                self.index_worker = Some(IndexWorker {
                    cancelled,
                    receiver,
                })
            }
            Err(error) => {
                self.index_error = Some(format!("Cannot start search index worker: {error}"));
                self.error.clone_from(&self.index_error);
                self.queued = false;
            }
        }
    }

    fn poll_index(&mut self) -> bool {
        let Some(worker) = &self.index_worker else {
            return false;
        };
        let output = match worker.receiver.try_recv() {
            Ok(paths) => Ok(paths),
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Search index worker ended without results".to_owned())
            }
        };
        if !worker.cancelled.load(Ordering::Relaxed) {
            match output {
                Ok(paths) => self.all_paths = Arc::new(paths),
                Err(error) => {
                    self.index_error = Some(error);
                    self.error.clone_from(&self.index_error);
                    self.queued = false;
                }
            }
        }
        self.index_worker = None;
        self.start_index();
        self.start_worker();
        true
    }

    /// Number of characters (not bytes) in the query.
    fn query_char_len(&self) -> usize {
        self.query.chars().count()
    }

    /// Byte offset corresponding to the current (char-based) cursor position.
    pub(crate) fn cursor_byte_offset(&self) -> usize {
        self.query
            .char_indices()
            .nth(self.cursor)
            .map(|(byte, _)| byte)
            .unwrap_or(self.query.len())
    }

    /// Set the query and execute search.
    #[allow(dead_code)]
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.cursor = self.query_char_len();
        self.execute_search();
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        let byte = self.cursor_byte_offset();
        self.query.insert(byte, c);
        self.set_cursor_byte(byte + c.len_utf8());
        self.execute_search();
    }

    /// Delete the grapheme before the cursor.
    pub fn delete_char_before(&mut self) {
        let end = self.cursor_byte_offset();
        if end > 0 {
            let start = self.query[..end]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(index, _)| index);
            self.query.replace_range(start..end, "");
            self.set_cursor_byte(start);
            self.execute_search();
        }
    }

    /// Delete the grapheme at the cursor.
    pub fn delete_char_at(&mut self) {
        let start = self.cursor_byte_offset();
        if let Some(grapheme) = self.query[start..].graphemes(true).next() {
            let end = start + grapheme.len();
            self.query.replace_range(start..end, "");
            self.set_cursor_byte(start);
            self.execute_search();
        }
    }

    /// Move cursor left by one grapheme.
    pub fn move_cursor_left(&mut self) {
        let end = self.cursor_byte_offset();
        let start = self.query[..end]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
        self.set_cursor_byte(start);
    }

    /// Move cursor right by one grapheme.
    pub fn move_cursor_right(&mut self) {
        let start = self.cursor_byte_offset();
        let end = start
            + self.query[start..]
                .graphemes(true)
                .next()
                .map_or(0, str::len);
        self.set_cursor_byte(end);
    }

    fn set_cursor_byte(&mut self, byte: usize) {
        let boundary = self
            .query
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|&index| index >= byte)
            .unwrap_or(self.query.len());
        self.cursor = self.query[..boundary].chars().count();
    }

    /// Move cursor to start.
    pub fn move_cursor_start(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end.
    pub fn move_cursor_end(&mut self) {
        self.cursor = self.query_char_len();
    }

    /// Clear the query.
    pub fn clear_query(&mut self) {
        self.cancel_search();
        self.query.clear();
        self.cursor = 0;
        self.error = None;
        self.results.clear();
        self.selected = 0;
    }

    /// Cycle to the next search mode.
    pub fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
        self.execute_search();
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    /// Get the currently selected result.
    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.results.get(self.selected)
    }

    /// Use one background worker at a time, coalescing edits into the latest query.
    pub fn enable_background(&mut self) {
        self.background = true;
    }

    /// Whether the current query is waiting for results.
    pub fn is_searching(&self) -> bool {
        self.pending_index.is_some()
            || self
                .index_worker
                .as_ref()
                .is_some_and(|worker| !worker.cancelled.load(Ordering::Relaxed))
            || self.queued
            || self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.cancelled.load(Ordering::Relaxed))
    }

    /// Whether a worker still needs polling, including one being cancelled.
    pub fn has_background_work(&self) -> bool {
        self.worker.is_some()
            || self.queued
            || self.index_worker.is_some()
            || self.pending_index.is_some()
    }

    fn cancel_search(&mut self) {
        self.queued = false;
        if let Some(worker) = &self.worker {
            worker.cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::Relaxed))
    }

    fn start_worker(&mut self) {
        if self.worker.is_some()
            || !self.queued
            || self.index_worker.is_some()
            || self.pending_index.is_some()
            || self.index_error.is_some()
        {
            return;
        }
        self.queued = false;
        let cancelled = Arc::new(AtomicBool::new(false));
        let all_paths = Arc::clone(&self.all_paths);
        let query = self.query.clone();
        let mode = self.mode;
        let cancellation = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("gravityfile-search".into())
            .spawn(move || {
                let mut state = Self::new();
                state.all_paths = all_paths;
                state.query = query;
                state.mode = mode;
                state.cancellation = Some(cancellation);
                state.execute_search();
                let _ = sender.send((std::mem::take(&mut state.results), state.error.take()));
            }) {
            Ok(_) => {
                self.worker = Some(SearchWorker {
                    cancelled,
                    receiver,
                })
            }
            Err(error) => self.error = Some(format!("Cannot start search worker: {error}")),
        }
    }

    /// Accept only the current query's output; obsolete work is never displayed.
    pub fn poll_background(&mut self) -> bool {
        let index_changed = self.poll_index();
        let Some(worker) = &self.worker else {
            return index_changed;
        };
        let output = match worker.receiver.try_recv() {
            Ok(output) => output,
            Err(mpsc::TryRecvError::Empty) => return index_changed,
            Err(mpsc::TryRecvError::Disconnected) => (
                Vec::new(),
                Some("Search worker ended without results".into()),
            ),
        };
        if !worker.cancelled.load(Ordering::Relaxed) {
            (self.results, self.error) = output;
            self.selected = 0;
        }
        self.worker = None;
        self.start_worker();
        true
    }

    /// Execute the search based on current query and mode.
    fn execute_search(&mut self) {
        self.error.clone_from(&self.index_error);
        self.results.clear();
        self.selected = 0;
        if self.index_error.is_some() {
            return;
        }

        if self.background {
            self.cancel_search();
            self.queued = !self.query.is_empty();
            self.start_worker();
            return;
        }
        if self.query.is_empty() || self.cancelled() {
            return;
        }

        match self.mode {
            SearchMode::Fuzzy => self.execute_fuzzy_search(),
            SearchMode::Glob => self.execute_glob_search(),
            SearchMode::Regex => self.execute_regex_search(),
        }
    }

    /// Score every candidate, retaining only bounded lightweight references.
    /// Materialize paths and highlighting for the winners after ranking.
    fn execute_fuzzy_search(&mut self) {
        let mut pattern_buf = Vec::new();
        let pattern = Utf32Str::new(&self.query, &mut pattern_buf);
        let mut haystack_buf = Vec::new();
        let mut best = BinaryHeap::with_capacity(MAX_SEARCH_RESULTS);
        for (index, (_, display, _)) in self.all_paths.iter().enumerate() {
            if self.cancelled() {
                return;
            }
            haystack_buf.clear();
            let haystack = Utf32Str::new(display, &mut haystack_buf);
            if let Some(score) = self.matcher.fuzzy_match(haystack, pattern) {
                // Largest heap entry is worst: lowest score, then latest input.
                let candidate = (Reverse(score), index);
                if best.len() < MAX_SEARCH_RESULTS {
                    best.push(candidate);
                } else if best.peek().is_some_and(|worst| candidate < *worst) {
                    *best.peek_mut().expect("full candidate heap") = candidate;
                }
            }
        }
        let mut winners = best.into_vec();
        winners.sort_unstable();
        for (Reverse(score), index) in winners {
            let (path, display, is_dir) = &self.all_paths[index];
            if self.cancelled() {
                return;
            }
            haystack_buf.clear();
            let haystack = Utf32Str::new(display, &mut haystack_buf);
            let mut indices = Vec::new();
            self.matcher.fuzzy_indices(haystack, pattern, &mut indices);
            self.results.push(SearchResult {
                path: path.clone(),
                display: display.clone(),
                score: score as u32,
                is_dir: *is_dir,
                matched_indices: indices.into_iter().map(|index| index as usize).collect(),
            });
        }
    }

    /// Execute glob pattern search.
    fn execute_glob_search(&mut self) {
        // Simple glob implementation (supports * and ?)
        let pattern = match Self::compile_glob(&self.query.to_lowercase()) {
            Ok(pattern) => pattern,
            Err(error) => {
                self.error = Some(format!("Invalid glob: {error}"));
                return;
            }
        };

        for (path, display, is_dir) in self.all_paths.iter() {
            if self.cancelled() {
                return;
            }
            let display_lower = display.to_lowercase();
            if pattern.is_match(&display_lower) {
                self.results.push(SearchResult {
                    path: path.clone(),
                    display: display.clone(),
                    score: 100, // Fixed score for glob matches
                    is_dir: *is_dir,
                    matched_indices: Vec::new(),
                });
                if self.results.len() == MAX_SEARCH_RESULTS {
                    break;
                }
            }
        }
    }

    /// Execute regex search.
    fn execute_regex_search(&mut self) {
        let regex = match regex::Regex::new(&self.query) {
            Ok(regex) => regex,
            Err(error) => {
                self.error = Some(format!("Invalid regex: {error}"));
                return;
            }
        };

        for (path, display, is_dir) in self.all_paths.iter() {
            if self.cancelled() {
                return;
            }
            if let Some(m) = regex.find(display) {
                // Convert byte offsets to char indices so highlighting (which
                // enumerates chars) lines up with multi-byte display names.
                let start_char = display[..m.start()].chars().count();
                let end_char = display[..m.end()].chars().count();
                let matched_indices: Vec<usize> = (start_char..end_char).collect();
                self.results.push(SearchResult {
                    path: path.clone(),
                    display: display.clone(),
                    score: 100, // Fixed score for regex matches
                    is_dir: *is_dir,
                    matched_indices,
                });
                if self.results.len() == MAX_SEARCH_RESULTS {
                    break;
                }
            }
        }
    }

    /// Compile the existing glob language (* and ?) once per query. Regex uses
    /// bounded automata rather than recursively enumerating wildcard splits.
    fn compile_glob(pattern: &str) -> Result<regex::Regex, regex::Error> {
        let mut expression = String::from("\\A");
        let mut star = false;
        for character in pattern.chars() {
            match character {
                '*' => {
                    if !star {
                        expression.push_str(".*");
                    }
                    star = true;
                }
                '?' => {
                    expression.push('.');
                    star = false;
                }
                character => {
                    expression.push_str(&regex::escape(&character.to_string()));
                    star = false;
                }
            }
        }
        expression.push_str("\\z");
        regex::RegexBuilder::new(&expression)
            .dot_matches_new_line(true)
            .build()
    }

    #[cfg(test)]
    fn matches_glob(pattern: &str, text: &str) -> bool {
        Self::compile_glob(pattern).unwrap().is_match(text)
    }
}

impl Drop for SearchState {
    fn drop(&mut self) {
        self.cancel_index();
        self.cancel_search();
    }
}

#[cfg(test)]
mod tests {
    fn settle(state: &mut SearchState) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while state.has_background_work() {
            state.poll_background();
            assert!(
                std::time::Instant::now() < deadline,
                "search worker timed out"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn background_index_replacement_coalesces_scopes_and_uses_latest_query() {
        let mut state = SearchState::new();
        state.activate();
        state.enable_background();
        let (entered, entry) = mpsc::sync_channel(1);
        state.build_paths(move |cancelled| {
            entered.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !cancelled.load(Ordering::Relaxed) {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            vec![("old".into(), "old".into(), false)]
        });
        entry
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        state.set_query("old".into());
        assert!(state.worker.is_none());
        assert!(state.is_searching());
        state.build_paths(|_| panic!("superseded index must never run"));
        state.build_paths(|_| vec![("new".into(), "new".into(), false)]);
        state.set_query("new".into());
        settle(&mut state);
        assert_eq!(state.results.len(), 1);
        assert_eq!(state.results[0].path, PathBuf::from("new"));
        state.build_paths(|_| vec![("discard".into(), "discard".into(), false)]);
        let token = Arc::clone(&state.index_worker.as_ref().unwrap().cancelled);
        state.deactivate();
        assert!(token.load(Ordering::Relaxed));
        settle(&mut state);
        assert!(state.results.is_empty());
        assert!(!state.is_searching());
    }

    #[test]
    fn failed_index_is_visible_and_recovers_only_with_a_new_index() {
        let mut state = SearchState::new();
        state.activate();
        state.enable_background();
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        state.index_worker = Some(IndexWorker {
            cancelled: Arc::new(AtomicBool::new(false)),
            receiver,
        });
        state.set_query("new".into());
        settle(&mut state);
        assert!(state.error.as_ref().unwrap().contains("index worker ended"));
        state.set_query("again".into());
        assert!(state.error.is_some());
        assert!(!state.has_background_work());
        state.build_paths(|_| vec![("again".into(), "again".into(), false)]);
        settle(&mut state);
        assert!(state.error.is_none());
        assert_eq!(state.results.len(), 1);
    }

    #[test]
    fn background_search_coalesces_edits_and_rejects_completed_stale_results() {
        let mut state = SearchState::new();
        state.activate();
        state.enable_background();
        state.update_paths(vec![("old".into(), "old".into(), false)]);
        state.set_query("old".into());
        let old_token = Arc::clone(&state.worker.as_ref().unwrap().cancelled);
        // A result that finishes before the next edit must also be rejected.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match state.worker.as_ref().unwrap().receiver.try_recv() {
                Ok(output) => {
                    let (sender, receiver) = mpsc::sync_channel(1);
                    sender.send(output).unwrap();
                    state.worker.as_mut().unwrap().receiver = receiver;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
                Err(error) => panic!("{error}"),
            }
        }
        for query in ["o", "ol", "new"] {
            state.set_query(query.into());
            assert!(Arc::ptr_eq(
                &old_token,
                &state.worker.as_ref().unwrap().cancelled
            ));
            assert!(state.results.is_empty());
        }
        state.update_paths(vec![("new".into(), "new".into(), false)]);
        assert!(old_token.load(Ordering::Relaxed));
        assert!(state.is_searching());
        settle(&mut state);
        assert_eq!(state.results.len(), 1);
        assert_eq!(state.results[0].path, PathBuf::from("new"));
        state.mode = SearchMode::Regex;
        state.set_query("[".into());
        settle(&mut state);
        assert!(state.error.as_ref().unwrap().contains("Invalid regex"));
        state.set_query("new".into());
        state.cycle_mode();
        settle(&mut state);
        assert!(state.error.is_none());
        assert_eq!(state.results.len(), 1);
        state.set_query("new".into());
        state.deactivate();
        settle(&mut state);
        assert!(state.results.is_empty());
        assert!(!state.is_searching());
    }

    #[test]
    fn background_cancellation_clears_pending_and_drop_signals_worker() {
        let mut state = SearchState::new();
        state.enable_background();
        state.set_query("one".into());
        let token = Arc::clone(&state.worker.as_ref().unwrap().cancelled);
        state.set_query("two".into());
        state.clear_query();
        assert!(!state.queued);
        assert!(token.load(Ordering::Relaxed));
        settle(&mut state);
        assert!(state.results.is_empty());
        state.set_query("three".into());
        let token = Arc::clone(&state.worker.as_ref().unwrap().cancelled);
        drop(state);
        assert!(token.load(Ordering::Relaxed));

        for mode in [SearchMode::Fuzzy, SearchMode::Glob, SearchMode::Regex] {
            let mut worker = SearchState::new();
            worker.mode = mode;
            worker.cancellation = Some(Arc::new(AtomicBool::new(true)));
            worker.update_paths(vec![("match".into(), "match".into(), false)]);
            worker.set_query("match".into());
            assert!(worker.results.is_empty());
        }
    }

    #[test]
    #[ignore = "manual optimized search-scale measurement"]
    fn search_scale_benchmark() {
        use super::*;
        use std::time::Instant;
        let mut measurements = Vec::new();
        for count in [100_000, 1_000_000] {
            let build_started = Instant::now();
            let paths = (0..count)
                .map(|index| {
                    let display = format!("src/module-{}/report-{index:07}.rs", index % 1024);
                    (PathBuf::from(&display), display, false)
                })
                .collect();
            let mut state = SearchState::new();
            state.update_paths(paths);
            let index_build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
            for (label, mode, query, expected) in [
                ("fuzzy-many", SearchMode::Fuzzy, "rpt", MAX_SEARCH_RESULTS),
                ("glob-many", SearchMode::Glob, "*.rs", MAX_SEARCH_RESULTS),
                (
                    "regex-many",
                    SearchMode::Regex,
                    "report-[0-9]+\\.rs$",
                    MAX_SEARCH_RESULTS,
                ),
                ("glob-missing", SearchMode::Glob, "*a*a*a*a*a*a*z", 0),
                ("regex-missing", SearchMode::Regex, "NEVERMATCH", 0),
            ] {
                let mut elapsed_ms = Vec::new();
                state.mode = mode;
                for _ in 0..3 {
                    let started = Instant::now();
                    state.set_query(query.into());
                    elapsed_ms.push(started.elapsed().as_secs_f64() * 1000.0);
                    assert!(state.error.is_none());
                    assert_eq!(state.results.len(), expected);
                }
                measurements.push(serde_json::json!({"paths": count, "case": label,
                    "query": query, "index_build_ms": index_build_ms,
                    "elapsed_ms": elapsed_ms, "retained_results": state.results.len()}));
            }
        }
        println!(
            "SEARCH_SCALE_JSON={}",
            serde_json::json!({"schema_version": 1,
            "measurements": measurements,
            "limits": ["Synthetic in-memory paths", "Includes query compilation and result materialization",
                "No filesystem scan, terminal rendering, or peak RSS measurement", "Queries execute synchronously"]})
        );
    }

    #[test]
    fn bounded_search_preserves_full_sort_ranking_and_equal_score_order() {
        use super::*;
        let paths: Vec<_> = (0..1800)
            .map(|index| {
                (
                    PathBuf::from(format!("path-{index}")),
                    if index >= 1700 {
                        "ab".into()
                    } else {
                        format!("a{}b", "x".repeat(index % 30))
                    },
                    false,
                )
            })
            .collect();
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut expected = Vec::new();
        for (index, (_, display, _)) in paths.iter().enumerate() {
            let mut haystack = Vec::new();
            let mut needle = Vec::new();
            let mut indices = Vec::new();
            if let Some(score) = matcher.fuzzy_indices(
                Utf32Str::new(display, &mut haystack),
                Utf32Str::new("ab", &mut needle),
                &mut indices,
            ) {
                expected.push((Reverse(score), index, indices));
            }
        }
        expected.sort_by_key(|(score, index, _)| (*score, *index));
        expected.truncate(MAX_SEARCH_RESULTS);
        let mut state = SearchState::new();
        state.update_paths(paths.clone());
        state.set_query("ab".into());
        assert_eq!(state.results.len(), MAX_SEARCH_RESULTS);
        for (actual, (Reverse(score), index, indices)) in state.results.iter().zip(expected) {
            assert_eq!(actual.path, paths[index].0);
            assert_eq!(actual.score, score as u32);
            assert_eq!(
                actual.matched_indices,
                indices
                    .into_iter()
                    .map(|index| index as usize)
                    .collect::<Vec<_>>()
            );
        }
        for (mode, query) in [(SearchMode::Glob, "*"), (SearchMode::Regex, ".*")] {
            state.mode = mode;
            state.set_query(query.into());
            assert_eq!(state.results.len(), MAX_SEARCH_RESULTS);
            for (result, (path, _, _)) in state.results.iter().zip(&paths) {
                assert_eq!(&result.path, path);
            }
        }
    }
    #[test]
    fn glob_wildcards_do_not_backtrack_and_regex_errors_clear_on_edit() {
        use super::*;
        assert!(!SearchState::matches_glob(
            &format!("{}b", "*a".repeat(40)),
            &"a".repeat(80)
        ));
        for (pattern, text, expected) in [
            ("?", "日", true),
            ("?", "\n", true),
            ("[a]", "a", false),
            ("[a]", "[a]", true),
            ("a.b", "axb", false),
            ("*", "", true),
            ("a", "a\n", false),
            ("**a**", "ba\nc", true),
        ] {
            assert_eq!(SearchState::matches_glob(pattern, text), expected);
        }
        let mut state = SearchState::new();
        state.mode = SearchMode::Regex;
        state.set_query("[".into());
        assert!(state.error.as_ref().unwrap().contains("Invalid regex"));
        assert!(state.results.is_empty());
        state.set_query("valid".into());
        assert!(state.error.is_none());
        state.set_query("[".into());
        state.cycle_mode();
        assert!(state.error.is_none());
        state.mode = SearchMode::Regex;
        state.set_query("[".into());
        state.clear_query();
        assert!(state.error.is_none());
    }
    #[test]
    fn search_editing_moves_and_deletes_whole_graphemes() {
        use super::*;
        for query in ["é日本", "e\u{301}👩‍💻🇺🇸1️⃣"] {
            let mut state = SearchState::new();
            for character in query.chars() {
                state.insert_char(character);
            }
            assert_eq!(state.query, query);
            let boundaries: Vec<_> = query
                .grapheme_indices(true)
                .map(|(index, _)| index)
                .chain(std::iter::once(query.len()))
                .collect();
            for &boundary in boundaries.iter().rev().skip(1) {
                state.move_cursor_left();
                assert_eq!(state.cursor_byte_offset(), boundary);
            }
            for &boundary in boundaries.iter().skip(1) {
                state.move_cursor_right();
                assert_eq!(state.cursor_byte_offset(), boundary);
            }
            for remaining in (0..query.graphemes(true).count()).rev() {
                state.delete_char_before();
                assert_eq!(state.query.graphemes(true).count(), remaining);
            }
            state.set_query(query.into());
            state.move_cursor_start();
            for remaining in (0..query.graphemes(true).count()).rev() {
                state.delete_char_at();
                assert_eq!(state.query.graphemes(true).count(), remaining);
            }
        }
        let mut state = SearchState::new();
        state.set_query("🇺x🇸".into());
        state.move_cursor_left();
        state.delete_char_before();
        assert_eq!(state.query, "🇺🇸");
        assert_eq!(state.cursor_byte_offset(), state.query.len());
    }
    use super::*;

    #[test]
    fn test_search_mode_cycle() {
        let mode = SearchMode::Fuzzy;
        assert_eq!(mode.next(), SearchMode::Glob);
        assert_eq!(mode.next().next(), SearchMode::Regex);
        assert_eq!(mode.next().next().next(), SearchMode::Fuzzy);
    }

    #[test]
    fn test_glob_matching() {
        assert!(SearchState::matches_glob("*.rs", "main.rs"));
        assert!(SearchState::matches_glob("*.rs", "lib.rs"));
        assert!(!SearchState::matches_glob("*.rs", "main.py"));
        assert!(SearchState::matches_glob("test?", "test1"));
        assert!(SearchState::matches_glob("test?", "testa"));
        assert!(!SearchState::matches_glob("test?", "test"));
        assert!(SearchState::matches_glob("*test*", "mytest.rs"));
    }

    #[test]
    fn test_search_state_cursor() {
        let mut state = SearchState::new();
        state.activate();

        state.insert_char('t');
        state.insert_char('e');
        state.insert_char('s');
        state.insert_char('t');
        assert_eq!(state.query, "test");
        assert_eq!(state.cursor, 4);

        state.move_cursor_left();
        assert_eq!(state.cursor, 3);

        state.move_cursor_start();
        assert_eq!(state.cursor, 0);

        state.move_cursor_end();
        assert_eq!(state.cursor, 4);

        state.delete_char_before();
        assert_eq!(state.query, "tes");
        assert_eq!(state.cursor, 3);
    }
}
