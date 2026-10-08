//! Help overlay: the key bindings in effect, generated from the keymap, then
//! the mouse, the keys particular to some views, and the commands. It flows
//! into two columns when there is room and scrolls when it does not fit.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::event::{KeyAction, get_command_help};
use crate::keymap::Keymap;
use crate::theme::Theme;

/// Help overlay showing the key bindings in effect.
pub struct HelpOverlay<'a> {
    theme: &'a Theme,
    keymap: &'a Keymap,
    scroll: usize,
    /// Set to the largest useful scroll offset when rendered.
    max_scroll: Option<&'a std::cell::Cell<usize>>,
}

impl<'a> HelpOverlay<'a> {
    /// Help for `keymap`, scrolled down `scroll` lines.
    pub fn new(theme: &'a Theme, keymap: &'a Keymap, scroll: usize) -> Self {
        Self {
            theme,
            keymap,
            scroll,
            max_scroll: None,
        }
    }

    /// Report the largest useful scroll offset for the rendered size.
    pub fn max_scroll(mut self, cell: &'a std::cell::Cell<usize>) -> Self {
        self.max_scroll = Some(cell);
        self
    }

    /// Section titles and their (keys, description) rows.
    fn sections(&self) -> Vec<(String, Vec<(String, String)>)> {
        let owned = |rows: Vec<(String, &str)>| {
            rows.into_iter()
                .map(|(keys, text)| (keys, text.to_string()))
                .collect::<Vec<_>>()
        };
        let mut sections: Vec<(String, Vec<(String, String)>)> = self
            .keymap
            .help()
            .into_iter()
            .map(|(title, rows)| (title.to_string(), owned(rows)))
            .collect();
        let mark = self.keymap.hint(&[KeyAction::ToggleMark]);
        let delete = self.keymap.hint(&[KeyAction::Delete]);
        let mut duplicates = Vec::new();
        if !delete.is_empty() {
            duplicates.push((
                format!("{delete} on a group"),
                "Mark all but the first copy".to_string(),
            ));
        }
        if !mark.is_empty() {
            duplicates.push((
                format!("{mark} on a group"),
                "Mark or unmark the whole group".to_string(),
            ));
        }
        if !duplicates.is_empty() {
            sections.push(("Duplicates view".to_string(), duplicates));
        }
        sections.push((
            "Mouse".to_string(),
            owned(vec![
                ("Click".into(), "Select"),
                ("Double-click".into(), "Enter a directory, or open a file"),
                ("Right-click".into(), "Mark or unmark"),
                ("Wheel".into(), "Scroll"),
            ]),
        ));
        sections.push((
            "Commands".to_string(),
            get_command_help()
                .into_iter()
                .map(|(command, text)| (command.to_string(), text.to_string()))
                .collect(),
        ));
        sections
    }

    /// The help as lines, sections separated by a blank line.
    fn lines(&self, key_width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for (title, rows) in self.sections() {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(
                title,
                ratatui::style::Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            ));
            for (keys, text) in rows {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:>key_width$}", super::fit_to_width(&keys, key_width)),
                        self.theme.help_key,
                    ),
                    Span::styled(format!(" {text}"), self.theme.help_desc),
                ]));
            }
        }
        lines
    }
}

impl Widget for HelpOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let popup_width = 100.min(area.width.saturating_sub(4));
        let popup_height = area.height.saturating_sub(4);
        let popup_area = Rect::new(
            area.x + (area.width.saturating_sub(popup_width)) / 2,
            area.y + (area.height.saturating_sub(popup_height)) / 2,
            popup_width,
            popup_height,
        );
        Clear.render(popup_area, buf);
        let block = Block::default()
            .title(" Help: j/k to scroll, ? or Esc to close ")
            .title_style(self.theme.title)
            .borders(Borders::ALL)
            .border_style(self.theme.border);
        let inner = block.inner(popup_area);
        block.render(popup_area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Two columns when each still fits a key and a description.
        let columns: Vec<Rect> = if inner.width >= 80 {
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(inner)
                .to_vec()
        } else {
            vec![inner]
        };
        let lines = self.lines(14);
        let height = inner.height as usize;
        let page = height * columns.len();
        let max_scroll = lines.len().saturating_sub(page);
        if let Some(cell) = self.max_scroll {
            cell.set(max_scroll);
        }
        let scroll = self.scroll.min(max_scroll);
        for (column, area) in columns.iter().enumerate() {
            let start = scroll + column * height;
            for (row, line) in lines.iter().skip(start).take(height).enumerate() {
                line.clone()
                    .render(Rect::new(area.x, area.y + row as u16, area.width, 1), buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(keymap: &Keymap, width: u16, height: u16, scroll: usize) -> (String, usize) {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let max = std::cell::Cell::new(0);
        HelpOverlay::new(&Theme::dark(), keymap, scroll)
            .max_scroll(&max)
            .render(area, &mut buf);
        let text = buf
            .content()
            .chunks(usize::from(width.max(1)))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        (text, max.get())
    }

    #[test]
    fn every_line_is_reachable_by_scrolling() {
        let keymap = Keymap::default();
        let (top, max) = rendered(&keymap, 60, 20, 0);
        assert!(top.contains("Move down"), "{top}");
        assert!(max > 0, "a small window must scroll");
        let (bottom, _) = rendered(&keymap, 60, 20, max);
        // The last command is at the very end of the help.
        let (last_command, _) = *get_command_help().last().unwrap();
        assert!(bottom.contains(last_command), "{bottom}");
        // Scrolling further changes nothing.
        assert_eq!(rendered(&keymap, 60, 20, max + 50).0, bottom);
    }

    #[test]
    fn the_help_follows_rebound_keys() {
        let mut keymap = Keymap::default();
        let file = toml::from_str::<crate::keymap::KeymapFile>(
            "[[keymap]]\non = '<C-n>'\nrun = 'move_down'",
        )
        .unwrap();
        assert!(keymap.apply(&file).is_empty());
        let (text, _) = rendered(&keymap, 120, 60, 0);
        assert!(text.contains("j/↓/Ctrl-n"), "{text}");
    }

    #[test]
    fn renders_at_every_size_without_panicking() {
        let keymap = Keymap::default();
        for width in [0, 1, 5, 30, 79, 80, 200] {
            for height in [0, 1, 5, 40] {
                rendered(&keymap, width, height, 3);
            }
        }
    }
}
