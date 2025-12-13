//! Application rendering.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Widget};
use strum::IntoEnumIterator;

use gravityfile_analyze::format_age;
use gravityfile_ops::{Conflict, OperationProgress};

use crate::theme::Theme;
use crate::ui::modals::{
    CommandPalette, ConflictModal, DeleteConfirmModal, DeletionProgressModal, InputModal,
    OperationProgressModal,
};
use crate::ui::{
    format_relative_time, format_size, AppLayout, HelpOverlay, MillerColumns, MillerState,
    TreeState, TreeView,
};

use super::input::InputState;
use super::state::{
    AppMode, ClipboardMode, ClipboardState, DeletionProgress, LayoutMode, ScanView, SelectedInfo,
    View,
};

/// Render context containing all the state needed for rendering.
pub struct RenderContext<'a> {
    pub mode: AppMode,
    pub view: View,
    pub scan_view: ScanView,
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
    pub selected_dup_group: usize,
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
}

/// Main render function for the application.
pub fn render_app(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    // Fill entire area with theme background color
    let base_style = Style::default()
        .bg(ctx.theme.background)
        .fg(ctx.theme.foreground);
    buf.set_style(area, base_style);

    // Layout: header, tabs, content, footer
    let [header, tabs_area, content, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .areas(area);

    // Render header
    render_header(ctx, header, buf);

    // Render tabs
    render_tabs(ctx, tabs_area, buf);

    // Render content based on mode and view
    let is_scanning = ctx.scan_progress.is_some() && ctx.tree.is_none();

    if is_scanning {
        match ctx.scan_view {
            ScanView::Progress => render_scanning(ctx, content, buf),
            ScanView::Errors => render_errors(ctx, content, buf),
        }
    } else {
        match ctx.view {
            View::Explorer => render_explorer(ctx, content, buf),
            View::Duplicates => render_duplicates(ctx, content, buf),
            View::Age => render_age(ctx, content, buf),
            View::Errors => render_errors(ctx, content, buf),
        }
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
        _ => {}
    }
}

fn render_header(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let title = Span::styled(
        " gravityfile ",
        ctx.theme.title.add_modifier(Modifier::BOLD),
    );

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

    let line = Line::from(vec![title, Span::raw(" "), stats_span, status, clipboard_status]);

    Paragraph::new(line)
        .style(ctx.theme.header)
        .render(area, buf);
}

fn render_tabs(ctx: &RenderContext, area: Rect, buf: &mut Buffer) {
    let is_scanning = ctx.scan_progress.is_some() && ctx.tree.is_none();

    if is_scanning {
        let error_count = ctx.scan_progress.map(|p| p.errors_count).unwrap_or(0);
        let titles = vec![
            " Progress ".to_string(),
            if error_count > 0 {
                format!(" Errors ({}) ", error_count)
            } else {
                " Errors ".to_string()
            },
        ];

        let tabs = Tabs::new(titles)
            .select(ctx.scan_view as usize)
            .style(ctx.theme.footer)
            .highlight_style(ctx.theme.selected);

        tabs.render(area, buf);
    } else {
        let titles: Vec<String> = View::iter().map(|v| format!(" {} ", v)).collect();

        let tabs = Tabs::new(titles)
            .select(ctx.view as usize)
            .style(ctx.theme.footer)
            .highlight_style(ctx.theme.selected);

        tabs.render(area, buf);
    }
}

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
                );

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
            let msg = Paragraph::new("No duplicate files found in this directory.")
                .style(Style::default().fg(ctx.theme.muted));
            msg.render(inner, buf);
            return;
        }

        let header = format!(
            " {} groups, {} wasted",
            filtered_groups.len(),
            format_size(*total_wasted)
        );
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

        let selected = ctx
            .selected_dup_group
            .min(filtered_groups.len().saturating_sub(1));

        let scroll_offset = if selected >= visible_height {
            selected - visible_height + 1
        } else {
            0
        };

        for (i, group) in filtered_groups
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(visible_height)
        {
            let y = list_area.y + (i - scroll_offset) as u16;
            let is_selected = i == selected;

            let files_in_view = group
                .paths
                .iter()
                .filter(|p| p.starts_with(ctx.view_root))
                .count();
            let total_files = group.count();

            let file_info = if files_in_view < total_files {
                format!("{}/{} files", files_in_view, total_files)
            } else {
                format!("{} files", total_files)
            };

            let line = format!(
                " {}, {} each ({} wasted)",
                file_info,
                format_size(group.size),
                format_size(group.wasted_bytes)
            );

            let style = if is_selected {
                ctx.theme.selected
            } else {
                Style::default()
            };

            let line = Line::styled(line, style);
            let line_area = Rect::new(list_area.x, y, list_area.width, 1);
            Paragraph::new(line).render(line_area, buf);
        }
    } else {
        let msg = Paragraph::new("Analyzing duplicates...")
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
    let title = if ctx.mode == AppMode::Scanning {
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
        let lines = if ctx.mode == AppMode::Scanning {
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
            v
        }
        View::Duplicates | View::Age => {
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

    keys.extend([("?", "Help"), ("q", "Quit")]);

    let spans: Vec<Span> = keys
        .iter()
        .flat_map(|(key, desc)| {
            vec![
                Span::styled(format!(" {} ", key), ctx.theme.help_key),
                Span::styled(format!("{} ", desc), ctx.theme.help_desc),
            ]
        })
        .collect();

    let line = Line::from(spans);

    Paragraph::new(line)
        .style(ctx.theme.footer)
        .render(area, buf);
}
