//! Application rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs, Widget};
use strum::IntoEnumIterator;
use unicode_width::UnicodeWidthStr;

use gravityfile_analyze::format_age;

use crate::app::App;
use crate::event::KeyAction;
use crate::ui::modals::{
    BookmarkListModal, BookmarkPrompt, BulkRenameConfirmModal, CommandPalette, ConflictModal,
    DeleteConfirmModal, DeletionProgressModal, InputModal, OperationProgressModal, SettingsModal,
};
use crate::ui::{
    AppLayout, HelpOverlay, MillerColumns, TreeView, TreemapView, fit_left_to_width, fit_to_width,
    format_duration, format_relative_time, format_size,
};

use super::state::{AppMode, ClipboardMode, LayoutMode, View};

/// Item in the duplicates list (either a group header or a file within a group).
enum DuplicateListItem<'a> {
    GroupHeader {
        group: &'a gravityfile_analyze::DuplicateGroup,
        files_in_view: usize,
        is_expanded: bool,
        is_selected: bool,
    },
    File {
        file_idx: usize,
        path: &'a std::path::PathBuf,
        is_selected: bool,
        is_marked: bool,
    },
}

/// Map a flattened grouped list to visible (group, item) coordinates without
/// allocating or visiting every file row. Item zero is the group header.
fn duplicate_viewport(
    groups: impl Iterator<Item = (usize, Option<usize>)> + Clone,
    height: usize,
) -> (Vec<(usize, usize)>, usize, usize) {
    let mut total = 0usize;
    let mut selected = None;
    for (count, selection) in groups.clone() {
        if let Some(index) = selection.filter(|index| *index < count) {
            selected = Some(total + index);
        }
        total += count;
    }
    if height == 0 {
        return (Vec::new(), 0, total);
    }
    let offset = selected.unwrap_or(0).saturating_sub(height - 1);
    let end = offset.saturating_add(height).min(total);
    let mut visible = Vec::with_capacity(end - offset);
    let mut base = 0usize;
    for (group, (count, _)) in groups.enumerate() {
        if base >= end {
            break;
        }
        let start = offset.saturating_sub(base).min(count);
        let stop = end.saturating_sub(base).min(count);
        visible.extend((start..stop).map(|item| (group, item)));
        base += count;
    }
    (visible, offset, total)
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Views that are not drawn this frame keep their current offsets.
        self.rendered_offsets
            .set((self.nav.tree_state.offset, self.nav.miller_state.offset));
        // Fill entire area with theme background color
        let base_style = Style::default()
            .bg(self.theme.background)
            .fg(self.theme.foreground);
        buf.set_style(area, base_style);

        // Determine if we need to show directory tabs
        let show_dir_tabs = self.tab_manager.len() > 1;

        // Layout: header, [dir_tabs], view_tabs, content, footer
        let (header, dir_tabs_area, tabs_area, content, footer) = if show_dir_tabs {
            let [header, dir_tabs, tabs_area, content, footer] = area.layout(&Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ]));
            (header, Some(dir_tabs), tabs_area, content, footer)
        } else {
            let [header, tabs_area, content, footer] = area.layout(&Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ]));
            (header, None, tabs_area, content, footer)
        };

        // Render header
        self.render_header(header, buf);

        // Render directory tabs (if multiple tabs open)
        if let Some(dir_tabs) = dir_tabs_area {
            self.render_dir_tabs(dir_tabs, buf);
        }

        // Render view tabs (Explorer, Duplicates, Age, Errors)
        self.render_tabs(tabs_area, buf);

        // Render scan/analysis state as a non-modal banner above the active view.
        let content = self.render_activity_banner(content, buf);

        // Render content based on view
        match self.view {
            View::Explorer => self.render_explorer(content, buf),
            View::Duplicates => self.render_duplicates(content, buf),
            View::Age => self.render_age(content, buf),
            View::Errors => self.render_errors(content, buf),
            View::Treemap => self.render_treemap(content, buf),
        }

        // Render footer
        self.render_footer(footer, buf);

        // Render overlays
        self.render_overlays(area, footer, buf);
    }
}

impl App {
    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let title = Span::styled(
            " gravityfile ",
            self.theme.title.add_modifier(Modifier::BOLD),
        );

        // Show background activity without blocking normal exploration.
        let scanning_indicator = if self.scan_progress.is_some() {
            if let Some(progress) = &self.scan_progress {
                Span::styled(
                    format!(
                        " ⟳ scanning: {} items, {} ",
                        progress.total_items(),
                        format_size(progress.bytes_scanned)
                    ),
                    Style::default()
                        .fg(self.theme.info)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    " ⟳ scanning... ",
                    Style::default()
                        .fg(self.theme.info)
                        .add_modifier(Modifier::BOLD),
                )
            }
        } else if self.analyzing {
            Span::styled(
                " ⟳ analyzing duplicates/age ",
                Style::default()
                    .fg(self.theme.info)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        };

        let stats = if let Some(tree) = &self.tree {
            let suffix = if !self.has_full_scan && self.scan_progress.is_none() {
                " (quick)"
            } else {
                ""
            };
            format!(
                " {} in {} files, {} dirs{} ",
                format_size(tree.stats.total_size),
                tree.stats.total_files,
                tree.stats.total_dirs,
                suffix
            )
        } else if self.scan_progress.is_some() {
            " starting scan ".to_string()
        } else {
            String::new()
        };

        let stats_span = Span::styled(stats, self.theme.header);

        // Show status message or selection info
        let status = if let Some((success, msg)) = &self.operation_message {
            let color = if *success {
                self.theme.success
            } else {
                self.theme.warning
            };
            Span::styled(format!(" {} ", msg), Style::default().fg(color))
        } else if let Some((success, msg)) = &self.deletion_message {
            let color = if *success {
                self.theme.success
            } else {
                self.theme.warning
            };
            Span::styled(format!(" {} ", msg), Style::default().fg(color))
        } else if !self.marked.is_empty() {
            let total_size: u64 = self
                .marked
                .iter()
                .filter_map(|p| self.get_path_size(p))
                .fold(0u64, u64::saturating_add);
            Span::styled(
                format!(
                    " {} selected ({}) ",
                    self.marked.len(),
                    format_size(total_size)
                ),
                Style::default()
                    .fg(self.theme.background)
                    .bg(self.theme.info),
            )
        } else {
            Span::raw("")
        };

        // Show clipboard status
        let clipboard_status = if !self.clipboard.is_empty() {
            let mode_str = match self.clipboard.mode {
                ClipboardMode::Copy => "copied",
                ClipboardMode::Cut => "cut",
                ClipboardMode::Empty => "",
            };
            if !mode_str.is_empty() {
                Span::styled(
                    format!(" {} {} ", self.clipboard.paths.len(), mode_str),
                    Style::default()
                        .fg(self.theme.background)
                        .bg(self.theme.success),
                )
            } else {
                Span::raw("")
            }
        } else {
            Span::raw("")
        };

        let line = Line::from(vec![
            title,
            scanning_indicator,
            Span::raw(" "),
            stats_span,
            status,
            clipboard_status,
        ]);

        Paragraph::new(line)
            .style(self.theme.header)
            .render(area, buf);
    }

    fn render_activity_banner(&self, area: Rect, buf: &mut Buffer) -> Rect {
        if area.height < 5 {
            return area;
        }

        let Some(line) = self.activity_banner_line(area.width as usize) else {
            return area;
        };

        let [banner, rest] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        Paragraph::new(line)
            .style(Style::default().bg(self.theme.background))
            .render(banner, buf);
        rest
    }

    fn activity_banner_line(&self, width: usize) -> Option<Line<'static>> {
        if let Some(progress) = &self.scan_progress {
            let files_per_second = progress.files_per_second();
            let rate = if files_per_second >= 1000.0 {
                format!("{:.1}k files/s", files_per_second / 1000.0)
            } else if files_per_second > 0.0 {
                format!("{files_per_second:.0} files/s")
            } else {
                "starting".to_string()
            };

            let path = if progress.current_path.as_os_str().is_empty() {
                self.nav.view_root.display().to_string()
            } else {
                progress.current_path.display().to_string()
            };

            let prefix = format!(
                " Scan in background | {} items | {} | {} | {} elapsed | ",
                progress.total_items(),
                format_size(progress.bytes_scanned),
                rate,
                format_duration(progress.elapsed),
            );
            let path_width = width.saturating_sub(prefix.width() + 6);

            return Some(Line::from(vec![
                Span::styled(" SCAN ", self.theme.help_key_style()),
                Span::styled(prefix, self.theme.help_desc_style()),
                Span::styled(
                    fit_to_width(&path, path_width),
                    Style::default().fg(self.theme.muted),
                ),
            ]));
        }

        if self.analyzing {
            return Some(Line::from(vec![
                Span::styled(" ANALYZE ", self.theme.help_key_style()),
                Span::styled(
                    " Building duplicate and age reports in the background; Explorer remains usable.",
                    self.theme.help_desc_style(),
                ),
            ]));
        }

        if !self.has_full_scan {
            return Some(Line::from(vec![
                Span::styled(" QUICK ", self.theme.help_key_style()),
                Span::styled(
                    " Directory sizes are not fully measured. Press R for full disk usage, duplicates, age, and treemap accuracy.",
                    self.theme.help_desc_style(),
                ),
            ]));
        }

        None
    }

    fn render_overlays(&self, area: Rect, footer: Rect, buf: &mut Buffer) {
        match self.mode {
            AppMode::Help => {
                HelpOverlay::new(&self.theme, &self.keymap, self.help_scroll)
                    .max_scroll(&self.help_max_scroll)
                    .render(area, buf);
            }
            AppMode::ConfirmDelete => {
                let modal =
                    DeleteConfirmModal::new(&self.theme, &self.marked, |p| self.get_path_size(p));
                modal.render(area, buf);
            }
            AppMode::Deleting => {
                DeletionProgressModal::new(&self.theme, self.deletion_progress.as_ref())
                    .render(area, buf);
            }
            AppMode::Command => {
                CommandPalette::new(
                    &self.theme,
                    self.command_input.buffer(),
                    self.command_input.cursor(),
                )
                .render(footer, buf);
            }
            AppMode::Renaming => {
                if let Some(input) = &self.input_state {
                    InputModal::new(&self.theme, input, "Rename", "Enter new name:")
                        .render(area, buf);
                }
            }
            AppMode::CreatingFile => {
                if let Some(input) = &self.input_state {
                    InputModal::new(&self.theme, input, "Create File", "Enter file name:")
                        .render(area, buf);
                }
            }
            AppMode::CreatingDirectory => {
                if let Some(input) = &self.input_state {
                    InputModal::new(
                        &self.theme,
                        input,
                        "Create Directory",
                        "Enter directory name:",
                    )
                    .render(area, buf);
                }
            }
            AppMode::Taking => {
                if let Some(input) = &self.input_state {
                    InputModal::new(
                        &self.theme,
                        input,
                        "Take (mkdir + cd)",
                        "Enter directory name:",
                    )
                    .render(area, buf);
                }
            }
            AppMode::GoingTo => {
                if let Some(input) = &self.input_state {
                    InputModal::new(&self.theme, input, "Go To Directory", "Enter path:")
                        .render(area, buf);
                }
            }
            AppMode::Copying | AppMode::Moving | AppMode::Undoing | AppMode::UpdatingEntry => {
                if let Some(progress) = &self.operation_progress {
                    OperationProgressModal::new(&self.theme, progress).render(area, buf);
                } else {
                    let width = 55.min(area.width.saturating_sub(4));
                    let height = 7.min(area.height.saturating_sub(4));
                    let popup = Rect::new(
                        area.x + area.width.saturating_sub(width) / 2,
                        area.y + area.height.saturating_sub(height) / 2,
                        width,
                        height,
                    );
                    Clear.render(popup, buf);
                    let block = Block::default()
                        .title(" Operation in progress ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(self.theme.info));
                    let inner = block.inner(popup);
                    block.render(popup, buf);
                    let message = self
                        .operation_message
                        .as_ref()
                        .map(|(_, message)| message.as_str())
                        .unwrap_or("Waiting for worker progress...");
                    Paragraph::new(vec![
                        Line::raw(""),
                        Line::raw(fit_left_to_width(message, inner.width as usize)),
                    ])
                    .style(Style::default().fg(self.theme.info))
                    .render(inner, buf);
                }
            }
            AppMode::ConflictResolution => {
                if let Some(conflict) = &self.pending_conflict {
                    ConflictModal::new(&self.theme, conflict.conflict()).render(area, buf);
                }
            }
            AppMode::Search => {
                self.render_search_overlay(area, buf);
            }
            AppMode::Settings => {
                if let Some(state) = &self.settings_state {
                    SettingsModal::new(&self.theme, state).render(area, buf);
                }
            }
            AppMode::BookmarkList => {
                if let Some(state) = &self.bookmark_list_state {
                    BookmarkListModal::new(&self.theme, &self.user_settings.bookmarks, state)
                        .render(area, buf);
                }
            }
            AppMode::SettingBookmark => {
                let footer_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
                BookmarkPrompt::new(&self.theme, "Set bookmark:").render(footer_area, buf);
            }
            AppMode::JumpingToBookmark => {
                let footer_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
                BookmarkPrompt::new(&self.theme, "Jump to bookmark:").render(footer_area, buf);
            }
            AppMode::ConfirmBulkRename => {
                if let Some(state) = &self.bulk_rename_state {
                    BulkRenameConfirmModal::new(&self.theme, state).render(area, buf);
                }
            }
            _ => {}
        }
    }

    fn render_dir_tabs(&self, area: Rect, buf: &mut Buffer) {
        let labels = self.tab_manager.labels(&self.nav);
        let active = self.tab_manager.active_index();
        let max_tab_width = (area.width as usize / labels.len().max(1)).clamp(8, 20);

        let titles: Vec<String> = labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let prefix = if i < 9 {
                    format!("{}:", i + 1)
                } else {
                    String::new()
                };
                let label =
                    crate::ui::fit_to_width(label, max_tab_width.saturating_sub(prefix.len() + 2));
                format!(" {}{} ", prefix, label)
            })
            .collect();

        Tabs::new(titles)
            .select(active)
            .style(Style::default().fg(self.theme.muted))
            .highlight_style(self.theme.selected.add_modifier(Modifier::BOLD))
            .divider("|")
            .render(area, buf);
    }

    fn render_tabs(&self, area: Rect, buf: &mut Buffer) {
        let is_scanning = self.scan_progress.is_some();
        let error_count = self
            .warnings
            .iter()
            .fold(0usize, |count, warning| {
                count.saturating_add(
                    usize::try_from(warning.represented_count()).unwrap_or(usize::MAX),
                )
            })
            .max(
                self.scan_progress
                    .as_ref()
                    .map(|p| usize::try_from(p.errors_count).unwrap_or(usize::MAX))
                    .unwrap_or(0),
            )
            .saturating_add(self.operation_warnings.len());

        let titles: Vec<String> = View::iter()
            .map(|v| {
                if v == View::Errors && error_count > 0 {
                    format!(" {} ({}) ", v, error_count)
                } else if self.analyzing && matches!(v, View::Duplicates | View::Age) {
                    format!(" {} ⟳ ", v)
                } else if v == View::Explorer && is_scanning {
                    " Explorer ⟳ ".to_string()
                } else {
                    format!(" {} ", v)
                }
            })
            .collect();

        Tabs::new(titles)
            .select(self.view as usize)
            .style(self.theme.footer)
            .highlight_style(self.theme.selected)
            .render(area, buf);
    }

    fn render_explorer(&self, area: Rect, buf: &mut Buffer) {
        let layout = AppLayout::new(area, self.show_details);

        if let Some((view_node, view_path)) = self.get_view_root_node() {
            match self.layout_mode {
                LayoutMode::Tree => {
                    let title = if self.nav.view_root != self.path {
                        let relative = self
                            .nav
                            .view_root
                            .strip_prefix(&self.path)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| self.nav.view_root.display().to_string());
                        format!(" {} (\u{2190} Backspace) ", relative)
                    } else {
                        format!(" {} ", view_path.display())
                    };

                    let tree_view = TreeView::new(
                        view_node,
                        &view_path,
                        &self.theme,
                        &self.marked,
                        &self.clipboard,
                    )
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(self.theme.border)
                            .title(title)
                            .title_style(self.theme.title),
                    );

                    let mut tree_state = self.nav.tree_state.clone();
                    ratatui::widgets::StatefulWidget::render(
                        tree_view,
                        layout.main,
                        buf,
                        &mut tree_state,
                    );
                    let (_, miller) = self.rendered_offsets.get();
                    self.rendered_offsets.set((tree_state.offset, miller));
                }
                LayoutMode::Miller => {
                    // Record the preview column's inner size so the next image
                    // preview is encoded at the size it will actually occupy.
                    // Mirrors the 40% third column in `MillerColumns`, less the
                    // block border and the one-line image header.
                    let preview_width = layout.main.width * 40 / 100;
                    self.preview_area.set(ratatui::layout::Size::new(
                        preview_width.saturating_sub(2).max(1),
                        layout.main.height.saturating_sub(3).max(1),
                    ));

                    let parent_node = self.get_parent_node();
                    let current_name = self.get_current_dir_name().unwrap_or_default();

                    let miller = MillerColumns::new(
                        view_node,
                        parent_node,
                        &current_name,
                        &view_path,
                        &self.marked,
                        &self.clipboard,
                        &self.theme,
                    )
                    .file_preview(&self.preview_state.content)
                    .preview_mode(self.preview_state.mode);

                    let mut miller_state = self.nav.miller_state.clone();
                    ratatui::widgets::StatefulWidget::render(
                        miller,
                        layout.main,
                        buf,
                        &mut miller_state,
                    );
                    let (tree, _) = self.rendered_offsets.get();
                    self.rendered_offsets.set((tree, miller_state.offset));
                }
            }
        } else if let Some(error) = &self.error {
            let error_block = Block::default()
                .borders(Borders::ALL)
                .border_style(self.theme.border)
                .title(" Error ")
                .title_style(Style::default().fg(self.theme.error));

            Paragraph::new(error.as_str())
                .block(error_block)
                .style(Style::default().fg(self.theme.error))
                .render(layout.main, buf);
        } else {
            let placeholder_block = Block::default()
                .borders(Borders::ALL)
                .border_style(self.theme.border)
                .title(format!(" {} ", self.path.display()))
                .title_style(self.theme.title);

            Paragraph::new(vec![
                Line::raw(""),
                Line::styled(
                    "  No data available. Press 'r' to rescan.",
                    Style::default().fg(self.theme.muted),
                ),
            ])
            .block(placeholder_block)
            .render(layout.main, buf);
        }

        if let Some(details_area) = layout.details {
            self.render_details(details_area, buf);
        }
    }

    fn render_duplicates(&self, area: Rect, buf: &mut Buffer) {
        let title = if self.nav.view_root != self.path {
            let relative = self
                .nav
                .view_root
                .strip_prefix(&self.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| self.nav.view_root.display().to_string());
            format!(" Duplicates in {} ", relative)
        } else {
            " Duplicates ".to_string()
        };

        let title = if let Some(report) = &self.duplicates
            && !report.status.is_complete()
        {
            format!("{title}[{}] ", report.status.state)
        } else {
            title
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if let Some((filtered_groups, total_wasted)) = self.get_filtered_duplicates() {
            if filtered_groups.is_empty() {
                let message = if self
                    .duplicates
                    .as_ref()
                    .is_some_and(|report| !report.status.is_complete())
                {
                    "Analysis incomplete. No matches in the files successfully checked."
                } else if self.has_full_scan {
                    "No duplicate files found in this directory."
                } else {
                    "No duplicate data yet.\n\nPress R to run a full scan."
                };
                Paragraph::new(message)
                    .style(Style::default().fg(self.theme.muted))
                    .render(inner, buf);
                return;
            }

            let marked_count = self.marked.len();
            let header = if marked_count > 0 {
                format!(
                    " {} groups | {} reclaimable | {} marked for deletion",
                    filtered_groups.len(),
                    format_size(total_wasted),
                    marked_count
                )
            } else {
                format!(
                    " {} groups | {} reclaimable",
                    filtered_groups.len(),
                    format_size(total_wasted)
                )
            };
            let header_line = Line::styled(header, self.theme.title);
            let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
            Paragraph::new(header_line).render(header_area, buf);

            let list_area = Rect::new(
                inner.x,
                inner.y + 2,
                inner.width,
                inner.height.saturating_sub(2),
            );
            let visible_height = list_area.height as usize;

            let layout = filtered_groups.iter().enumerate().map(|(index, group)| {
                let count = 1 + if self.duplicates_state.is_expanded(index) {
                    group.paths.len()
                } else {
                    0
                };
                let selected = (index == self.duplicates_state.selected_group)
                    .then(|| self.duplicates_state.selected_item(index));
                (count, selected)
            });
            let (rows, scroll_offset, total_rows) = duplicate_viewport(layout, visible_height);
            let max_wasted = filtered_groups
                .iter()
                .map(|group| group.wasted_bytes)
                .max()
                .unwrap_or(1)
                .max(1);
            for (render_idx, (group_idx, item_idx)) in rows.into_iter().enumerate() {
                let group = filtered_groups[group_idx];
                let is_selected = group_idx == self.duplicates_state.selected_group
                    && item_idx == self.duplicates_state.selected_item(group_idx);
                let item = if item_idx == 0 {
                    DuplicateListItem::GroupHeader {
                        group,
                        files_in_view: self.filtered_duplicate_path_count(group_idx),
                        is_expanded: self.duplicates_state.is_expanded(group_idx),
                        is_selected,
                    }
                } else {
                    let path = &group.paths[item_idx - 1];
                    DuplicateListItem::File {
                        file_idx: item_idx - 1,
                        path,
                        is_selected,
                        is_marked: self.marked.contains(path),
                    }
                };
                let item = &item;
                let y = list_area.y + render_idx as u16;
                let line_area = Rect::new(list_area.x, y, list_area.width, 1);

                match item {
                    DuplicateListItem::GroupHeader {
                        group,
                        files_in_view,
                        is_expanded,
                        is_selected,
                        ..
                    } => {
                        let expand_icon = if *is_expanded { "▼" } else { "▶" };
                        let files_in_view = *files_in_view;
                        let total_files = group.count();

                        let file_info = if files_in_view < total_files {
                            format!("{}/{}", files_in_view, total_files)
                        } else {
                            format!("{}", total_files)
                        };

                        let heat_ratio = group.wasted_bytes as f64 / max_wasted as f64;

                        let bar_width = 8;
                        let filled =
                            ((heat_ratio * bar_width as f64).round() as usize).min(bar_width);
                        let mut heat_bar = "█".repeat(filled);
                        heat_bar.push_str(&"░".repeat(bar_width - filled));
                        let heat_color = self.theme.size_color(heat_ratio);

                        let expand_style = if *is_selected {
                            self.theme.selected
                        } else {
                            Style::default()
                        };
                        let line = Line::from(vec![
                            Span::styled(format!(" {} ", expand_icon), expand_style),
                            Span::styled(heat_bar, Style::default().fg(heat_color)),
                            Span::styled(
                                format!(
                                    " {} × {} = {}",
                                    file_info,
                                    format_size(group.size),
                                    format_size(group.wasted_bytes)
                                ),
                                if *is_selected {
                                    self.theme.selected
                                } else {
                                    Style::default().fg(self.theme.warning)
                                },
                            ),
                        ]);

                        Paragraph::new(line).render(line_area, buf);
                    }
                    DuplicateListItem::File {
                        file_idx,
                        path,
                        is_selected,
                        is_marked,
                        ..
                    } => {
                        let prefix = if *file_idx == 0 { "keep" } else { " dup" };
                        let mark = if *is_marked { "●" } else { " " };
                        let display_path = path
                            .strip_prefix(&self.nav.view_root)
                            .unwrap_or(path)
                            .display()
                            .to_string();

                        let line = format!("   {} {} {}", prefix, mark, display_path);

                        let style = if *is_selected {
                            self.theme.selected
                        } else if *is_marked {
                            Style::default().fg(self.theme.error)
                        } else if *file_idx == 0 {
                            Style::default().fg(self.theme.success)
                        } else {
                            Style::default().fg(self.theme.muted)
                        };

                        Paragraph::new(Line::styled(line, style)).render(line_area, buf);
                    }
                }
            }

            if total_rows > visible_height && visible_height > 0 {
                let total = total_rows;
                let hint = format!(" ({}/{})", scroll_offset + 1, total);
                let hint_x = list_area.x + list_area.width.saturating_sub(hint.len() as u16 + 1);
                if hint_x > list_area.x {
                    let hint_area = Rect::new(
                        hint_x,
                        list_area.y + visible_height as u16 - 1,
                        hint.len() as u16,
                        1,
                    );
                    if hint_area.y < area.y + area.height - 1 {
                        Paragraph::new(Line::styled(hint, Style::default().fg(self.theme.muted)))
                            .render(hint_area, buf);
                    }
                }
            }
        } else {
            let lines = if self.scan_progress.is_some() {
                vec![
                    Line::styled(
                        "Full scan is running before duplicate analysis starts.",
                        Style::default().fg(self.theme.muted),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Explorer remains usable while scanning.",
                        Style::default().fg(self.theme.muted),
                    ),
                ]
            } else if self.analyzing {
                vec![
                    Line::styled(
                        "Analyzing duplicate files...",
                        Style::default().fg(self.theme.muted),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Large trees may take a moment because content hashes are compared.",
                        Style::default().fg(self.theme.muted),
                    ),
                ]
            } else if !self.has_full_scan {
                vec![
                    Line::styled(
                        "Duplicate analysis needs a full recursive scan.",
                        Style::default().fg(self.theme.muted),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Press R to scan this directory.",
                        Style::default().fg(self.theme.muted),
                    ),
                ]
            } else {
                vec![
                    Line::styled(
                        "Duplicate analysis is not running.",
                        Style::default().fg(self.theme.muted),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Run :analyze to start duplicate and age analysis for this scan.",
                        Style::default().fg(self.theme.muted),
                    ),
                ]
            };

            Paragraph::new(lines).render(inner, buf);
        }
    }

    fn render_age(&self, area: Rect, buf: &mut Buffer) {
        let title = if self.nav.view_root != self.path {
            let relative = self
                .nav
                .view_root
                .strip_prefix(&self.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| self.nav.view_root.display().to_string());
            format!(" Age Analysis - {} ", relative)
        } else {
            " Age Analysis ".to_string()
        };

        let title = if let Some(report) = &self.age_report
            && !report.status.is_complete()
        {
            format!("{title}[{}] ", report.status.state)
        } else {
            title
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if let Some(age) = &self.age_report {
            let max_size = age.buckets.iter().map(|b| b.total_size).max().unwrap_or(1);
            let chart_height = age.buckets.len().min(inner.height as usize / 2);

            let bucket_start_y = if self.nav.view_root != self.path {
                Line::styled(
                    " Distribution (full scan):",
                    Style::default().fg(self.theme.muted),
                )
                .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
                inner.y + 1
            } else {
                inner.y
            };

            for (i, bucket) in age.buckets.iter().enumerate().take(chart_height) {
                let y = bucket_start_y + i as u16;
                if y >= inner.y + inner.height {
                    break;
                }
                let bar_width = if max_size > 0 {
                    ((bucket.total_size as f64 / max_size as f64) * 20.0) as usize
                } else {
                    0
                };

                let bar = "\u{2588}".repeat(bar_width);
                let line = format!(
                    " {:<12} {:>10} {:>8} files  {}",
                    bucket.name,
                    format_size(bucket.total_size),
                    bucket.file_count,
                    bar
                );

                Paragraph::new(line).render(Rect::new(inner.x, y, inner.width, 1), buf);
            }

            if let Some(stale_dirs) = self.get_filtered_stale_dirs() {
                let stale_y = bucket_start_y + chart_height as u16 + 1;
                if stale_y < inner.y + inner.height {
                    let total_stale_size: u64 = stale_dirs
                        .iter()
                        .map(|d| d.size)
                        .fold(0u64, u64::saturating_add);
                    let stale_header = if age.stale_directory_status
                        != gravityfile_analyze::StaleDirectoryStatus::Evaluated
                    {
                        format!(" Stale directories {}", age.stale_directory_status)
                    } else if stale_dirs.is_empty() {
                        " No stale directories found.".to_string()
                    } else {
                        format!(
                            " Stale Directories ({}, {} total)",
                            stale_dirs.len(),
                            format_size(total_stale_size)
                        )
                    };
                    Paragraph::new(Line::styled(stale_header, self.theme.title))
                        .render(Rect::new(inner.x, stale_y, inner.width, 1), buf);

                    let list_y = stale_y + 1;
                    let list_height = (inner.y + inner.height).saturating_sub(list_y) as usize;

                    let selected = self
                        .selected_stale_dir
                        .min(stale_dirs.len().saturating_sub(1));

                    for (i, dir) in stale_dirs.iter().enumerate().take(list_height) {
                        let y = list_y + i as u16;
                        let is_selected = i == selected;

                        let line = format!(
                            "   {} ({}, {} old)",
                            dir.path
                                .file_name()
                                .map(|n| n.to_string_lossy())
                                .unwrap_or_default(),
                            format_size(dir.size),
                            format_age(dir.newest_file_age)
                        );

                        let style = if is_selected {
                            self.theme.selected
                        } else {
                            Style::default()
                        };

                        Paragraph::new(Line::styled(line, style))
                            .render(Rect::new(inner.x, y, inner.width, 1), buf);
                    }
                }
            }
        } else {
            let lines = if self.scan_progress.is_some() {
                vec![
                    Line::styled(
                        "Full scan is running before age analysis starts.",
                        Style::default().fg(self.theme.muted),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Age buckets and stale-directory candidates appear after analysis.",
                        Style::default().fg(self.theme.muted),
                    ),
                ]
            } else if self.analyzing {
                vec![Line::styled(
                    "Analyzing file ages and stale directories...",
                    Style::default().fg(self.theme.muted),
                )]
            } else if !self.has_full_scan {
                vec![
                    Line::styled(
                        "Age analysis needs a full recursive scan.",
                        Style::default().fg(self.theme.muted),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Press R to scan this directory.",
                        Style::default().fg(self.theme.muted),
                    ),
                ]
            } else {
                vec![
                    Line::styled(
                        "Age analysis is not running.",
                        Style::default().fg(self.theme.muted),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Run :analyze to start duplicate and age analysis for this scan.",
                        Style::default().fg(self.theme.muted),
                    ),
                ]
            };

            Paragraph::new(lines).render(inner, buf);
        }
    }

    fn render_errors(&self, area: Rect, buf: &mut Buffer) {
        let is_scanning = self.scan_progress.is_some();

        let title = if is_scanning {
            let count = self
                .scan_progress
                .as_ref()
                .map(|p| p.errors_count)
                .unwrap_or(0);
            format!(
                " Errors & Warnings ({}) ",
                usize::try_from(count)
                    .unwrap_or(usize::MAX)
                    .saturating_add(self.operation_warnings.len())
            )
        } else if self.warning_count() > 0 {
            format!(
                " Errors & Warnings ({} retained records) ",
                self.warning_count()
            )
        } else {
            " Errors & Warnings ".to_string()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if self.warning_count() == 0 {
            let lines = if is_scanning {
                let error_count = self
                    .scan_progress
                    .as_ref()
                    .map(|p| p.errors_count)
                    .unwrap_or(0);
                if error_count > 0 {
                    vec![
                        Line::raw(""),
                        Line::styled(
                            format!("  {} errors encountered during scan", error_count),
                            Style::default().fg(self.theme.warning),
                        ),
                        Line::raw(""),
                        Line::styled(
                            "  Error details will be available after scan completes.",
                            Style::default().fg(self.theme.muted),
                        ),
                    ]
                } else {
                    vec![Line::styled(
                        "No errors yet...",
                        Style::default().fg(self.theme.muted),
                    )]
                }
            } else {
                vec![Line::styled(
                    "No errors or warnings during scan.",
                    Style::default().fg(self.theme.muted),
                )]
            };
            Paragraph::new(lines).render(inner, buf);
            return;
        }

        let lines_per_item = 2;
        let visible_items = inner.height as usize / lines_per_item;

        let scroll_offset = if self.selected_warning >= visible_items {
            self.selected_warning - visible_items + 1
        } else {
            0
        };

        for (i, warning) in self
            .operation_warnings
            .iter()
            .chain(self.warnings.iter())
            .enumerate()
            .skip(scroll_offset)
            .take(visible_items)
        {
            let base_y = inner.y + ((i - scroll_offset) * lines_per_item) as u16;
            let is_selected = i == self.selected_warning;

            let (icon, kind_label, can_delete) = if i < self.operation_warnings.len() {
                ("!", "Operation", false)
            } else {
                match warning.kind {
                    gravityfile_core::WarningKind::PermissionDenied => {
                        ("\u{1F512}", "Permission Denied", false)
                    }
                    gravityfile_core::WarningKind::BrokenSymlink => {
                        ("\u{1F517}", "Broken Symlink", true)
                    }
                    gravityfile_core::WarningKind::ReadError => ("\u{26A0}", "Read Error", false),
                    gravityfile_core::WarningKind::MetadataError => {
                        ("\u{1F4CB}", "Metadata Error", false)
                    }
                    gravityfile_core::WarningKind::CrossFilesystem => {
                        ("\u{1F4BE}", "Cross Filesystem", false)
                    }
                    gravityfile_core::WarningKind::Omitted { .. } => {
                        ("!", "Omitted Warnings", false)
                    }
                }
            };
            let is_marked = self.marked.contains(&warning.path);
            let path_str = warning.path.display().to_string();
            let mark_indicator = if is_marked { "[x] " } else { "    " };
            let delete_hint = if can_delete && is_selected {
                " [d to delete]"
            } else {
                ""
            };
            let prefix = format!("{}{} {} ", mark_indicator, icon, kind_label);
            let suffix_len = delete_hint.len();
            let available_width =
                (inner.width as usize).saturating_sub(prefix.len() + suffix_len + 1);
            let display_path = fit_left_to_width(&path_str, available_width);

            let style = if is_selected {
                self.theme.selected
            } else if is_marked {
                Style::default().fg(self.theme.error)
            } else {
                Style::default().fg(self.theme.warning)
            };

            Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(display_path, style),
                Span::styled(delete_hint, Style::default().fg(self.theme.muted)),
            ])
            .render(Rect::new(inner.x, base_y, inner.width, 1), buf);

            if base_y + 1 < inner.y + inner.height {
                let msg_style = if is_selected {
                    self.theme.selected
                } else {
                    Style::default().fg(self.theme.muted)
                };

                let max_msg_len = (inner.width as usize).saturating_sub(4);
                let msg = fit_to_width(&warning.message, max_msg_len);

                Line::styled(format!("    {}", msg), msg_style)
                    .render(Rect::new(inner.x, base_y + 1, inner.width, 1), buf);
            }
        }
    }

    fn render_treemap(&self, area: Rect, buf: &mut Buffer) {
        let Some(tree) = &self.tree else {
            Block::default()
                .borders(Borders::ALL)
                .border_style(self.theme.border)
                .title(" Treemap ")
                .title_style(self.theme.title)
                .render(area, buf);
            return;
        };

        let (view_node, view_path) = self
            .get_view_root_node()
            .unwrap_or((&tree.root, tree.root_path.clone()));

        let title_suffix = if self.scan_progress.is_some() {
            " (scanning)"
        } else if !self.has_full_scan {
            " (quick sizes)"
        } else {
            ""
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(format!(
                " Treemap: {}{} ",
                view_path.display(),
                title_suffix
            ))
            .title_style(self.theme.title);

        TreemapView::new(
            view_node,
            &view_path,
            &self.theme,
            &self.marked,
            &self.clipboard,
        )
        .block(block)
        .selected(self.nav.treemap_state.selected)
        .render(area, buf);
    }

    fn render_details(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(" Details ")
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if let Some(info) = self.get_selected_info() {
            let mut lines = vec![
                Line::from(Span::styled(
                    &info.name,
                    self.theme.title.add_modifier(Modifier::BOLD),
                )),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("Size: ", self.theme.help_desc),
                    Span::raw(format_size(info.size)),
                ]),
            ];

            if info.is_dir {
                lines.push(Line::from(vec![
                    Span::styled("Files: ", self.theme.help_desc),
                    Span::raw(info.file_count.to_string()),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("Dirs: ", self.theme.help_desc),
                    Span::raw(info.dir_count.to_string()),
                ]));
            }

            lines.push(Line::from(vec![
                Span::styled("Modified: ", self.theme.help_desc),
                Span::raw(format_relative_time(info.modified)),
            ]));

            lines.push(Line::raw(""));
            lines.push(Line::styled("Path:", self.theme.help_desc));

            let path_str = info.path.display().to_string();
            let max_width = inner.width.saturating_sub(2) as usize;
            for chunk in path_str
                .chars()
                .collect::<Vec<_>>()
                .chunks(max_width)
                .map(|c| c.iter().collect::<String>())
            {
                lines.push(Line::raw(chunk));
            }

            Paragraph::new(lines).render(inner, buf);
        }
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let in_visual_mode = self.mode == AppMode::Visual;

        // Keys come from the keymap, so rebinding one updates its hint, and an
        // unbound action drops out. Visual mode reads its keys directly.
        let hint = |actions: &[KeyAction]| self.keymap.hint(actions);
        let up_down = hint(&[KeyAction::MoveDown, KeyAction::MoveUp]);
        let mark = hint(&[KeyAction::ToggleMark]);
        let delete = hint(&[KeyAction::Delete]);
        let cancel = hint(&[KeyAction::Cancel]);
        let mut keys: Vec<(String, &str)> = if in_visual_mode {
            [
                ("V", "VISUAL"),
                ("j/k", "Extend"),
                ("Enter", "Apply"),
                ("Esc", "Cancel"),
                ("y", "Yank"),
                ("d", "Delete"),
            ]
            .map(|(key, label)| (key.to_string(), label))
            .to_vec()
        } else {
            match self.view {
                View::Explorer => {
                    let mut v = vec![
                        (up_down, "Nav"),
                        (hint(&[KeyAction::Yank]), "Copy"),
                        (hint(&[KeyAction::Cut]), "Cut"),
                        (delete, "Del"),
                    ];
                    if !self.clipboard.is_empty() {
                        v.push((hint(&[KeyAction::Paste]), "Paste"));
                        v.push((cancel, "Unclip"));
                    } else if !self.marked.is_empty() {
                        v.push((cancel, "Unmark"));
                    } else {
                        v.push((mark, "+Sel"));
                    }
                    v.push((hint(&[KeyAction::EnterVisual]), "Visual"));
                    v.push((
                        hint(&[KeyAction::ToggleLayout]),
                        match self.layout_mode {
                            LayoutMode::Tree => "Miller",
                            LayoutMode::Miller => "Tree",
                        },
                    ));
                    v.push((hint(&[KeyAction::Sort]), self.sort_mode.short_label()));
                    v
                }
                View::Duplicates => {
                    let is_on_header = !self
                        .duplicates_state
                        .is_expanded(self.duplicates_state.selected_group)
                        || self
                            .duplicates_state
                            .selected_item(self.duplicates_state.selected_group)
                            == 0;
                    let mut v = vec![
                        (up_down, "Nav"),
                        (hint(&[KeyAction::MoveLeft, KeyAction::MoveRight]), "±Grp"),
                    ];
                    if is_on_header {
                        v.push((delete, "Del Dups"));
                        v.push((mark, "Sel All"));
                    } else {
                        v.push((delete, "Del File"));
                        v.push((mark, "Toggle"));
                    }
                    if !self.marked.is_empty() {
                        v.push((cancel, "Clear"));
                    }
                    if self.has_full_scan && self.duplicates.is_none() && !self.analyzing {
                        v.push((":analyze".to_string(), "Run"));
                    }
                    v
                }
                View::Age => {
                    let mut v = vec![
                        (up_down, "Nav"),
                        (hint(&[KeyAction::Yank]), "Copy"),
                        (delete, "Del"),
                    ];
                    if !self.clipboard.is_empty() {
                        v.push((cancel, "Unclip"));
                    } else if !self.marked.is_empty() {
                        v.push((cancel, "Unmark"));
                    } else {
                        v.push((mark, "+Sel"));
                    }
                    if self.has_full_scan && self.age_report.is_none() && !self.analyzing {
                        v.push((":analyze".to_string(), "Run"));
                    }
                    v
                }
                View::Errors => {
                    let mut v = vec![(up_down, "Nav"), (mark, "+Sel"), (delete, "Del")];
                    if !self.marked.is_empty() {
                        v.push((cancel, "Clear"));
                    }
                    v
                }
                View::Treemap => {
                    let mut v = vec![
                        (up_down, "Select"),
                        (hint(&[KeyAction::DrillDown]), "Drill"),
                        (hint(&[KeyAction::NavigateBack]), "Back"),
                        (mark, "+Sel"),
                        (delete, "Del"),
                    ];
                    if !self.marked.is_empty() {
                        v.push((cancel, "Clear"));
                    }
                    v
                }
            }
        };

        if (self.scan_progress.is_some() || self.analyzing)
            && self.clipboard.is_empty()
            && self.marked.is_empty()
        {
            let label = if self.scan_progress.is_some() {
                "Cancel scan"
            } else {
                "Cancel analysis"
            };
            keys.insert(0, (hint(&[KeyAction::Cancel]), label));
        }

        if self.scan_progress.is_none() && !self.analyzing {
            if !self.has_full_scan {
                keys.insert(0, (hint(&[KeyAction::Refresh]), "Scan"));
            } else {
                keys.push((hint(&[KeyAction::Refresh]), "Rescan"));
            }
        }

        keys.extend([
            (hint(&[KeyAction::NextView]), "Views"),
            (hint(&[KeyAction::ToggleHelp]), "Help"),
            (hint(&[KeyAction::Quit]), "Quit"),
        ]);
        keys.retain(|(key, _)| !key.is_empty());

        let spans: Vec<Span> = keys
            .iter()
            .flat_map(|(key, desc)| {
                vec![
                    Span::styled(format!(" {} ", key), self.theme.help_key_style()),
                    Span::styled(format!("{} ", desc), self.theme.help_desc_style()),
                ]
            })
            .collect();

        Paragraph::new(Line::from(spans))
            .style(self.theme.footer_style())
            .render(area, buf);
    }

    fn render_search_overlay(&self, area: Rect, buf: &mut Buffer) {
        use ratatui::widgets::Clear;

        let popup_width = area.width.clamp(30, 60);
        let popup_height = area.height.clamp(10, 20);
        let x = (area.width.saturating_sub(popup_width)) / 2;
        let y = (area.height.saturating_sub(popup_height)) / 3;

        let popup_area = Rect::new(area.x + x, area.y + y, popup_width, popup_height);
        Clear.render(popup_area, buf);

        let title = format!(" Search ({}) ", self.search_state.mode.label());
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .style(
                Style::default()
                    .bg(self.theme.background)
                    .fg(self.theme.foreground),
            )
            .border_style(self.theme.border);
        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let [input_area, sep_area, results_area] = inner.layout(&Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ]));

        let query = &self.search_state.query;
        let cursor = self.search_state.cursor_byte_offset();
        let mut spans = vec![Span::styled("/", self.theme.help_key)];

        if query.is_empty() {
            spans.push(Span::styled(
                "_",
                Style::default().add_modifier(Modifier::SLOW_BLINK),
            ));
        } else {
            let (before_cursor, at_cursor, after_cursor) = crate::ui::visible_input_parts(
                query,
                cursor,
                input_area.width.saturating_sub(1) as usize,
            );
            spans.push(Span::raw(before_cursor));
            spans.push(Span::styled(
                at_cursor,
                Style::default().add_modifier(Modifier::REVERSED),
            ));
            spans.push(Span::raw(after_cursor));
        }

        Paragraph::new(Line::from(spans)).render(input_area, buf);

        Line::from(vec![
            Span::styled("Tab", self.theme.help_key),
            Span::styled(":mode ", self.theme.help_desc),
            Span::styled("↑↓", self.theme.help_key),
            Span::styled(":nav ", self.theme.help_desc),
            Span::styled("Enter", self.theme.help_key),
            Span::styled(":go ", self.theme.help_desc),
            Span::styled("Esc", self.theme.help_key),
            Span::styled(":cancel", self.theme.help_desc),
        ])
        .style(Style::default().fg(self.theme.muted))
        .render(sep_area, buf);

        let results = &self.search_state.results;
        let selected = self.search_state.selected;

        if let Some(error) = &self.search_state.error {
            Paragraph::new(error.as_str())
                .style(Style::default().fg(self.theme.error))
                .wrap(ratatui::widgets::Wrap { trim: false })
                .render(results_area, buf);
        } else if self.search_state.is_searching() {
            Paragraph::new("Searching…")
                .style(Style::default().fg(self.theme.muted))
                .render(results_area, buf);
        } else if results.is_empty() {
            if !query.is_empty() {
                Paragraph::new("No matches found")
                    .style(Style::default().fg(self.theme.muted))
                    .render(results_area, buf);
            }
        } else {
            let max_visible = results_area.height as usize;
            let offset = if selected >= max_visible {
                selected - max_visible + 1
            } else {
                0
            };

            for (i, result) in results.iter().skip(offset).take(max_visible).enumerate() {
                let row = i as u16;
                let is_selected = i + offset == selected;
                let base_style = if is_selected {
                    self.theme.selected
                } else if result.is_dir {
                    self.theme.directory
                } else {
                    Style::default().fg(self.theme.foreground)
                };
                let prefix = if result.is_dir { " " } else { "   " };
                let match_style = if is_selected {
                    base_style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default()
                        .fg(self.theme.warning)
                        .add_modifier(Modifier::BOLD)
                };

                let mut spans: Vec<Span> = vec![Span::styled(prefix, base_style)];
                for (char_pos, ch) in result.display.chars().enumerate() {
                    let is_match = result.matched_indices.contains(&char_pos);
                    spans.push(Span::styled(
                        ch.to_string(),
                        if is_match { match_style } else { base_style },
                    ));
                }

                Line::from(spans).render(
                    Rect::new(results_area.x, results_area.y + row, results_area.width, 1),
                    buf,
                );
            }

            if results.len() > max_visible {
                let count_str = format!("[{}/{}]", selected + 1, results.len());
                Line::from(count_str)
                    .style(Style::default().fg(self.theme.muted))
                    .render(
                        Rect::new(
                            results_area.x + results_area.width.saturating_sub(10),
                            results_area.y + results_area.height - 1,
                            10,
                            1,
                        ),
                        buf,
                    );
            }
        }
    }
}

#[cfg(test)]
mod duplicate_viewport_tests {
    use super::duplicate_viewport;

    #[test]
    fn viewport_matches_flattened_reference_at_group_boundaries() {
        for selected_group in 0..3 {
            for selected_item in 0..4 {
                for height in 0..10 {
                    let counts = [3, 1, 4];
                    let groups: Vec<_> = counts
                        .iter()
                        .enumerate()
                        .map(|(group, count)| {
                            (*count, (group == selected_group).then_some(selected_item))
                        })
                        .collect();
                    let flat: Vec<_> = counts
                        .iter()
                        .enumerate()
                        .flat_map(|(group, count)| (0..*count).map(move |item| (group, item)))
                        .collect();
                    let selection = flat
                        .iter()
                        .position(|row| *row == (selected_group, selected_item))
                        .unwrap_or(0);
                    let offset = if height == 0 {
                        0
                    } else {
                        selection.saturating_sub(height - 1)
                    };
                    let expected: Vec<_> = flat.iter().skip(offset).take(height).copied().collect();
                    let (rows, actual_offset, total) =
                        duplicate_viewport(groups.iter().copied(), height);
                    assert_eq!(rows, expected);
                    assert_eq!(actual_offset, offset);
                    assert_eq!(total, flat.len());
                }
            }
        }
    }

    #[test]
    fn million_row_group_only_materializes_the_viewport() {
        let groups = [(1_000_001, Some(1_000_000))];
        let (rows, offset, total) = duplicate_viewport(groups.into_iter(), 24);
        assert_eq!(rows.len(), 24);
        assert_eq!(offset, 999_977);
        assert_eq!(total, 1_000_001);
        assert_eq!(rows.last(), Some(&(0, 1_000_000)));
    }
}
