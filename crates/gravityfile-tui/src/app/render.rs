//! Application rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Widget};
use strum::IntoEnumIterator;

use gravityfile_analyze::format_age;
use gravityfile_ops::{Conflict, OperationProgress};

use crate::preview::PreviewContent;
use crate::theme::Theme;
use crate::ui::modals::{
    CommandPalette, ConflictModal, DeleteConfirmModal, DeletionProgressModal, InputModal,
    OperationProgressModal, SettingsModal,
};
use crate::ui::{
    format_relative_time, format_size, AppLayout, HelpOverlay, MillerColumns, MillerState,
    TreeState, TreeView,
};

use super::input::InputState;
use super::state::{
    AppMode, ClipboardMode, ClipboardState, DeletionProgress, LayoutMode, SelectedInfo,
    SettingsState, SortMode, View,
};

/// Item in the duplicates list (either a group header or a file within a group).
enum DuplicateListItem<'a> {
    GroupHeader {
        #[allow(dead_code)]
        group_idx: usize,
        group: &'a gravityfile_analyze::DuplicateGroup,
        is_expanded: bool,
        is_selected: bool,
    },
    File {
        #[allow(dead_code)]
        group_idx: usize,
        #[allow(dead_code)]
        file_idx: usize,
        path: &'a std::path::PathBuf,
        is_selected: bool,
        is_marked: bool,
    },
}

/// Render context containing all the state needed for rendering.
pub struct RenderContext<'a> {
    pub mode: AppMode,
    pub view: View,
    pub theme: &'a Theme,
    pub path: &'a std::path::Path,
    pub view_root: &'a std::path::Path,
    pub show_details: bool,
    pub tree: Option<&'a gravityfile_core::FileTree>,
    pub tree_state: &'a TreeState,
    pub layout_mode: LayoutMode,
    pub miller_state: &'a MillerState,
    pub scan_progress: Option<&'a gravityfile_scan::ScanProgress>,
    pub deletion_progress: Option<&'a DeletionProgress>,
    #[allow(dead_code)] // Used indirectly via get_filtered_duplicates
    pub duplicates: Option<&'a gravityfile_analyze::DuplicateReport>,
    pub age_report: Option<&'a gravityfile_analyze::AgeReport>,
    pub warnings: &'a [gravityfile_core::ScanWarning],
    pub duplicates_state: &'a super::state::DuplicatesViewState,
    pub selected_stale_dir: usize,
    pub selected_warning: usize,
    pub marked: &'a std::collections::HashSet<std::path::PathBuf>,
    pub deletion_message: Option<&'a (bool, String)>,
    pub operation_message: Option<&'a (bool, String)>,
    pub error: Option<&'a str>,
    pub command_input: &'a str,
    pub command_cursor: usize,
    pub input_state: Option<&'a InputState>,
    pub operation_progress: Option<&'a OperationProgress>,
    pub pending_conflict: Option<&'a Conflict>,
    pub clipboard: &'a ClipboardState,
    pub get_path_size: Box<dyn Fn(&std::path::PathBuf) -> Option<u64> + 'a>,
    pub get_selected_info: Option<SelectedInfo>,
    pub get_view_root_node:
        Option<(&'a gravityfile_core::FileNode, std::path::PathBuf)>,
    pub get_parent_node: Option<&'a gravityfile_core::FileNode>,
    pub current_dir_name: Option<String>,
    pub get_filtered_duplicates:
        Option<(Vec<&'a gravityfile_analyze::DuplicateGroup>, u64)>,
    pub get_filtered_stale_dirs: Option<Vec<&'a gravityfile_analyze::StaleDirectory>>,
    pub sort_mode: SortMode,
    pub search_state: &'a crate::search::SearchState,
    pub tab_manager: &'a super::state::TabManager,
    pub preview_content: &'a PreviewContent,
    /// Current preview mode.
    pub preview_mode: crate::preview::PreviewMode,
    /// Whether a full recursive scan has been completed (vs just quick_list).
    pub has_full_scan: bool,
    /// Settings modal state.
    pub settings_state: Option<&'a SettingsState>,
}

/// Main render function for the application.
pub fn render_app(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    // Fill entire area with theme background color
    let base_style = Style::default()
        .bg(ctx.theme.background)
        .fg(ctx.theme.foreground);
    buf.set_style(area, base_style);

    // Determine if we need to show directory tabs
    let show_dir_tabs = ctx.tab_manager.len() > 1;

    // Layout: header, [dir_tabs], view_tabs, content, footer
    let (header, dir_tabs_area, tabs_area, content, footer) = if show_dir_tabs {
        let [header, dir_tabs, tabs_area, content, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .areas(area);
        (header, Some(dir_tabs), tabs_area, content, footer)
    } else {
        let [header, tabs_area, content, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .areas(area);
        (header, None, tabs_area, content, footer)
    };

    // Render header
    render_header(ctx, header, buf);

    // Render directory tabs (if multiple tabs open)
    if let Some(dir_tabs) = dir_tabs_area {
        render_dir_tabs(ctx, dir_tabs, buf);
    }

    // Render view tabs (Explorer, Duplicates, Age, Errors)
    render_tabs(ctx, tabs_area, buf);

    // Render content based on view
    // Always show the view the user selected - scanning happens in background with header indicator
    match ctx.view {
        View::Explorer => render_explorer(ctx, content, buf),
        View::Duplicates => render_duplicates(ctx, content, buf),
        View::Age => render_age(ctx, content, buf),
        View::Errors => render_errors(ctx, content, buf),
    }

    // Render footer
    render_footer(ctx, footer, buf);

    // Render overlays
    match ctx.mode {
        AppMode::Help => {
            HelpOverlay::new(ctx.theme).render(area, buf);
        }
        AppMode::ConfirmDelete => {
            let modal = DeleteConfirmModal::new(ctx.theme, ctx.marked, |p| (ctx.get_path_size)(p));
            modal.render(area, buf);
        }
        AppMode::Deleting => {
            DeletionProgressModal::new(ctx.theme, ctx.deletion_progress).render(area, buf);
        }
        AppMode::Command => {
            CommandPalette::new(ctx.theme, ctx.command_input, ctx.command_cursor)
                .render(footer, buf);
        }
        AppMode::Renaming => {
            if let Some(input) = ctx.input_state {
                InputModal::new(ctx.theme, input, "Rename", "Enter new name:").render(area, buf);
            }
        }
        AppMode::CreatingFile => {
            if let Some(input) = ctx.input_state {
                InputModal::new(ctx.theme, input, "Create File", "Enter file name:")
                    .render(area, buf);
            }
        }
        AppMode::CreatingDirectory => {
            if let Some(input) = ctx.input_state {
                InputModal::new(ctx.theme, input, "Create Directory", "Enter directory name:")
                    .render(area, buf);
            }
        }
        AppMode::Taking => {
            if let Some(input) = ctx.input_state {
                InputModal::new(ctx.theme, input, "Take (mkdir + cd)", "Enter directory name:")
                    .render(area, buf);
            }
        }
        AppMode::GoingTo => {
            if let Some(input) = ctx.input_state {
                InputModal::new(ctx.theme, input, "Go To Directory", "Enter path:")
                    .render(area, buf);
            }
        }
        AppMode::Copying | AppMode::Moving => {
            if let Some(progress) = ctx.operation_progress {
                OperationProgressModal::new(ctx.theme, progress).render(area, buf);
            }
        }
        AppMode::ConflictResolution => {
            if let Some(conflict) = ctx.pending_conflict {
                ConflictModal::new(ctx.theme, conflict).render(area, buf);
            }
        }
        AppMode::Search => {
            render_search_overlay(ctx, area, buf);
        }
        AppMode::Settings => {
            if let Some(state) = ctx.settings_state {
                SettingsModal::new(ctx.theme, state).render(area, buf);
            }
        }
        _ => {}
    }
}

/// Render the fuzzy search overlay.
fn render_search_overlay(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    use ratatui::widgets::Clear;

    // Create a centered popup
    let popup_width = area.width.min(60).max(30);
    let popup_height = area.height.min(20).max(10);
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 3; // Slightly above center

    let popup_area = Rect::new(area.x + x, area.y + y, popup_width, popup_height);

    // Clear the popup area
    Clear.render(popup_area, buf);

    // Draw border with mode indicator
    let title = format!(" Search ({}) ", ctx.search_state.mode.label());
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::default().bg(ctx.theme.background).fg(ctx.theme.foreground))
        .border_style(ctx.theme.border);
    let inner = block.inner(popup_area);
    block.render(popup_area, buf);

    // Layout: search input (1 line), separator (1 line), results
    let [input_area, sep_area, results_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    // Render search input with cursor
    let query = &ctx.search_state.query;
    let cursor = ctx.search_state.cursor;

    let mut spans = vec![
        Span::styled("/", ctx.theme.help_key),
    ];

    // Display query with cursor
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

    // Render separator with hints
    let hints = Line::from(vec![
        Span::styled("Tab", ctx.theme.help_key),
        Span::styled(":mode ", ctx.theme.help_desc),
        Span::styled("↑↓", ctx.theme.help_key),
        Span::styled(":nav ", ctx.theme.help_desc),
        Span::styled("Enter", ctx.theme.help_key),
        Span::styled(":go ", ctx.theme.help_desc),
        Span::styled("Esc", ctx.theme.help_key),
        Span::styled(":cancel", ctx.theme.help_desc),
    ]);
    Paragraph::new(hints)
        .style(Style::default().fg(ctx.theme.muted))
        .render(sep_area, buf);

    // Render results
    let results = &ctx.search_state.results;
    let selected = ctx.search_state.selected;

    if results.is_empty() {
        if !query.is_empty() {
            let msg = Paragraph::new("No matches found")
                .style(Style::default().fg(ctx.theme.muted));
            msg.render(results_area, buf);
        }
    } else {
        let max_visible = results_area.height as usize;
        let offset = if selected >= max_visible {
            selected - max_visible + 1
        } else {
            0
        };

        let mut y = results_area.y;
        for (i, result) in results.iter().skip(offset).take(max_visible).enumerate() {
            let is_selected = i + offset == selected;
            let style = if is_selected {
                ctx.theme.selected
            } else if result.is_dir {
                ctx.theme.directory
            } else {
                Style::default().fg(ctx.theme.foreground)
            };

            // Add indicator for directories
            let prefix = if result.is_dir { "📁 " } else { "   " };
            let text = format!("{}{}", prefix, result.display);

            // Truncate if needed
            let max_width = results_area.width as usize;
            let display = if text.len() > max_width {
                format!("{}…", &text[..max_width - 1])
            } else {
                text
            };

            buf.set_string(results_area.x, y, &display, style);
            y += 1;
        }

        // Show result count
        if results.len() > max_visible {
            let count_str = format!("[{}/{}]", selected + 1, results.len());
            let x = results_area.x + results_area.width.saturating_sub(count_str.len() as u16);
            buf.set_string(x, results_area.y + results_area.height - 1, &count_str, Style::default().fg(ctx.theme.muted));
        }
    }
}

fn render_header(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let title = Span::styled(
        " gravityfile ",
        ctx.theme.title.add_modifier(Modifier::BOLD),
    );

    // Show scanning indicator when scan is in progress (even with quick tree)
    let is_scanning = ctx.scan_progress.is_some();
    let scanning_indicator = if is_scanning {
        if let Some(progress) = ctx.scan_progress {
            Span::styled(
                format!(
                    " ⟳ scanning: {} files, {} ",
                    progress.files_scanned,
                    format_size(progress.bytes_scanned)
                ),
                Style::default()
                    .fg(ctx.theme.info)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                " ⟳ scanning... ",
                Style::default()
                    .fg(ctx.theme.info)
                    .add_modifier(Modifier::BOLD),
            )
        }
    } else {
        Span::raw("")
    };

    let stats = if let Some(tree) = &ctx.tree {
        format!(
            " {} in {} files, {} dirs ",
            format_size(tree.stats.total_size),
            tree.stats.total_files,
            tree.stats.total_dirs
        )
    } else {
        String::new()
    };

    let stats_span = Span::styled(stats, ctx.theme.header);

    // Show operation message, deletion message, marked items, or clipboard status
    let status = if let Some((success, msg)) = ctx.operation_message {
        let color = if *success {
            ctx.theme.success
        } else {
            ctx.theme.warning
        };
        Span::styled(format!(" {} ", msg), Style::default().fg(color))
    } else if let Some((success, msg)) = ctx.deletion_message {
        let color = if *success {
            ctx.theme.success
        } else {
            ctx.theme.warning
        };
        Span::styled(format!(" {} ", msg), Style::default().fg(color))
    } else if !ctx.marked.is_empty() {
        let total_size: u64 = ctx
            .marked
            .iter()
            .filter_map(|p| (ctx.get_path_size)(p))
            .sum();
        Span::styled(
            format!(
                " {} selected ({}) ",
                ctx.marked.len(),
                format_size(total_size)
            ),
            Style::default()
                .fg(ctx.theme.background)
                .bg(ctx.theme.info),
        )
    } else {
        Span::raw("")
    };

    // Show clipboard status
    let clipboard_status = if !ctx.clipboard.is_empty() {
        let mode_str = match ctx.clipboard.mode {
            ClipboardMode::Copy => "copied",
            ClipboardMode::Cut => "cut",
            ClipboardMode::Empty => "",
        };
        if !mode_str.is_empty() {
            Span::styled(
                format!(" {} {} ", ctx.clipboard.paths.len(), mode_str),
                Style::default()
                    .fg(ctx.theme.background)
                    .bg(ctx.theme.success),
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
        .style(ctx.theme.header)
        .render(area, buf);
}

fn render_tabs(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    // Always show the standard View tabs - user can switch views during scanning
    let is_scanning = ctx.scan_progress.is_some();
    let error_count = ctx.warnings.len();

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

    let tabs = Tabs::new(titles)
        .select(ctx.view as usize)
        .style(ctx.theme.footer)
        .highlight_style(ctx.theme.selected);

    tabs.render(area, buf);
}

/// Render directory tabs (multiple directory contexts).
fn render_dir_tabs(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let tabs = ctx.tab_manager.tabs();
    let active = ctx.tab_manager.active_index();

    // Calculate max width per tab
    let max_tab_width = if tabs.is_empty() {
        10
    } else {
        (area.width as usize / tabs.len()).max(8).min(20)
    };

    // Build tab titles with index prefix
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

    let tabs_widget = Tabs::new(titles)
        .select(active)
        .style(Style::default().fg(ctx.theme.muted))
        .highlight_style(ctx.theme.selected.add_modifier(Modifier::BOLD))
        .divider("|");

    tabs_widget.render(area, buf);
}

#[allow(dead_code)]
fn render_scanning(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ctx.theme.border)
        .title(format!(" Scanning {} ", ctx.path.display()))
        .title_style(ctx.theme.title);

    let inner = block.inner(area);
    block.render(area, buf);

    let mut lines = vec![
        Line::raw(""),
        Line::styled(
            "  Scanning directory...",
            Style::default()
                .fg(ctx.theme.info)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];

    if let Some(progress) = &ctx.scan_progress {
        lines.push(Line::from(vec![
            Span::styled("  Files: ", ctx.theme.help_desc),
            Span::raw(progress.files_scanned.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Dirs:  ", ctx.theme.help_desc),
            Span::raw(progress.dirs_scanned.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  Size:  ", ctx.theme.help_desc),
            Span::raw(format_size(progress.bytes_scanned)),
        ]));

        if progress.errors_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  Errors: ", Style::default().fg(ctx.theme.warning)),
                Span::styled(
                    progress.errors_count.to_string(),
                    Style::default().fg(ctx.theme.warning),
                ),
            ]));
        }

        let current = progress.current_path.display().to_string();
        let max_width = inner.width.saturating_sub(4) as usize;
        let display_path = if current.len() > max_width {
            format!(
                "...{}",
                &current[current.len().saturating_sub(max_width - 3)..]
            )
        } else {
            current
        };
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  {}", display_path),
            Style::default().fg(ctx.theme.muted),
        ));
    }

    Paragraph::new(lines).render(inner, buf);
}

fn render_explorer(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let layout = AppLayout::new(area, ctx.show_details);

    if let Some((view_node, view_path)) = &ctx.get_view_root_node {
        match ctx.layout_mode {
            LayoutMode::Tree => {
                // Build title showing navigation context
                let title = if ctx.view_root != ctx.path {
                    let relative = ctx
                        .view_root
                        .strip_prefix(ctx.path)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| ctx.view_root.display().to_string());
                    format!(" {} (\u{2190} Backspace) ", relative)
                } else {
                    format!(" {} ", view_path.display())
                };

                let tree_view = TreeView::new(view_node, view_path, ctx.theme, ctx.marked, ctx.clipboard).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(ctx.theme.border)
                        .title(title)
                        .title_style(ctx.theme.title),
                );

                let mut tree_state = ctx.tree_state.clone();
                ratatui::widgets::StatefulWidget::render(
                    tree_view,
                    layout.main,
                    buf,
                    &mut tree_state,
                );
            }
            LayoutMode::Miller => {
                // Render Miller columns view
                let current_name = ctx
                    .current_dir_name
                    .as_deref()
                    .unwrap_or_else(|| view_path.file_name().and_then(|n| n.to_str()).unwrap_or(""));

                let miller = MillerColumns::new(
                    view_node,
                    ctx.get_parent_node,
                    current_name,
                    view_path,
                    ctx.marked,
                    ctx.clipboard,
                    ctx.theme,
                )
                .file_preview(ctx.preview_content)
                .preview_mode(ctx.preview_mode);

                let mut miller_state = ctx.miller_state.clone();
                ratatui::widgets::StatefulWidget::render(
                    miller,
                    layout.main,
                    buf,
                    &mut miller_state,
                );
            }
        }
    } else if let Some(error) = ctx.error {
        let error_block = Block::default()
            .borders(Borders::ALL)
            .border_style(ctx.theme.border)
            .title(" Error ")
            .title_style(Style::default().fg(ctx.theme.error));

        let error_text = Paragraph::new(error)
            .block(error_block)
            .style(Style::default().fg(ctx.theme.error));

        error_text.render(layout.main, buf);
    } else {
        // No tree and no error - show placeholder
        let placeholder_block = Block::default()
            .borders(Borders::ALL)
            .border_style(ctx.theme.border)
            .title(format!(" {} ", ctx.path.display()))
            .title_style(ctx.theme.title);

        let placeholder = Paragraph::new(vec![
            Line::raw(""),
            Line::styled(
                "  No data available. Press 'r' to rescan.",
                Style::default().fg(ctx.theme.muted),
            ),
        ])
        .block(placeholder_block);

        placeholder.render(layout.main, buf);
    }

    if let Some(details_area) = layout.details {
        render_details(ctx, details_area, buf);
    }
}

fn render_duplicates(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let title = if ctx.view_root != ctx.path {
        let relative = ctx
            .view_root
            .strip_prefix(ctx.path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ctx.view_root.display().to_string());
        format!(" Duplicates in {} ", relative)
    } else {
        " Duplicates ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ctx.theme.border)
        .title(title)
        .title_style(ctx.theme.title);

    let inner = block.inner(area);
    block.render(area, buf);

    if let Some((filtered_groups, total_wasted)) = &ctx.get_filtered_duplicates {
        if filtered_groups.is_empty() {
            let msg = Paragraph::new("No duplicate files found in this directory.\n\nTip: Press R to scan for duplicates if you haven't already.")
                .style(Style::default().fg(ctx.theme.muted));
            msg.render(inner, buf);
            return;
        }

        // Header with summary - clearer explanation of wasted space
        let marked_count = ctx.marked.len();
        let header = if marked_count > 0 {
            format!(
                " {} groups | {} reclaimable | {} marked for deletion",
                filtered_groups.len(),
                format_size(*total_wasted),
                marked_count
            )
        } else {
            format!(
                " {} groups | {} reclaimable",
                filtered_groups.len(),
                format_size(*total_wasted)
            )
        };
        let header_line = Line::styled(header, ctx.theme.title);
        let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
        Paragraph::new(header_line).render(header_area, buf);

        let list_area = Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(2),
        );
        let visible_height = list_area.height as usize;

        // Build a flat list of items to render (groups + expanded files)
        let mut items: Vec<DuplicateListItem> = Vec::new();
        for (group_idx, group) in filtered_groups.iter().enumerate() {
            let is_expanded = ctx.duplicates_state.is_expanded(group_idx);
            let selected_in_group = ctx.duplicates_state.selected_item(group_idx);
            let is_group_selected = group_idx == ctx.duplicates_state.selected_group;

            // Group header
            items.push(DuplicateListItem::GroupHeader {
                group_idx,
                group,
                is_expanded,
                is_selected: is_group_selected && selected_in_group == 0,
            });

            // Files within expanded group
            if is_expanded {
                for (file_idx, path) in group.paths.iter().enumerate() {
                    let is_marked = ctx.marked.contains(path);
                    items.push(DuplicateListItem::File {
                        group_idx,
                        file_idx,
                        path,
                        is_selected: is_group_selected && selected_in_group == file_idx + 1,
                        is_marked,
                    });
                }
            }
        }

        // Calculate scroll offset based on selected item
        let selected_item_idx = items.iter().position(|item| match item {
            DuplicateListItem::GroupHeader { is_selected, .. } => *is_selected,
            DuplicateListItem::File { is_selected, .. } => *is_selected,
        }).unwrap_or(0);

        let scroll_offset = if selected_item_idx >= visible_height {
            selected_item_idx - visible_height + 1
        } else {
            0
        };

        // Render visible items
        for (render_idx, item) in items.iter().enumerate().skip(scroll_offset).take(visible_height) {
            let y = list_area.y + (render_idx - scroll_offset) as u16;
            let line_area = Rect::new(list_area.x, y, list_area.width, 1);

            match item {
                DuplicateListItem::GroupHeader { group, is_expanded, is_selected, .. } => {
                    let expand_icon = if *is_expanded { "▼" } else { "▶" };
                    let files_in_view = group.paths.iter().filter(|p| p.starts_with(ctx.view_root)).count();
                    let total_files = group.count();

                    let file_info = if files_in_view < total_files {
                        format!("{}/{}", files_in_view, total_files)
                    } else {
                        format!("{}", total_files)
                    };

                    // Calculate heat ratio (relative to max wasted in this view)
                    let max_wasted = filtered_groups.iter().map(|g| g.wasted_bytes).max().unwrap_or(1);
                    let heat_ratio = group.wasted_bytes as f64 / max_wasted as f64;

                    // Build heat bar (8 chars wide)
                    let bar_width = 8;
                    let filled = (heat_ratio * bar_width as f64).round() as usize;
                    let heat_bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);
                    let heat_color = ctx.theme.size_color(heat_ratio);

                    // Render with colored heat bar
                    let expand_style = if *is_selected { ctx.theme.selected } else { Style::default() };
                    let line = Line::from(vec![
                        Span::styled(format!(" {} ", expand_icon), expand_style),
                        Span::styled(heat_bar, Style::default().fg(heat_color)),
                        Span::styled(
                            format!(" {} × {} = {}",
                                file_info,
                                format_size(group.size),
                                format_size(group.wasted_bytes)
                            ),
                            if *is_selected { ctx.theme.selected } else { Style::default().fg(ctx.theme.warning) }
                        ),
                    ]);

                    Paragraph::new(line).render(line_area, buf);
                }
                DuplicateListItem::File { file_idx, path, is_selected, is_marked, .. } => {
                    // First file (index 0) is the "keep" original, rest are duplicates
                    let prefix = if *file_idx == 0 {
                        "keep"
                    } else {
                        " dup"
                    };
                    let mark = if *is_marked { "●" } else { " " };
                    let display_path = path
                        .strip_prefix(ctx.view_root)
                        .unwrap_or(path)
                        .display()
                        .to_string();

                    let line = format!("   {} {} {}", prefix, mark, display_path);

                    let style = if *is_selected {
                        ctx.theme.selected
                    } else if *is_marked {
                        Style::default().fg(ctx.theme.error)  // Marked for deletion = red
                    } else if *file_idx == 0 {
                        Style::default().fg(ctx.theme.success)  // Keep file = green
                    } else {
                        Style::default().fg(ctx.theme.muted)  // Duplicates = muted
                    };

                    Paragraph::new(Line::styled(line, style)).render(line_area, buf);
                }
            }
        }

        // Show hint at bottom if there are more items
        if items.len() > visible_height {
            let total = items.len();
            let hint = format!(" ({}/{})", scroll_offset + 1, total);
            let hint_x = list_area.x + list_area.width.saturating_sub(hint.len() as u16 + 1);
            if hint_x > list_area.x {
                let hint_area = Rect::new(hint_x, list_area.y + visible_height as u16 - 1, hint.len() as u16, 1);
                if hint_area.y < area.y + area.height - 1 {
                    Paragraph::new(Line::styled(hint, Style::default().fg(ctx.theme.muted)))
                        .render(hint_area, buf);
                }
            }
        }
    } else {
        let msg = Paragraph::new("Analyzing duplicates...\n\nTip: Press Tab to switch views, R to scan.")
            .style(Style::default().fg(ctx.theme.muted));
        msg.render(inner, buf);
    }
}

fn render_age(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let title = if ctx.view_root != ctx.path {
        let relative = ctx
            .view_root
            .strip_prefix(ctx.path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ctx.view_root.display().to_string());
        format!(" Age Analysis - {} ", relative)
    } else {
        " Age Analysis ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ctx.theme.border)
        .title(title)
        .title_style(ctx.theme.title);

    let inner = block.inner(area);
    block.render(area, buf);

    if let Some(age) = ctx.age_report {
        let max_size = age.buckets.iter().map(|b| b.total_size).max().unwrap_or(1);
        let chart_height = age.buckets.len().min(inner.height as usize / 2);

        let bucket_start_y = if ctx.view_root != ctx.path {
            let note = Line::styled(
                " Distribution (full scan):",
                Style::default().fg(ctx.theme.muted),
            );
            let note_area = Rect::new(inner.x, inner.y, inner.width, 1);
            Paragraph::new(note).render(note_area, buf);
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

            let line_area = Rect::new(inner.x, y, inner.width, 1);
            Paragraph::new(line).render(line_area, buf);
        }

        let stale_dirs = ctx.get_filtered_stale_dirs.as_ref().map_or(vec![], |v| v.clone());

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
            let header_area = Rect::new(inner.x, stale_y, inner.width, 1);
            Paragraph::new(Line::styled(stale_header, ctx.theme.title)).render(header_area, buf);

            let list_y = stale_y + 1;
            let list_height = (inner.y + inner.height).saturating_sub(list_y) as usize;

            let selected = ctx
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
                    ctx.theme.selected
                } else {
                    Style::default()
                };

                let line_area = Rect::new(inner.x, y, inner.width, 1);
                Paragraph::new(Line::styled(line, style)).render(line_area, buf);
            }
        }
    } else {
        let msg = Paragraph::new("Analyzing file ages...")
            .style(Style::default().fg(ctx.theme.muted));
        msg.render(inner, buf);
    }
}

fn render_errors(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let is_scanning = ctx.scan_progress.is_some();

    let title = if is_scanning {
        let count = ctx.scan_progress.map(|p| p.errors_count).unwrap_or(0);
        format!(" Errors & Warnings ({}) ", count)
    } else if !ctx.warnings.is_empty() {
        format!(" Scan Errors & Warnings ({}) ", ctx.warnings.len())
    } else {
        " Scan Errors & Warnings ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ctx.theme.border)
        .title(title)
        .title_style(ctx.theme.title);

    let inner = block.inner(area);
    block.render(area, buf);

    if ctx.warnings.is_empty() {
        let lines = if is_scanning {
            let error_count = ctx.scan_progress.map(|p| p.errors_count).unwrap_or(0);
            if error_count > 0 {
                vec![
                    Line::raw(""),
                    Line::styled(
                        format!("  {} errors encountered during scan", error_count),
                        Style::default().fg(ctx.theme.warning),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "  Error details will be available after scan completes.",
                        Style::default().fg(ctx.theme.muted),
                    ),
                ]
            } else {
                vec![Line::styled(
                    "No errors yet...",
                    Style::default().fg(ctx.theme.muted),
                )]
            }
        } else {
            vec![Line::styled(
                "No errors or warnings during scan.",
                Style::default().fg(ctx.theme.muted),
            )]
        };
        Paragraph::new(lines).render(inner, buf);
        return;
    }

    let lines_per_item = 2;
    let visible_items = inner.height as usize / lines_per_item;

    let scroll_offset = if ctx.selected_warning >= visible_items {
        ctx.selected_warning - visible_items + 1
    } else {
        0
    };

    for (i, warning) in ctx
        .warnings
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_items)
    {
        let base_y = inner.y + ((i - scroll_offset) * lines_per_item) as u16;
        let is_selected = i == ctx.selected_warning;

        let (icon, kind_label) = match warning.kind {
            gravityfile_core::WarningKind::PermissionDenied => ("\u{1F512}", "Permission Denied"),
            gravityfile_core::WarningKind::BrokenSymlink => ("\u{1F517}", "Broken Symlink"),
            gravityfile_core::WarningKind::ReadError => ("\u{26A0}", "Read Error"),
            gravityfile_core::WarningKind::MetadataError => ("\u{1F4CB}", "Metadata Error"),
            gravityfile_core::WarningKind::CrossFilesystem => ("\u{1F4BE}", "Cross Filesystem"),
        };

        let path_str = warning.path.display().to_string();
        let prefix = format!(" {} {} ", icon, kind_label);
        let available_width = (inner.width as usize).saturating_sub(prefix.len() + 1);
        let display_path = if path_str.len() > available_width {
            format!(
                "...{}",
                &path_str[path_str.len().saturating_sub(available_width - 3)..]
            )
        } else {
            path_str
        };

        let style = if is_selected {
            ctx.theme.selected
        } else {
            Style::default().fg(ctx.theme.warning)
        };

        let line1 = Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(display_path, style),
        ]);
        let line1_area = Rect::new(inner.x, base_y, inner.width, 1);
        Paragraph::new(line1).render(line1_area, buf);

        if base_y + 1 < inner.y + inner.height {
            let msg_style = if is_selected {
                ctx.theme.selected
            } else {
                Style::default().fg(ctx.theme.muted)
            };

            let max_msg_len = (inner.width as usize).saturating_sub(4);
            let msg = if warning.message.len() > max_msg_len {
                format!("{}...", &warning.message[..max_msg_len.saturating_sub(3)])
            } else {
                warning.message.clone()
            };

            let line2 = Line::styled(format!("    {}", msg), msg_style);
            let line2_area = Rect::new(inner.x, base_y + 1, inner.width, 1);
            Paragraph::new(line2).render(line2_area, buf);
        }
    }
}

fn render_details(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ctx.theme.border)
        .title(" Details ")
        .title_style(ctx.theme.title);

    let inner = block.inner(area);
    block.render(area, buf);

    if let Some(info) = &ctx.get_selected_info {
        let mut lines = vec![
            Line::from(Span::styled(
                &info.name,
                ctx.theme.title.add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            Line::from(vec![
                Span::styled("Size: ", ctx.theme.help_desc),
                Span::raw(format_size(info.size)),
            ]),
        ];

        if info.is_dir {
            lines.push(Line::from(vec![
                Span::styled("Files: ", ctx.theme.help_desc),
                Span::raw(info.file_count.to_string()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Dirs: ", ctx.theme.help_desc),
                Span::raw(info.dir_count.to_string()),
            ]));
        }

        lines.push(Line::from(vec![
            Span::styled("Modified: ", ctx.theme.help_desc),
            Span::raw(format_relative_time(info.modified)),
        ]));

        lines.push(Line::raw(""));
        lines.push(Line::styled("Path:", ctx.theme.help_desc));

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

fn render_footer(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let mut keys: Vec<(&str, &str)> = match ctx.view {
        View::Explorer => {
            let mut v = vec![("j/k", "Nav")];

            // File operations - always available (work on highlighted OR marked items)
            v.push(("y", "Copy"));
            v.push(("x", "Cut"));
            v.push(("d", "Del"));

            // Paste if clipboard has content
            if !ctx.clipboard.is_empty() {
                v.push(("p", "Paste"));
            }

            // Esc clears clipboard (if any) then marks (if any)
            // Show what Esc will do based on current state
            if !ctx.clipboard.is_empty() {
                v.push(("Esc", "Unclip"));
            } else if !ctx.marked.is_empty() {
                v.push(("Esc", "Unmark"));
            } else {
                v.push(("Spc", "+Sel"));
            }

            // Layout toggle
            let layout_hint = match ctx.layout_mode {
                LayoutMode::Tree => "Miller",
                LayoutMode::Miller => "Tree",
            };
            v.push(("v", layout_hint));

            // Sort mode
            v.push(("s", ctx.sort_mode.short_label()));

            v
        }
        View::Duplicates => {
            // Context-aware hints based on selection
            let is_on_header = !ctx.duplicates_state.is_expanded(ctx.duplicates_state.selected_group)
                || ctx.duplicates_state.selected_item(ctx.duplicates_state.selected_group) == 0;

            let mut v = vec![("j/k", "Nav"), ("h/l", "±Grp")];

            if is_on_header {
                // On group header - actions affect all duplicates
                v.push(("d", "Del Dups"));
                v.push(("Spc", "Sel All"));
            } else {
                // On individual file
                v.push(("d", "Del File"));
                v.push(("Spc", "Toggle"));
            }

            if !ctx.marked.is_empty() {
                v.push(("Esc", "Clear"));
            }
            v
        }
        View::Age => {
            let mut v = vec![("j/k", "Nav"), ("y", "Copy"), ("d", "Del")];
            if !ctx.clipboard.is_empty() {
                v.push(("Esc", "Unclip"));
            } else if !ctx.marked.is_empty() {
                v.push(("Esc", "Unmark"));
            } else {
                v.push(("Spc", "+Sel"));
            }
            v
        }
        View::Errors => vec![("j/k", "Nav")],
    };

    // Add scan hint - prominent when no full scan has been done
    // Use Shift+R format to make it clear it's Shift+R not lowercase r
    if ctx.scan_progress.is_none() {
        if !ctx.has_full_scan {
            // Make it very prominent - insert at beginning
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
                Span::styled(format!(" {} ", key), ctx.theme.help_key_style()),
                Span::styled(format!("{} ", desc), ctx.theme.help_desc_style()),
            ]
        })
        .collect();

    let line = Line::from(spans);

    Paragraph::new(line)
        .style(ctx.theme.footer_style())
        .render(area, buf);
}
