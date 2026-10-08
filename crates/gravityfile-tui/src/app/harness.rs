//! Drive the explorer the way a person does: key presses and clicks go through
//! the same entry point as the terminal, followed by the same post-input and
//! background steps as the run loop, and every frame is drawn through a real
//! `Terminal` so render-time state (scroll offsets) is adopted as in use.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio_util::sync::CancellationToken;

use super::App;
use crate::test_support::sample_dir_on_disk;

/// Longest a single background step may take before the test fails.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) struct Harness {
    pub app: App,
    pub terminal: Terminal<TestBackend>,
    root: PathBuf,
    _fixture: tempfile::TempDir,
}

impl Harness {
    /// The explorer on a fully scanned copy of the shared fixture, in a
    /// 100×30 terminal.
    pub async fn scanned() -> Self {
        Self::scanned_sized(100, 30).await
    }

    pub async fn scanned_sized(width: u16, height: u16) -> Self {
        // Opening a directory must not start a scan the test did not ask for:
        // a scan of an ancestor would reach other tests' fixtures.
        let settings = super::UserSettings {
            scan_on_startup: false,
            ..super::UserSettings::default()
        };
        Self::scanned_with(width, height, settings).await
    }

    /// The scanned fixture, opened with saved `settings`.
    pub async fn scanned_with(width: u16, height: u16, settings: super::UserSettings) -> Self {
        let fixture = sample_dir_on_disk();
        // The scanner stores a canonical root; on macOS temp dirs sit behind
        // a `/var` symlink.
        let root = fixture.path().canonicalize().expect("canonical fixture");
        let mut app = App::with_loaded_settings(
            root.clone(),
            crate::TuiConfig {
                // Tests must never move fixtures into the user's real Trash.
                replace_policy: gravityfile_ops::ReplacePolicy::Permanent,
                delete_mode: gravityfile_ops::DeleteMode::Permanent,
                ..crate::TuiConfig::default()
            },
            Ok(settings),
        );
        app.search_state.enable_background();
        let tree = gravityfile_scan::JwalkScanner::new()
            .scan(
                &gravityfile_core::ScanConfig::new(&root),
                CancellationToken::new(),
            )
            .expect("scan the fixture");
        app.handle_scan_result(super::ScanResult::ScanComplete(Ok(tree)));
        let mut harness = Self {
            app,
            terminal: Terminal::new(TestBackend::new(width, height)).expect("test terminal"),
            root,
            _fixture: fixture,
        };
        harness.settle().await;
        harness.draw();
        harness
    }

    /// The scanned root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path below the scanned root.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Press keys written in a vim-like notation: plain characters are typed
    /// as themselves (uppercase with Shift), and `<name>` stands for a special
    /// key: `<enter> <esc> <space> <tab> <s-tab> <bs> <del> <up> <down>
    /// <left> <right> <home> <end> <pgup> <pgdn>`, or `<c-x>` for Ctrl+x.
    pub async fn press(&mut self, keys: &str) {
        for key in parse_keys(keys) {
            self.event(Event::Key(key)).await;
        }
    }

    /// Type `text` literally, including `<` and spaces.
    pub async fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            let modifiers = if character.is_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            self.event(Event::Key(KeyEvent::new(
                KeyCode::Char(character),
                modifiers,
            )))
            .await;
        }
    }

    pub async fn click(&mut self, column: u16, row: u16) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), column, row)
            .await;
    }

    pub async fn right_click(&mut self, column: u16, row: u16) {
        self.mouse(MouseEventKind::Down(MouseButton::Right), column, row)
            .await;
    }

    pub async fn scroll_down(&mut self, column: u16, row: u16) {
        self.mouse(MouseEventKind::ScrollDown, column, row).await;
    }

    async fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) {
        self.event(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
        .await;
    }

    /// One event, then exactly what the run loop does after it.
    async fn event(&mut self, event: Event) {
        let old_view_root = self.app.view_root.clone();
        self.app.handle_terminal_event(event);
        self.app.after_input(&old_view_root).await;
        self.settle().await;
        self.draw();
    }

    /// Wait until every background task has reported and been handled.
    pub async fn settle(&mut self) {
        loop {
            if self.app.has_background() {
                let old_view_root = self.app.view_root.clone();
                let background = tokio::time::timeout(STEP_TIMEOUT, self.app.next_background())
                    .await
                    .expect("a background task stopped making progress");
                self.app.handle_background(background);
                self.app.after_input(&old_view_root).await;
            } else if self.app.search_state.has_background_work() {
                tokio::time::sleep(Duration::from_millis(5)).await;
                self.app.search_state.poll_background();
            } else {
                return;
            }
        }
    }

    /// Draw a frame as the run loop does and return it as text.
    pub fn draw(&mut self) -> String {
        self.terminal
            .draw(|frame| self.app.draw(frame))
            .expect("draw to the test terminal");
        self.app.adopt_rendered_offsets();
        self.screen()
    }

    /// The last frame, one line per row with trailing spaces removed.
    pub fn screen(&self) -> String {
        let buffer = self.terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer
            .content()
            .chunks(width)
            .map(|row| {
                let line: String = row.iter().map(|cell| cell.symbol()).collect();
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The selected entry's path.
    pub fn selected(&self) -> Option<PathBuf> {
        self.app.get_selected_path()
    }

    /// Move the cursor to the entry named `name` in the current view with
    /// `g` and `j`, as a person would.
    pub async fn select(&mut self, name: &str) {
        self.press("g").await;
        for _ in 0..100 {
            if self
                .selected()
                .is_some_and(|path| path.file_name().is_some_and(|n| n == name))
            {
                return;
            }
            self.press("j").await;
        }
        panic!("{name:?} cannot be selected:\n{}", self.screen());
    }

    /// The row on screen whose text contains `needle`.
    pub fn row_of(&self, needle: &str) -> u16 {
        self.screen()
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{}", self.screen()))
            as u16
    }
}

fn parse_keys(keys: &str) -> Vec<KeyEvent> {
    let mut events = Vec::new();
    let mut characters = keys.chars();
    while let Some(character) = characters.next() {
        if character != '<' {
            let modifiers = if character.is_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            events.push(KeyEvent::new(KeyCode::Char(character), modifiers));
            continue;
        }
        let name: String = characters.by_ref().take_while(|&c| c != '>').collect();
        let code = |code| KeyEvent::new(code, KeyModifiers::NONE);
        events.push(match name.as_str() {
            "enter" => code(KeyCode::Enter),
            "esc" => code(KeyCode::Esc),
            "space" => code(KeyCode::Char(' ')),
            "tab" => code(KeyCode::Tab),
            "s-tab" => KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            "bs" => code(KeyCode::Backspace),
            "del" => code(KeyCode::Delete),
            "up" => code(KeyCode::Up),
            "down" => code(KeyCode::Down),
            "left" => code(KeyCode::Left),
            "right" => code(KeyCode::Right),
            "home" => code(KeyCode::Home),
            "end" => code(KeyCode::End),
            "pgup" => code(KeyCode::PageUp),
            "pgdn" => code(KeyCode::PageDown),
            control if control.starts_with("c-") && control.chars().count() == 3 => {
                let key = control.chars().nth(2).expect("checked length");
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL)
            }
            other => panic!("unknown key <{other}>"),
        });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_notation_covers_characters_specials_and_control() {
        let keys = parse_keys("jG<enter><c-g><space>");
        assert_eq!(
            keys[0],
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)
        );
        assert_eq!(
            keys[1],
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)
        );
        assert_eq!(keys[2].code, KeyCode::Enter);
        assert_eq!(
            keys[3],
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)
        );
        assert_eq!(keys[4].code, KeyCode::Char(' '));
    }
}
