//! Application rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Widget};
use strum::IntoEnumIterator;

use gravityfile_analyze::format_age;

use crate::app::App;
use crate::ui::modals::{
    BookmarkListModal, BookmarkPrompt, BulkRenameConfirmModal, CommandPalette, ConflictModal,
    DeleteConfirmModal, DeletionProgressModal, InputModal, OperationProgressModal, SettingsModal,
};
use crate::ui::{
    AppLayout, HelpOverlay, MillerColumns, TreeView, TreemapView,
    format_relative_time, format_size,
};

use super::state::{
    AppMode, ClipboardMode, LayoutMode, View,
};

/// Item in the duplicates list (either a group header or a file within a group).
enum DuplicateListItem<'a> {
    GroupHeader {
        group: &'a gravityfile_analyze::DuplicateGroup,
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

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
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

        // Show scanning indicator when scan is in progress
        let is_scanning = self.scan_progress.is_some();
        let scanning_indicator = if is_scanning {
            if let Some(progress) = &self.scan_progress {
                Span::styled(
                    format!(
                        " ⟳ scanning: {} files, {} ",
                        progress.files_scanned,
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
        } else {
            Span::raw("")
        };

        let stats = if let Some(tree) = &self.tree {
            format!(
                " {} in {} files, {} dirs ",
                format_size(tree.stats.total_size),
                tree.stats.total_files,
                tree.stats.total_dirs
            )
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
                .sum();
            Span::styled(
                format!(
                    " {} selected ({}) ",
                    self.marked.len(),
                    format_size(total_size)
                ),
                Style::default().fg(self.theme.background).bg(self.theme.info),
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

    fn render_overlays(&self, area: Rect, footer: Rect, buf: &mut Buffer) {
        match self.mode {
            AppMode::Help => {
                HelpOverlay::new(&self.theme).render(area, buf);
            }
            AppMode::ConfirmDelete => {
                let modal = DeleteConfirmModal::new(&self.theme, &self.marked, |p| self.get_path_size(p));
                modal.render(area, buf);
            }
            AppMode::Deleting => {
                DeletionProgressModal::new(&self.theme, self.deletion_progress.as_ref()).render(area, buf);
            }
            AppMode::Command => {
                CommandPalette::new(&self.theme, self.command_input.buffer(), self.command_input.cursor())
                    .render(footer, buf);
            }
            AppMode::Renaming => {
                if let Some(input) = &self.input_state {
                    InputModal::new(&self.theme, input, "Rename", "Enter new name:").render(area, buf);
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
            AppMode::Copying | AppMode::Moving => {
                if let Some(progress) = &self.operation_progress {
                    OperationProgressModal::new(&self.theme, progress).render(area, buf);
                }
            }
            AppMode::ConflictResolution => {
                if let Some(conflict) = &self.pending_conflict {
                    ConflictModal::new(&self.theme, conflict).render(area, buf);
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
                    BookmarkListModal::new(&self.theme, &self.user_settings.bookmarks, state).render(area, buf);
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
        let tabs = self.tab_manager.tabs();
        let active = self.tab_manager.active_index();

        let max_tab_width = if tabs.is_empty() {
            10
        } else {
            (area.width as usize / tabs.len()).clamp(8, 20)
        };

        let titles: Vec<String> = tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let prefix = if i < 9 {
                    format!("{}:", i + 1)
                } else {
                    String::new()
                };
                let label = tab.short_label(max_tab_width.saturating_sub(prefix.len() + 2));
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
        let error_count = self.warnings.len();

        let titles: Vec<String> = View::iter()
            .map(|v| {
                if v == View::Errors && error_count > 0 {
                    format!(" {} ({}) ", v, error_count)
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
                    let title = if self.view_root != self.path {
                        let relative = self.view_root
                            .strip_prefix(&self.path)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| self.view_root.display().to_string());
                        format!(" {} (\u{2190} Backspace) ", relative)
                    } else {
                        format!(" {} ", view_path.display())
                    };

                    let tree_view = TreeView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(self.theme.border)
                                .title(title)
                                .title_style(self.theme.title),
                        );

                    let mut tree_state = self.tree_state.clone();
                    ratatui::widgets::StatefulWidget::render(
                        tree_view,
                        layout.main,
                        buf,
                        &mut tree_state,
                    );
                }
                LayoutMode::Miller => {
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

                    let mut miller_state = self.miller_state.clone();
                    ratatui::widgets::StatefulWidget::render(
                        miller,
                        layout.main,
                        buf,
                        &mut miller_state,
                    );
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
        let title = if self.view_root != self.path {
            let relative = self.view_root
                .strip_prefix(&self.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| self.view_root.display().to_string());
            format!(" Duplicates in {} ", relative)
        } else {
            " Duplicates ".to_string()
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
                Paragraph::new("No duplicate files found in this directory.\n\nTip: Press R to scan for duplicates if you haven't already.")
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

            let mut items: Vec<DuplicateListItem> = Vec::new();
            for (group_idx, group) in filtered_groups.iter().enumerate() {
                let is_expanded = self.duplicates_state.is_expanded(group_idx);
                let selected_in_group = self.duplicates_state.selected_item(group_idx);
                let is_group_selected = group_idx == self.duplicates_state.selected_group;

                items.push(DuplicateListItem::GroupHeader {
                    group,
                    is_expanded,
                    is_selected: is_group_selected && selected_in_group == 0,
                });

                if is_expanded {
                    for (file_idx, path) in group.paths.iter().enumerate() {
                        let is_marked = self.marked.contains(path);
                        items.push(DuplicateListItem::File {
                            file_idx,
                            path,
                            is_selected: is_group_selected && selected_in_group == file_idx + 1,
                            is_marked,
                        });
                    }
                }
            }

            let selected_item_idx = items
                .iter()
                .position(|item| match item {
                    DuplicateListItem::GroupHeader { is_selected, .. } => *is_selected,
                    DuplicateListItem::File { is_selected, .. } => *is_selected,
                })
                .unwrap_or(0);

            let scroll_offset = if selected_item_idx >= visible_height {
                selected_item_idx - visible_height + 1
            } else {
                0
            };

            for (render_idx, item) in items
                .iter()
                .enumerate()
                .skip(scroll_offset)
                .take(visible_height)
            {
                let y = list_area.y + (render_idx - scroll_offset) as u16;
                let line_area = Rect::new(list_area.x, y, list_area.width, 1);

                match item {
                    DuplicateListItem::GroupHeader {
                        group,
                        is_expanded,
                        is_selected,
                        ..
                    } => {
                        let expand_icon = if *is_expanded { "▼" } else { "▶" };
                        let files_in_view = group
                            .paths
                            .iter()
                            .filter(|p| p.starts_with(&self.view_root))
                            .count();
                        let total_files = group.count();

                        let file_info = if files_in_view < total_files {
                            format!("{}/{}", files_in_view, total_files)
                        } else {
                            format!("{}", total_files)
                        };

                        let max_wasted = filtered_groups
                            .iter()
                            .map(|g| g.wasted_bytes)
                            .max()
                            .unwrap_or(1);
                        let heat_ratio = group.wasted_bytes as f64 / max_wasted as f64;

                        let bar_width = 8;
                        let filled = (heat_ratio * bar_width as f64).round() as usize;
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
                            .strip_prefix(&self.view_root)
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

            if items.len() > visible_height {
                let total = items.len();
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
            Paragraph::new("Analyzing duplicates...\n\nTip: Press Tab to switch views, R to scan.")
                .style(Style::default().fg(self.theme.muted))
                .render(inner, buf);
        }
    }

    fn render_age(&self, area: Rect, buf: &mut Buffer) {
        let title = if self.view_root != self.path {
            let relative = self.view_root
                .strip_prefix(&self.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| self.view_root.display().to_string());
            format!(" Age Analysis - {} ", relative)
        } else {
            " Age Analysis ".to_string()
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

            let bucket_start_y = if self.view_root != self.path {
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
                    let total_stale_size: u64 = stale_dirs.iter().map(|d| d.size).sum();
                    let stale_header = if stale_dirs.is_empty() {
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

                    let selected = self.selected_stale_dir.min(stale_dirs.len().saturating_sub(1));

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
            Paragraph::new("Analyzing file ages...")
                .style(Style::default().fg(self.theme.muted))
                .render(inner, buf);
        }
    }

    fn render_errors(&self, area: Rect, buf: &mut Buffer) {
        let is_scanning = self.scan_progress.is_some();

        let title = if is_scanning {
            let count = self.scan_progress.as_ref().map(|p| p.errors_count).unwrap_or(0);
            format!(" Errors & Warnings ({}) ", count)
        } else if !self.warnings.is_empty() {
            format!(" Scan Errors & Warnings ({}) ", self.warnings.len())
        } else {
            " Scan Errors & Warnings ".to_string()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title)
            .title_style(self.theme.title);

        let inner = block.inner(area);
        block.render(area, buf);

        if self.warnings.is_empty() {
            let lines = if is_scanning {
                let error_count = self.scan_progress.as_ref().map(|p| p.errors_count).unwrap_or(0);
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
            .warnings
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_items)
        {
            let base_y = inner.y + ((i - scroll_offset) * lines_per_item) as u16;
            let is_selected = i == self.selected_warning;

            let (icon, kind_label, can_delete) = match warning.kind {
                gravityfile_core::WarningKind::PermissionDenied => {
                    ("\u{1F512}", "Permission Denied", false)
                }
                gravityfile_core::WarningKind::BrokenSymlink => ("\u{1F517}", "Broken Symlink", true),
                gravityfile_core::WarningKind::ReadError => ("\u{26A0}", "Read Error", false),
                gravityfile_core::WarningKind::MetadataError => ("\u{1F4CB}", "Metadata Error", false),
                gravityfile_core::WarningKind::CrossFilesystem => {
                    ("\u{1F4BE}", "Cross Filesystem", false)
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
            let available_width = (inner.width as usize).saturating_sub(prefix.len() + suffix_len + 1);
            let display_path = if path_str.len() > available_width {
                format!(
                    "...{}",
                    &path_str[path_str.len().saturating_sub(available_width - 3)..]
                )
            } else {
                path_str
            };

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
                let msg = if warning.message.len() > max_msg_len {
                    format!("{}...", &warning.message[..max_msg_len.saturating_sub(3)])
                } else {
                    warning.message.clone()
                };

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

        let (view_node, view_path) = self.get_view_root_node().unwrap_or((&tree.root, tree.root_path.clone()));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(format!(" Treemap: {} ", view_path.display()))
            .title_style(self.theme.title);

        TreemapView::new(view_node, &view_path, &self.theme, &self.marked, &self.clipboard)
            .block(block)
            .selected(self.treemap_state.selected)
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

        let mut keys: Vec<(&str, &str)> = if in_visual_mode {
            let mut v = vec![
                ("j/k", "Extend"),
                ("Enter", "Apply"),
                ("Esc", "Cancel"),
                ("y", "Yank"),
                ("d", "Delete"),
            ];
            v.insert(0, ("V", "VISUAL"));
            v
        } else {
            match self.view {
                View::Explorer => {
                    let mut v = vec![("j/k", "Nav"), ("y", "Copy"), ("x", "Cut"), ("d", "Del")];
                    if !self.clipboard.is_empty() {
                        v.push(("p", "Paste"));
                        v.push(("Esc", "Unclip"));
                    } else if !self.marked.is_empty() {
                        v.push(("Esc", "Unmark"));
                    } else {
                        v.push(("Spc", "+Sel"));
                    }
                    v.push(("V", "Visual"));
                    v.push(("v", match self.layout_mode {
                        LayoutMode::Tree => "Miller",
                        LayoutMode::Miller => "Tree",
                    }));
                    v.push(("s", self.sort_mode.short_label()));
                    v
                }
                View::Duplicates => {
                    let is_on_header = !self.duplicates_state.is_expanded(self.duplicates_state.selected_group)
                        || self.duplicates_state.selected_item(self.duplicates_state.selected_group) == 0;
                    let mut v = vec![("j/k", "Nav"), ("h/l", "±Grp")];
                    if is_on_header {
                        v.push(("d", "Del Dups"));
                        v.push(("Spc", "Sel All"));
                    } else {
                        v.push(("d", "Del File"));
                        v.push(("Spc", "Toggle"));
                    }
                    if !self.marked.is_empty() {
                        v.push(("Esc", "Clear"));
                    }
                    v
                }
                View::Age => {
                    let mut v = vec![("j/k", "Nav"), ("y", "Copy"), ("d", "Del")];
                    if !self.clipboard.is_empty() {
                        v.push(("Esc", "Unclip"));
                    } else if !self.marked.is_empty() {
                        v.push(("Esc", "Unmark"));
                    } else {
                        v.push(("Spc", "+Sel"));
                    }
                    v
                }
                View::Errors => {
                    let mut v = vec![("j/k", "Nav"), ("Spc", "+Sel"), ("d", "Del")];
                    if !self.marked.is_empty() {
                        v.push(("Esc", "Clear"));
                    }
                    v
                }
                View::Treemap => {
                    let mut v = vec![("Enter", "Drill"), ("Bksp", "Back"), ("Spc", "+Sel"), ("d", "Del")];
                    if !self.marked.is_empty() {
                        v.push(("Esc", "Clear"));
                    }
                    v
                }
            }
        };

        if self.scan_progress.is_none() {
            if !self.has_full_scan {
                keys.insert(0, ("⇧R", "Scan"));
            } else {
                keys.push(("⇧R", "Rescan"));
            }
        }

        keys.extend([("?", "Help"), ("q", "Quit")]);

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
            .style(Style::default().bg(self.theme.background).fg(self.theme.foreground))
            .border_style(self.theme.border);
        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let [input_area, sep_area, results_area] = inner.layout(&Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ]));

        let query = &self.search_state.query;
        let cursor = self.search_state.cursor;
        let mut spans = vec![Span::styled("/", self.theme.help_key)];

        if query.is_empty() {
            spans.push(Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)));
        } else {
            let before_cursor = &query[..cursor];
            let at_cursor = query.chars().nth(cursor).map(|c| c.to_string()).unwrap_or_else(|| " ".to_string());
            let after_cursor = if cursor < query.len() { &query[cursor + 1..] } else { "" };
            spans.push(Span::raw(before_cursor));
            spans.push(Span::styled(at_cursor, Style::default().add_modifier(Modifier::REVERSED)));
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

        if results.is_empty() {
            if !query.is_empty() {
                Paragraph::new("No matches found").style(Style::default().fg(self.theme.muted)).render(results_area, buf);
            }
        } else {
            let max_visible = results_area.height as usize;
            let offset = if selected >= max_visible { selected - max_visible + 1 } else { 0 };

            for (i, result) in results.iter().skip(offset).take(max_visible).enumerate() {
                let row = i as u16;
                let is_selected = i + offset == selected;
                let base_style = if is_selected { self.theme.selected } else if result.is_dir { self.theme.directory } else { Style::default().fg(self.theme.foreground) };
                let prefix = if result.is_dir { " " } else { "   " };
                let match_style = if is_selected { base_style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { Style::default().fg(self.theme.warning).add_modifier(Modifier::BOLD) };

                let mut spans: Vec<Span> = vec![Span::styled(prefix, base_style)];
                for (char_pos, ch) in result.display.chars().enumerate() {
                    let is_match = result.matched_indices.contains(&char_pos);
                    spans.push(Span::styled(ch.to_string(), if is_match { match_style } else { base_style }));
                }

                Line::from(spans).render(Rect::new(results_area.x, results_area.y + row, results_area.width, 1), buf);
            }

            if results.len() > max_visible {
                let count_str = format!("[{}/{}]", selected + 1, results.len());
                Line::from(count_str)
                    .style(Style::default().fg(self.theme.muted))
                    .render(Rect::new(results_area.x + results_area.width.saturating_sub(10), results_area.y + results_area.height - 1, 10, 1), buf);
            }
        }
    }
}
