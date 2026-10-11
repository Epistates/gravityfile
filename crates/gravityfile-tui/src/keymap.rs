//! Key bindings.
//!
//! [`ACTIONS`] names every bindable action with its help section and
//! description, and [`DEFAULT_KEYMAP`] binds keys to them. The user's
//! `keymap.toml` adds to and replaces those bindings. Key lookup, the help
//! overlay and the footer hints are all derived from the result, so they
//! cannot disagree with each other.
//!
//! ```toml
//! [[keymap]]
//! on = "<C-n>"
//! run = "move_down"
//!
//! [[keymap]]
//! on = ["d", "D"]
//! run = "none"      # unbind
//! ```

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

use crate::event::KeyAction;

/// A key and its modifiers.
///
/// A character carries its own Shift: `G` is Shift-g and `:` is Shift-; on
/// most layouts. Terminals disagree about also reporting the Shift
/// modifier for them, so it is dropped and the character decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Key {
    /// A key, normalised as described on [`Key`].
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        let code = match code {
            KeyCode::Char(c) if modifiers.contains(KeyModifiers::SHIFT) => {
                KeyCode::Char(c.to_ascii_uppercase())
            }
            KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            code => code,
        };
        let modifiers = match code {
            KeyCode::Char(_) | KeyCode::BackTab => modifiers.difference(KeyModifiers::SHIFT),
            _ => modifiers,
        };
        Self { code, modifiers }
    }

    /// The key a terminal event reports.
    pub fn from_event(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }

    /// The event crossterm reports for this key, with Shift set for an
    /// uppercase letter and for Shift-Tab.
    #[cfg(test)]
    pub fn event(self) -> KeyEvent {
        let shifted = match self.code {
            KeyCode::Char(c) => c.is_uppercase(),
            KeyCode::BackTab => true,
            _ => false,
        };
        let modifiers = if shifted {
            self.modifiers | KeyModifiers::SHIFT
        } else {
            self.modifiers
        };
        KeyEvent::new(self.code, modifiers)
    }

    /// Parse key notation: one character (`j`, `G`, `?`), or a key name in
    /// angle brackets with optional modifier prefixes, as in `<Enter>`,
    /// `<C-u>`, `<S-Tab>`, `<A-x>`, `<F5>`, `<Space>`, or `<lt>` for `<`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Ok(Self::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let unknown = || {
            format!("unknown key {text:?}; use one character, or a name such as <Enter> or <C-u>")
        };
        let mut rest = text
            .strip_prefix('<')
            .and_then(|inner| inner.strip_suffix('>'))
            .filter(|inner| !inner.is_empty())
            .ok_or_else(unknown)?;
        let mut modifiers = KeyModifiers::NONE;
        while rest.len() > 2 && rest.as_bytes()[1] == b'-' {
            modifiers |= match rest.as_bytes()[0].to_ascii_uppercase() {
                b'C' => KeyModifiers::CONTROL,
                b'S' => KeyModifiers::SHIFT,
                b'A' | b'M' => KeyModifiers::ALT,
                _ => return Err(unknown()),
            };
            rest = &rest[2..];
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "enter" | "cr" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "space" => KeyCode::Char(' '),
            "lt" => KeyCode::Char('<'),
            "gt" => KeyCode::Char('>'),
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            name => match name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                Some(n @ 1..=24) => KeyCode::F(n),
                _ => {
                    let mut chars = rest.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => KeyCode::Char(c),
                        _ => return Err(unknown()),
                    }
                }
            },
        };
        Ok(Self::new(code, modifiers))
    }
}

impl fmt::Display for Key {
    /// The key as the help and footer show it, such as `Ctrl-u` or `↑`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            f.write_str("Ctrl-")?;
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            f.write_str("Alt-")?;
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            f.write_str("Shift-")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Up => f.write_str("↑"),
            KeyCode::Down => f.write_str("↓"),
            KeyCode::Left => f.write_str("←"),
            KeyCode::Right => f.write_str("→"),
            KeyCode::Enter => f.write_str("Enter"),
            KeyCode::Esc => f.write_str("Esc"),
            KeyCode::Tab => f.write_str("Tab"),
            KeyCode::BackTab => f.write_str("Shift-Tab"),
            KeyCode::Backspace => f.write_str("Backspace"),
            KeyCode::Delete => f.write_str("Del"),
            KeyCode::Insert => f.write_str("Ins"),
            KeyCode::Home => f.write_str("Home"),
            KeyCode::End => f.write_str("End"),
            KeyCode::PageUp => f.write_str("PgUp"),
            KeyCode::PageDown => f.write_str("PgDn"),
            KeyCode::F(n) => write!(f, "F{n}"),
            code => write!(f, "{code:?}"),
        }
    }
}

/// Help sections, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Navigation,
    Selection,
    Files,
    View,
    Tabs,
    Bookmarks,
    General,
}

impl Section {
    const ALL: [Section; 7] = [
        Section::Navigation,
        Section::Selection,
        Section::Files,
        Section::View,
        Section::Tabs,
        Section::Bookmarks,
        Section::General,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::Navigation => "Navigation",
            Section::Selection => "Selection",
            Section::Files => "Files",
            Section::View => "View",
            Section::Tabs => "Tabs",
            Section::Bookmarks => "Bookmarks",
            Section::General => "General",
        }
    }
}

/// Every bindable action: its name in `keymap.toml`, help section and
/// description.
pub const ACTIONS: &[(&str, KeyAction, Section, &str)] = &[
    (
        "move_down",
        KeyAction::MoveDown,
        Section::Navigation,
        "Move down",
    ),
    ("move_up", KeyAction::MoveUp, Section::Navigation, "Move up"),
    (
        "move_left",
        KeyAction::MoveLeft,
        Section::Navigation,
        "Collapse, or go up a level",
    ),
    (
        "move_right",
        KeyAction::MoveRight,
        Section::Navigation,
        "Expand, or enter a directory",
    ),
    (
        "top",
        KeyAction::JumpToTop,
        Section::Navigation,
        "Jump to the top",
    ),
    (
        "bottom",
        KeyAction::JumpToBottom,
        Section::Navigation,
        "Jump to the bottom",
    ),
    ("page_up", KeyAction::PageUp, Section::Navigation, "Page up"),
    (
        "page_down",
        KeyAction::PageDown,
        Section::Navigation,
        "Page down",
    ),
    (
        "enter",
        KeyAction::DrillDown,
        Section::Navigation,
        "Enter a directory, or open a file",
    ),
    (
        "back",
        KeyAction::NavigateBack,
        Section::Navigation,
        "Go back",
    ),
    (
        "toggle_expand",
        KeyAction::ToggleExpand,
        Section::Navigation,
        "Expand or collapse",
    ),
    (
        "go_to",
        KeyAction::GoTo,
        Section::Navigation,
        "Go to a directory",
    ),
    ("search", KeyAction::Search, Section::Navigation, "Search"),
    (
        "toggle_mark",
        KeyAction::ToggleMark,
        Section::Selection,
        "Mark or unmark",
    ),
    (
        "visual",
        KeyAction::EnterVisual,
        Section::Selection,
        "Mark a range",
    ),
    (
        "clear_marks",
        KeyAction::ClearMarks,
        Section::Selection,
        "Clear marks",
    ),
    ("yank", KeyAction::Yank, Section::Files, "Copy"),
    ("cut", KeyAction::Cut, Section::Files, "Cut"),
    ("paste", KeyAction::Paste, Section::Files, "Paste"),
    ("delete", KeyAction::Delete, Section::Files, "Delete"),
    ("rename", KeyAction::Rename, Section::Files, "Rename"),
    (
        "create_file",
        KeyAction::CreateFile,
        Section::Files,
        "New file",
    ),
    (
        "create_directory",
        KeyAction::CreateDirectory,
        Section::Files,
        "New directory",
    ),
    (
        "take",
        KeyAction::Take,
        Section::Files,
        "New directory, and enter it",
    ),
    ("undo", KeyAction::Undo, Section::Files, "Undo"),
    ("next_view", KeyAction::NextView, Section::View, "Next view"),
    (
        "prev_view",
        KeyAction::PrevView,
        Section::View,
        "Previous view",
    ),
    (
        "toggle_layout",
        KeyAction::ToggleLayout,
        Section::View,
        "Tree or Miller columns",
    ),
    (
        "toggle_size_mode",
        KeyAction::ToggleSizeMode,
        Section::View,
        "Disk usage or apparent sizes",
    ),
    (
        "toggle_details",
        KeyAction::ToggleDetails,
        Section::View,
        "Details panel",
    ),
    (
        "cycle_preview",
        KeyAction::CyclePreviewMode,
        Section::View,
        "Preview mode",
    ),
    (
        "preview_down",
        KeyAction::PreviewDown,
        Section::View,
        "Scroll the preview down",
    ),
    (
        "preview_up",
        KeyAction::PreviewUp,
        Section::View,
        "Scroll the preview up",
    ),
    ("sort", KeyAction::Sort, Section::View, "Next sort order"),
    (
        "reverse_sort",
        KeyAction::ReverseSort,
        Section::View,
        "Reverse the sort",
    ),
    (
        "refresh",
        KeyAction::Refresh,
        Section::View,
        "Rescan this directory",
    ),
    (
        "toggle_theme",
        KeyAction::ToggleTheme,
        Section::View,
        "Light or dark theme",
    ),
    ("new_tab", KeyAction::NewDirTab, Section::Tabs, "New tab"),
    (
        "close_tab",
        KeyAction::CloseDirTab,
        Section::Tabs,
        "Close tab",
    ),
    ("next_tab", KeyAction::NextDirTab, Section::Tabs, "Next tab"),
    (
        "prev_tab",
        KeyAction::PrevDirTab,
        Section::Tabs,
        "Previous tab",
    ),
    (
        "tab_1",
        KeyAction::DirTab(1),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_2",
        KeyAction::DirTab(2),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_3",
        KeyAction::DirTab(3),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_4",
        KeyAction::DirTab(4),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_5",
        KeyAction::DirTab(5),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_6",
        KeyAction::DirTab(6),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_7",
        KeyAction::DirTab(7),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_8",
        KeyAction::DirTab(8),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "tab_9",
        KeyAction::DirTab(9),
        Section::Tabs,
        "Switch to tab",
    ),
    (
        "set_bookmark",
        KeyAction::SetBookmark,
        Section::Bookmarks,
        "Set a bookmark",
    ),
    (
        "jump_to_bookmark",
        KeyAction::JumpToBookmark,
        Section::Bookmarks,
        "Jump to a bookmark",
    ),
    (
        "bookmarks",
        KeyAction::ShowBookmarkList,
        Section::Bookmarks,
        "List bookmarks",
    ),
    (
        "command",
        KeyAction::CommandMode,
        Section::General,
        "Command palette",
    ),
    (
        "settings",
        KeyAction::OpenSettings,
        Section::General,
        "Settings",
    ),
    ("help", KeyAction::ToggleHelp, Section::General, "Help"),
    (
        "cancel",
        KeyAction::Cancel,
        Section::General,
        "Cancel, or clear marks",
    ),
    ("quit", KeyAction::Quit, Section::General, "Quit"),
    (
        "force_quit",
        KeyAction::ForceQuit,
        Section::General,
        "Quit at once",
    ),
];

/// The default bindings, in the order the help lists their keys.
pub const DEFAULT_KEYMAP: &[(&str, KeyAction)] = &[
    ("j", KeyAction::MoveDown),
    ("<Down>", KeyAction::MoveDown),
    ("k", KeyAction::MoveUp),
    ("<Up>", KeyAction::MoveUp),
    ("h", KeyAction::MoveLeft),
    ("<Left>", KeyAction::MoveLeft),
    ("l", KeyAction::MoveRight),
    ("<Right>", KeyAction::MoveRight),
    ("g", KeyAction::JumpToTop),
    ("<Home>", KeyAction::JumpToTop),
    ("G", KeyAction::JumpToBottom),
    ("<End>", KeyAction::JumpToBottom),
    ("<C-u>", KeyAction::PageUp),
    ("<PageUp>", KeyAction::PageUp),
    ("<C-d>", KeyAction::PageDown),
    ("<PageDown>", KeyAction::PageDown),
    ("<Enter>", KeyAction::DrillDown),
    ("<Backspace>", KeyAction::NavigateBack),
    ("-", KeyAction::NavigateBack),
    ("o", KeyAction::ToggleExpand),
    ("<C-g>", KeyAction::GoTo),
    ("/", KeyAction::Search),
    ("<Space>", KeyAction::ToggleMark),
    ("V", KeyAction::EnterVisual),
    ("u", KeyAction::ClearMarks),
    ("y", KeyAction::Yank),
    ("x", KeyAction::Cut),
    ("p", KeyAction::Paste),
    ("d", KeyAction::Delete),
    ("D", KeyAction::Delete),
    ("<Delete>", KeyAction::Delete),
    ("r", KeyAction::Rename),
    ("a", KeyAction::CreateFile),
    ("A", KeyAction::CreateDirectory),
    ("T", KeyAction::Take),
    ("<C-z>", KeyAction::Undo),
    ("<Tab>", KeyAction::NextView),
    ("<S-Tab>", KeyAction::PrevView),
    ("v", KeyAction::ToggleLayout),
    ("b", KeyAction::ToggleSizeMode),
    ("i", KeyAction::ToggleDetails),
    ("P", KeyAction::CyclePreviewMode),
    ("J", KeyAction::PreviewDown),
    ("K", KeyAction::PreviewUp),
    ("s", KeyAction::Sort),
    ("S", KeyAction::ReverseSort),
    ("R", KeyAction::Refresh),
    ("t", KeyAction::ToggleTheme),
    ("<C-t>", KeyAction::NewDirTab),
    ("<C-w>", KeyAction::CloseDirTab),
    ("]", KeyAction::NextDirTab),
    ("[", KeyAction::PrevDirTab),
    ("1", KeyAction::DirTab(1)),
    ("2", KeyAction::DirTab(2)),
    ("3", KeyAction::DirTab(3)),
    ("4", KeyAction::DirTab(4)),
    ("5", KeyAction::DirTab(5)),
    ("6", KeyAction::DirTab(6)),
    ("7", KeyAction::DirTab(7)),
    ("8", KeyAction::DirTab(8)),
    ("9", KeyAction::DirTab(9)),
    ("m", KeyAction::SetBookmark),
    ("'", KeyAction::JumpToBookmark),
    ("`", KeyAction::JumpToBookmark),
    ("B", KeyAction::ShowBookmarkList),
    (":", KeyAction::CommandMode),
    (",", KeyAction::OpenSettings),
    ("?", KeyAction::ToggleHelp),
    ("<Esc>", KeyAction::Cancel),
    ("n", KeyAction::Cancel),
    ("q", KeyAction::Quit),
    ("<C-c>", KeyAction::ForceQuit),
];

/// The contents of `keymap.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeymapFile {
    #[serde(default)]
    keymap: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    on: Keys,
    run: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Keys {
    One(String),
    Many(Vec<String>),
}

/// The bindings in effect.
#[derive(Debug, Clone)]
pub struct Keymap {
    /// Each key's action, in the order the help lists keys.
    bindings: Vec<(Key, KeyAction)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: DEFAULT_KEYMAP
                .iter()
                .map(|&(key, action)| (Key::parse(key).expect("default keys parse"), action))
                .collect(),
        }
    }
}

impl Keymap {
    /// The defaults with the user's `keymap.toml` applied, and any problems
    /// found in it. Valid entries apply even when others do not.
    pub fn load() -> (Self, Vec<String>) {
        let mut keymap = Self::default();
        let Some(path) = Self::path() else {
            return (keymap, Vec::new());
        };
        let problems = match crate::config::read(&path) {
            Ok(None) => Vec::new(),
            Ok(Some(text)) => match toml::from_str::<KeymapFile>(&text) {
                Ok(file) => keymap.apply(&file),
                Err(error) => vec![error.to_string()],
            },
            Err(error) => vec![error.to_string()],
        };
        let problems = problems
            .into_iter()
            .map(|problem| format!("{}: {problem}", path.display()))
            .collect();
        (keymap, problems)
    }

    fn path() -> Option<std::path::PathBuf> {
        // Unit tests must never read the user's real keymap.
        if cfg!(test) {
            return None;
        }
        gravityfile_core::paths::Dirs::discover().map(|dirs| dirs.keymap_file())
    }

    /// Bind each entry's keys to its action, replacing what they were bound
    /// to; `none` unbinds them. Returns the entries that could not apply.
    pub fn apply(&mut self, file: &KeymapFile) -> Vec<String> {
        let mut problems = Vec::new();
        for entry in &file.keymap {
            let action = if entry.run == "none" {
                None
            } else if let Some(&(_, action, _, _)) =
                ACTIONS.iter().find(|(name, ..)| *name == entry.run)
            {
                Some(action)
            } else {
                problems.push(format!(
                    "unknown action {:?}; see the action names in the README",
                    entry.run
                ));
                continue;
            };
            let keys = match &entry.on {
                Keys::One(key) => std::slice::from_ref(key),
                Keys::Many(keys) => keys.as_slice(),
            };
            for text in keys {
                match Key::parse(text) {
                    Ok(key) => {
                        self.bindings.retain(|(bound, _)| *bound != key);
                        if let Some(action) = action {
                            self.bindings.push((key, action));
                        }
                    }
                    Err(problem) => problems.push(problem),
                }
            }
        }
        problems
    }

    /// The action bound to a key press. A named key such as Enter or Esc
    /// also answers to its modified forms unless those are bound themselves.
    pub fn action(&self, event: KeyEvent) -> KeyAction {
        let key = Key::from_event(event);
        self.find(key)
            .or_else(|| match key.code {
                KeyCode::Char(_) => None,
                _ if key.modifiers.is_empty() => None,
                code => self.find(Key::new(code, KeyModifiers::NONE)),
            })
            .unwrap_or(KeyAction::None)
    }

    fn find(&self, key: Key) -> Option<KeyAction> {
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == key)
            .map(|&(_, action)| action)
    }

    /// The keys bound to `action`, in help order.
    pub fn keys(&self, action: KeyAction) -> impl Iterator<Item = Key> + '_ {
        self.bindings
            .iter()
            .filter(move |&&(_, bound)| bound == action)
            .map(|&(key, _)| key)
    }

    /// The first key of each action, joined with `/`, for footer hints such
    /// as `j/k`. Empty when none of them is bound.
    pub fn hint(&self, actions: &[KeyAction]) -> String {
        actions
            .iter()
            .filter_map(|&action| self.keys(action).next())
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The help: each section's title and its bound actions' keys and
    /// descriptions. Consecutive actions with one description, such as the
    /// tab numbers, share a line.
    pub fn help(&self) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
        Section::ALL
            .iter()
            .map(|&section| {
                let mut lines: Vec<(Vec<String>, &'static str)> = Vec::new();
                for &(_, action, _, description) in
                    ACTIONS.iter().filter(|(.., s, _)| *s == section)
                {
                    let keys: Vec<String> = self.keys(action).map(|key| key.to_string()).collect();
                    if keys.is_empty() {
                        continue;
                    }
                    match lines.last_mut() {
                        Some((shared, last)) if *last == description => shared.extend(keys),
                        _ => lines.push((keys, description)),
                    }
                }
                let lines = lines
                    .into_iter()
                    .map(|(keys, description)| (join_keys(&keys), description))
                    .collect();
                (section.title(), lines)
            })
            .filter(|(_, lines): &(_, Vec<_>)| !lines.is_empty())
            .collect()
    }
}

/// Keys joined with `/`, with a run of three or more consecutive digits
/// shortened to a range such as `1-9`.
fn join_keys(keys: &[String]) -> String {
    let digits: Option<Vec<u32>> = keys
        .iter()
        .map(|key| {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => c.to_digit(10),
                _ => None,
            }
        })
        .collect();
    if let Some(digits) = digits
        && digits.len() >= 3
        && digits.windows(2).all(|pair| pair[1] == pair[0] + 1)
    {
        return format!("{}-{}", digits[0], digits[digits.len() - 1]);
    }
    keys.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyAction {
        Keymap::default().action(KeyEvent::new(code, modifiers))
    }

    fn plain(c: char) -> KeyAction {
        press(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyAction {
        press(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn every_default_key_parses_and_names_a_listed_action() {
        let keymap = Keymap::default();
        assert_eq!(keymap.bindings.len(), DEFAULT_KEYMAP.len());
        for (_, action) in DEFAULT_KEYMAP {
            assert!(
                ACTIONS.iter().any(|(_, listed, ..)| listed == action),
                "{action:?} is bound but not listed"
            );
        }
    }

    #[test]
    fn no_key_is_bound_twice_by_default() {
        let keymap = Keymap::default();
        for (index, (key, _)) in keymap.bindings.iter().enumerate() {
            assert!(
                !keymap.bindings[..index]
                    .iter()
                    .any(|(earlier, _)| earlier == key),
                "{key} is bound twice"
            );
        }
    }

    #[test]
    fn action_names_are_unique() {
        for (index, (name, ..)) in ACTIONS.iter().enumerate() {
            assert!(!ACTIONS[..index].iter().any(|(earlier, ..)| earlier == name));
            assert_ne!(*name, "none", "none means unbind");
        }
    }

    #[test]
    fn the_readme_names_every_action() {
        let readme = include_str!("../../../README.md");
        for (name, ..) in ACTIONS {
            // The README writes the tab numbers as a range.
            let in_range = name
                .strip_prefix("tab_")
                .and_then(|number| number.parse::<u8>().ok())
                .is_some_and(|number| (2..=8).contains(&number));
            assert!(
                in_range || readme.contains(&format!("`{name}`")),
                "README does not list the action `{name}`"
            );
        }
    }

    #[test]
    fn notation_parses_named_keys_and_modifiers() {
        let cases = [
            ("j", KeyCode::Char('j'), KeyModifiers::NONE),
            ("G", KeyCode::Char('G'), KeyModifiers::NONE),
            ("<S-g>", KeyCode::Char('G'), KeyModifiers::NONE),
            ("<C-u>", KeyCode::Char('u'), KeyModifiers::CONTROL),
            (
                "<c-A-x>",
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
            ("<S-Tab>", KeyCode::BackTab, KeyModifiers::NONE),
            ("<Enter>", KeyCode::Enter, KeyModifiers::NONE),
            ("<space>", KeyCode::Char(' '), KeyModifiers::NONE),
            ("<lt>", KeyCode::Char('<'), KeyModifiers::NONE),
            ("<C-->", KeyCode::Char('-'), KeyModifiers::CONTROL),
            ("<F12>", KeyCode::F(12), KeyModifiers::NONE),
            ("<S-Up>", KeyCode::Up, KeyModifiers::SHIFT),
        ];
        for (text, code, modifiers) in cases {
            assert_eq!(Key::parse(text), Ok(Key::new(code, modifiers)), "{text}");
        }
        for bad in ["", "jj", "<>", "<Nope>", "<X-a>", "<F99>", "<C-ab>"] {
            assert!(Key::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_character_matches_whether_or_not_shift_is_reported() {
        // Crossterm sets Shift for uppercase letters; the kitty protocol and
        // some terminals do not, and ':' is shifted on most layouts.
        for (c, expected) in [
            ('G', KeyAction::JumpToBottom),
            ('V', KeyAction::EnterVisual),
            ('R', KeyAction::Refresh),
            (':', KeyAction::CommandMode),
        ] {
            assert_eq!(plain(c), expected, "{c}");
            assert_eq!(
                press(KeyCode::Char(c), KeyModifiers::SHIFT),
                expected,
                "Shift-{c}"
            );
        }
        // Shift with a lowercase letter is the uppercase binding.
        assert_eq!(
            press(KeyCode::Char('g'), KeyModifiers::SHIFT),
            KeyAction::JumpToBottom
        );
    }

    #[test]
    fn destructive_actions_require_their_exact_binding() {
        assert_eq!(plain('d'), KeyAction::Delete);
        assert_eq!(plain('D'), KeyAction::Delete);
        assert_eq!(
            press(KeyCode::Delete, KeyModifiers::NONE),
            KeyAction::Delete
        );
        // Ctrl-d pages; Alt-d does nothing.
        assert_eq!(ctrl('d'), KeyAction::PageDown);
        assert_eq!(
            press(KeyCode::Char('d'), KeyModifiers::ALT),
            KeyAction::None
        );
    }

    #[test]
    fn named_keys_answer_to_their_modified_forms() {
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
        ] {
            assert_eq!(press(KeyCode::Esc, modifiers), KeyAction::Cancel);
            assert_eq!(press(KeyCode::Enter, modifiers), KeyAction::DrillDown);
        }
        // But a modified character is a different key.
        assert_eq!(ctrl('q'), KeyAction::None);
    }

    #[test]
    fn digits_select_tabs_and_zero_does_not() {
        for n in 1u8..=9 {
            let c = char::from_digit(u32::from(n), 10).unwrap();
            assert_eq!(plain(c), KeyAction::DirTab(n));
        }
        assert_eq!(plain('0'), KeyAction::None);
    }

    #[test]
    fn overrides_rebind_unbind_and_report_mistakes() {
        let file: KeymapFile = toml::from_str(
            r#"
            [[keymap]]
            on = "<C-n>"
            run = "move_down"

            [[keymap]]
            on = ["d", "D"]
            run = "none"

            [[keymap]]
            on = "j"
            run = "page_down"

            [[keymap]]
            on = "<Nope>"
            run = "quit"

            [[keymap]]
            on = "z"
            run = "explode"
            "#,
        )
        .unwrap();
        let mut keymap = Keymap::default();
        let problems = keymap.apply(&file);
        assert_eq!(problems.len(), 2, "{problems:?}");
        let press = |code, modifiers| keymap.action(KeyEvent::new(code, modifiers));
        assert_eq!(
            press(KeyCode::Char('n'), KeyModifiers::CONTROL),
            KeyAction::MoveDown
        );
        assert_eq!(
            press(KeyCode::Char('d'), KeyModifiers::NONE),
            KeyAction::None
        );
        assert_eq!(
            press(KeyCode::Char('j'), KeyModifiers::NONE),
            KeyAction::PageDown
        );
        // The other delete key is untouched, and the help follows the change.
        assert_eq!(
            press(KeyCode::Delete, KeyModifiers::NONE),
            KeyAction::Delete
        );
        assert_eq!(keymap.hint(&[KeyAction::MoveDown]), "↓");
        assert!(
            toml::from_str::<KeymapFile>("[[keymap]]\non = 'x'\nrun = 'cut'\nextra = 1").is_err()
        );
    }

    #[test]
    fn help_lists_every_bound_action_once_and_shortens_digit_runs() {
        let keymap = Keymap::default();
        let help = keymap.help();
        let lines: Vec<&(String, &str)> = help.iter().flat_map(|(_, lines)| lines).collect();
        assert!(lines.contains(&&("j/↓".to_string(), "Move down")));
        assert!(lines.contains(&&("1-9".to_string(), "Switch to tab")));
        let mut descriptions: Vec<&str> = lines.iter().map(|(_, d)| *d).collect();
        descriptions.sort_unstable();
        let total = descriptions.len();
        descriptions.dedup();
        assert_eq!(descriptions.len(), total, "a description appears twice");
    }

    #[test]
    fn hints_take_the_first_key_of_each_action() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.hint(&[KeyAction::MoveDown, KeyAction::MoveUp]),
            "j/k"
        );
        assert_eq!(keymap.hint(&[KeyAction::ToggleMark]), "Space");
        assert_eq!(keymap.hint(&[KeyAction::None]), "");
    }
}
