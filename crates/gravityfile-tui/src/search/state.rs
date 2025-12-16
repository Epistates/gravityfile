//! Search state and fuzzy matching implementation.

use std::path::PathBuf;

use nucleo::{Config, Matcher, Utf32Str};

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

/// State for the search functionality.
pub struct SearchState {
    /// Whether search is currently active.
    pub active: bool,
    /// Current search mode.
    pub mode: SearchMode,
    /// Current search query.
    pub query: String,
    /// Cursor position in the query.
    pub cursor: usize,
    /// Filtered results.
    pub results: Vec<SearchResult>,
    /// Currently selected result index.
    pub selected: usize,
    /// Scroll offset for results display.
    pub offset: usize,
    /// The nucleo matcher for fuzzy search.
    matcher: Matcher,
    /// All paths available for searching.
    all_paths: Vec<(PathBuf, String, bool)>, // (path, display_name, is_dir)
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
            results: Vec::new(),
            selected: 0,
            offset: 0,
            matcher: Matcher::new(Config::DEFAULT),
            all_paths: Vec::new(),
        }
    }

    /// Activate search mode.
    pub fn activate(&mut self) {
        self.active = true;
        self.query.clear();
        self.cursor = 0;
        self.results.clear();
        self.selected = 0;
        self.offset = 0;
    }

    /// Deactivate search mode.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.query.clear();
        self.cursor = 0;
        self.results.clear();
        self.selected = 0;
        self.offset = 0;
    }

    /// Update the available paths from the file tree.
    pub fn update_paths(&mut self, paths: Vec<(PathBuf, String, bool)>) {
        self.all_paths = paths;
        if self.active && !self.query.is_empty() {
            self.execute_search();
        }
    }

    /// Set the query and execute search.
    #[allow(dead_code)]
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.cursor = self.query.len();
        self.execute_search();
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.query.insert(self.cursor, c);
        self.cursor += 1;
        self.execute_search();
    }

    /// Delete the character before the cursor.
    pub fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.query.remove(self.cursor);
            self.execute_search();
        }
    }

    /// Delete the character at the cursor.
    pub fn delete_char_at(&mut self) {
        if self.cursor < self.query.len() {
            self.query.remove(self.cursor);
            self.execute_search();
        }
    }

    /// Move cursor left.
    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move cursor right.
    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.query.len());
    }

    /// Move cursor to start.
    pub fn move_cursor_start(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end.
    pub fn move_cursor_end(&mut self) {
        self.cursor = self.query.len();
    }

    /// Clear the query.
    pub fn clear_query(&mut self) {
        self.query.clear();
        self.cursor = 0;
        self.results.clear();
        self.selected = 0;
        self.offset = 0;
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
            // Adjust offset if needed
            if self.selected < self.offset {
                self.offset = self.selected;
            }
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

    /// Execute the search based on current query and mode.
    fn execute_search(&mut self) {
        self.results.clear();
        self.selected = 0;
        self.offset = 0;

        if self.query.is_empty() {
            return;
        }

        match self.mode {
            SearchMode::Fuzzy => self.execute_fuzzy_search(),
            SearchMode::Glob => self.execute_glob_search(),
            SearchMode::Regex => self.execute_regex_search(),
        }

        // Sort by score descending
        self.results.sort_by(|a, b| b.score.cmp(&a.score));

        // Limit results to prevent UI slowdown
        self.results.truncate(1000);
    }

    /// Execute fuzzy search using nucleo.
    fn execute_fuzzy_search(&mut self) {
        let mut query_buf = Vec::new();

        for (path, display, is_dir) in &self.all_paths {
            // Convert display to UTF-32 for nucleo
            query_buf.clear();
            let haystack = Utf32Str::new(display, &mut query_buf);

            // Create pattern from query
            let mut pattern_buf = Vec::new();
            let pattern = Utf32Str::new(&self.query, &mut pattern_buf);

            // Perform fuzzy match
            let mut indices = Vec::new();
            if let Some(score) = self.matcher.fuzzy_indices(haystack, pattern, &mut indices) {
                self.results.push(SearchResult {
                    path: path.clone(),
                    display: display.clone(),
                    score: score as u32,
                    is_dir: *is_dir,
                    matched_indices: indices.iter().map(|&i| i as usize).collect(),
                });
            }
        }
    }

    /// Execute glob pattern search.
    fn execute_glob_search(&mut self) {
        // Simple glob implementation (supports * and ?)
        let pattern = self.query.to_lowercase();

        for (path, display, is_dir) in &self.all_paths {
            let display_lower = display.to_lowercase();
            if Self::matches_glob(&pattern, &display_lower) {
                self.results.push(SearchResult {
                    path: path.clone(),
                    display: display.clone(),
                    score: 100, // Fixed score for glob matches
                    is_dir: *is_dir,
                    matched_indices: Vec::new(),
                });
            }
        }
    }

    /// Execute regex search.
    fn execute_regex_search(&mut self) {
        let Ok(regex) = regex::Regex::new(&self.query) else {
            return;
        };

        for (path, display, is_dir) in &self.all_paths {
            if let Some(m) = regex.find(display) {
                let matched_indices: Vec<usize> = (m.start()..m.end()).collect();
                self.results.push(SearchResult {
                    path: path.clone(),
                    display: display.clone(),
                    score: 100, // Fixed score for regex matches
                    is_dir: *is_dir,
                    matched_indices,
                });
            }
        }
    }

    /// Simple glob pattern matching.
    fn matches_glob(pattern: &str, text: &str) -> bool {
        let pattern_chars: Vec<char> = pattern.chars().collect();
        let text_chars: Vec<char> = text.chars().collect();
        Self::glob_match_recursive(&pattern_chars, &text_chars, 0, 0)
    }

    fn glob_match_recursive(pattern: &[char], text: &[char], pi: usize, ti: usize) -> bool {
        if pi == pattern.len() {
            return ti == text.len();
        }

        if pattern[pi] == '*' {
            // Try matching zero or more characters
            for i in ti..=text.len() {
                if Self::glob_match_recursive(pattern, text, pi + 1, i) {
                    return true;
                }
            }
            return false;
        }

        if ti == text.len() {
            return false;
        }

        if pattern[pi] == '?' || pattern[pi] == text[ti] {
            return Self::glob_match_recursive(pattern, text, pi + 1, ti + 1);
        }

        false
    }
}

#[cfg(test)]
mod tests {
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
