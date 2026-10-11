//! `grav scan`: the largest entries, as a bounded tree.

use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

use color_eyre::Result;
use color_eyre::eyre::eyre;
use gravityfile_core::units::format_count;
use gravityfile_core::{FileNode, FileTree, GitStatus, NodeKind, escape_filename};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::{Context, counted};
use crate::cli::{Format, ScanCommand};
use crate::report::{
    Completeness, ReportEnvelope, ReportWriter, completeness, scan_status, write_json_report,
};
use crate::style::paint;

pub fn run(command: &ScanCommand, context: &Context) -> Result<Completeness> {
    let snapshot = super::is_snapshot(&command.scan);
    if command.record && snapshot {
        return Err(eyre!("--record needs a directory, not a snapshot"));
    }
    let tree = super::scan_tree(&command.scan, context)?;
    if command.record {
        super::history::record(&tree)?;
    }
    let view = View::build(&tree, command.depth, command.limit, &context.cancel)?;
    let status = scan_status(&tree);
    let mut stdout = std::io::stdout().lock();
    match command.output.format {
        Format::Json => {
            let mut envelope = ReportEnvelope::new("scan", &tree, &status, &view);
            envelope.parameters = serde_json::json!({
                "depth": command.depth,
                "limit": command.limit,
            });
            write_json_report(&envelope, &mut stdout, &context.cancel)?;
        }
        Format::Text => write_text(&tree, &view, snapshot, &mut stdout, context)?,
    }
    Ok(completeness(&status, &context.err))
}

/// The entries shown for a scan: the root's totals, then a depth-first list of
/// the largest entries down to the requested depth.
#[derive(Debug, Serialize)]
pub struct View<'a> {
    pub total: Totals,
    pub entries: Vec<Entry<'a>>,
    /// Root children beyond the limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted: Option<Omitted>,
}

#[derive(Debug, Serialize)]
pub struct Totals {
    pub size: u64,
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
}

#[derive(Debug, Serialize)]
pub struct Entry<'a> {
    /// Path relative to the scanned root.
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    pub path: PathBuf,
    pub depth: u32,
    pub kind: Kind,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directories: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'a str>,
    pub modified: SystemTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitStatus>,
    /// Children beyond the limit, when this directory's children are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted: Option<Omitted>,
    #[serde(skip)]
    node: &'a FileNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Omitted {
    pub entries: usize,
    pub size: u64,
}

impl<'a> View<'a> {
    /// `limit == 0` lists every child.
    pub fn build(
        tree: &'a FileTree,
        depth: u32,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        let shown = |node: &'a FileNode| {
            let count = if limit == 0 {
                node.children.len()
            } else {
                limit.min(node.children.len())
            };
            let rest = &node.children[count..];
            let omitted = (!rest.is_empty()).then(|| Omitted {
                entries: rest.len(),
                size: rest.iter().map(|child| child.size).sum(),
            });
            (node.children[..count].iter(), omitted)
        };
        let root = &tree.root;
        let mut view = View {
            total: Totals {
                size: root.size,
                files: tree.stats.total_files,
                directories: tree.stats.total_dirs.saturating_sub(1),
                symlinks: tree.stats.total_symlinks,
            },
            entries: Vec::new(),
            omitted: None,
        };
        if depth == 0 {
            return Ok(view);
        }
        let (children, omitted) = shown(root);
        view.omitted = omitted;
        // One iterator per open directory, rather than recursion, so deep trees
        // cannot exhaust the stack.
        let mut path = PathBuf::new();
        let mut open = vec![children];
        while let Some(children) = open.last_mut() {
            if cancel.is_cancelled() {
                return Err(eyre!("operation interrupted"));
            }
            let Some(node) = children.next() else {
                open.pop();
                path.pop();
                continue;
            };
            let level = open.len() as u32;
            let expand = level < depth && node.is_dir();
            let (grandchildren, omitted) = if expand {
                let (children, omitted) = shown(node);
                (Some(children), omitted)
            } else {
                (None, None)
            };
            view.entries.push(Entry::new(
                node,
                path.join(node.file_name()),
                level,
                omitted,
            ));
            if let Some(grandchildren) = grandchildren {
                path.push(node.file_name());
                open.push(grandchildren);
            }
        }
        Ok(view)
    }
}

impl<'a> Entry<'a> {
    fn new(node: &'a FileNode, path: PathBuf, depth: u32, omitted: Option<Omitted>) -> Self {
        let (kind, files, directories, target) = match &node.kind {
            NodeKind::Directory {
                file_count,
                dir_count,
            } => (Kind::Directory, Some(*file_count), Some(*dir_count), None),
            NodeKind::File { .. } => (Kind::File, None, None, None),
            NodeKind::Symlink { target, .. } => (Kind::Symlink, None, None, Some(&**target)),
            NodeKind::Other => (Kind::Other, None, None, None),
        };
        Self {
            path,
            depth,
            kind,
            size: node.size,
            files,
            directories,
            target,
            modified: node.timestamps.modified,
            git: node.git_status,
            omitted,
            node,
        }
    }
}

const SIZE_WIDTH: usize = 9;

/// `snapshot`: the tree was loaded from a saved snapshot, not scanned now.
fn write_text(
    tree: &FileTree,
    view: &View<'_>,
    snapshot: bool,
    destination: &mut impl Write,
    context: &Context,
) -> Result<()> {
    let styles = &context.out;
    let mut out = ReportWriter {
        output: destination,
        cancel: &context.cancel,
    };
    writeln!(
        out,
        "{}  {}",
        paint(styles.heading, escape_filename(tree.root_path.as_os_str())),
        paint(styles.size, context.size(view.total.size))
    )?;
    let when = if snapshot {
        let saved: chrono::DateTime<chrono::Local> = tree.scanned_at.into();
        format!("from a snapshot taken {}", saved.format("%Y-%m-%d %H:%M"))
    } else {
        format!("scanned in {:.2}s", tree.scan_duration.as_secs_f64())
    };
    writeln!(
        out,
        "{}",
        paint(
            styles.dim,
            format!(
                "{}, {}, {when}",
                counted(view.total.files, "file", "files"),
                counted(view.total.directories, "directory", "directories"),
            )
        )
    )?;
    if view.entries.is_empty() && view.omitted.is_none() {
        return Ok(());
    }
    writeln!(out)?;
    // Omission lines follow the listed children of their directory.
    let mut pending: Vec<(u32, Omitted)> = view.omitted.map(|o| (0, o)).into_iter().collect();
    let mut omitted_any = view.omitted.is_some();
    for entry in &view.entries {
        while let Some(&(depth, omitted)) = pending.last()
            && depth >= entry.depth
        {
            write_omitted(&mut out, depth, omitted, context)?;
            pending.pop();
        }
        write_entry(&mut out, entry, view.total.size, context)?;
        if let Some(omitted) = entry.omitted {
            pending.push((entry.depth, omitted));
            omitted_any = true;
        }
    }
    while let Some((depth, omitted)) = pending.pop() {
        write_omitted(&mut out, depth, omitted, context)?;
    }
    if omitted_any {
        writeln!(
            out,
            "{}",
            paint(
                styles.dim,
                "\nUse --limit 0 to list every entry, --depth N to go deeper."
            )
        )?;
    }
    out.flush()?;
    Ok(())
}

fn write_entry(
    out: &mut impl Write,
    entry: &Entry<'_>,
    total: u64,
    context: &Context,
) -> std::io::Result<()> {
    let styles = &context.out;
    let share = if total > 0 {
        entry.size as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let name = entry.node.display_name();
    let label = match entry.kind {
        Kind::Directory => paint(styles.directory, format!("{name}/")),
        Kind::Symlink => format!(
            "{} {}",
            paint(styles.link, &name),
            paint(
                styles.dim,
                format!(
                    "-> {}",
                    crate::terminal::escape(entry.target.unwrap_or_default())
                )
            )
        ),
        Kind::File | Kind::Other => name.into_owned(),
    };
    writeln!(
        out,
        "{}  {}  {}{label}",
        paint(
            styles.size,
            format!("{:>SIZE_WIDTH$}", context.size(entry.size))
        ),
        paint(styles.dim, format!("{share:>5.1}%")),
        "  ".repeat(entry.depth as usize - 1),
    )
}

fn write_omitted(
    out: &mut impl Write,
    parent_depth: u32,
    omitted: Omitted,
    context: &Context,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{:SIZE_WIDTH$}  {:6}  {}{}",
        "",
        "",
        "  ".repeat(parent_depth as usize),
        paint(
            context.out.dim,
            format!(
                "… {} more, {}",
                format_count(omitted.entries as u64),
                context.size(omitted.size)
            )
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gravityfile_core::{ScanConfig, Timestamps, TreeStats};

    fn tree(root: FileNode) -> FileTree {
        FileTree::new(
            root,
            PathBuf::from("/root"),
            ScanConfig::new("/root"),
            TreeStats::new(),
            std::time::Duration::ZERO,
            Vec::new(),
        )
    }

    fn directory(name: &str, size: u64, children: Vec<FileNode>) -> FileNode {
        let mut node =
            FileNode::new_directory(name, Timestamps::with_modified(SystemTime::UNIX_EPOCH));
        node.size = size;
        node.children = children;
        node
    }

    fn file(name: &str, size: u64) -> FileNode {
        FileNode::new_file(
            name,
            size,
            0,
            Timestamps::with_modified(SystemTime::UNIX_EPOCH),
            false,
        )
    }

    fn render(tree: &FileTree, depth: u32, limit: usize) -> String {
        let context = Context::plain(CancellationToken::new());
        let view = View::build(tree, depth, limit, &context.cancel).unwrap();
        let mut output = Vec::new();
        write_text(tree, &view, false, &mut output, &context).unwrap();
        String::from_utf8(output).unwrap()
    }

    fn sample() -> FileTree {
        tree(directory(
            "root",
            1000,
            vec![
                directory("child", 700, vec![file("big", 600), file("small", 100)]),
                file("loose", 300),
            ],
        ))
    }

    #[test]
    fn lists_largest_entries_with_omissions_after_their_children() {
        let text = render(&sample(), 2, 1);
        let lines: Vec<_> = text.lines().skip(3).collect();
        assert_eq!(lines[0], "    700 B   70.0%  child/");
        assert_eq!(lines[1], "    600 B   60.0%    big");
        assert_eq!(lines[2].trim(), "… 1 more, 100 B");
        assert!(lines[2].starts_with(&" ".repeat(SIZE_WIDTH + 2 + 6 + 2 + 2)));
        assert_eq!(lines[3].trim(), "… 1 more, 300 B");
        assert!(text.ends_with("--depth N to go deeper.\n"));
    }

    #[test]
    fn depth_and_limit_bound_the_listing() {
        let text = render(&sample(), 1, 0);
        assert!(text.contains("child/") && text.contains("loose"));
        assert!(!text.contains("big") && !text.contains("more,"));
        let text = render(&sample(), 0, 0);
        assert_eq!(text.lines().count(), 2, "{text}");
    }

    #[test]
    fn json_rows_carry_relative_paths_and_omissions() {
        let tree = sample();
        let view = View::build(&tree, 2, 1, &CancellationToken::new()).unwrap();
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["total"]["size"], 1000);
        assert_eq!(json["omitted"]["entries"], 1);
        assert_eq!(json["entries"][0]["path"], "child");
        assert_eq!(json["entries"][0]["kind"], "directory");
        assert_eq!(json["entries"][0]["omitted"]["size"], 100);
        assert_eq!(
            json["entries"][1]["path"],
            format!("child{}big", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(json["entries"][1]["depth"], 2);
        assert!(json["entries"][1].get("files").is_none());
    }

    #[test]
    fn deep_trees_render_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut node = directory("leaf", 1, Vec::new());
                for _ in 0..5000 {
                    node = directory("parent", 1, vec![node]);
                }
                let tree = tree(node);
                assert_eq!(render(&tree, u32::MAX, 0).lines().count(), 5000 + 3);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn output_stops_when_cancelled_mid_report() {
        struct CancelAfterLine {
            token: CancellationToken,
            bytes: Vec<u8>,
        }
        impl Write for CancelAfterLine {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                if bytes.contains(&b'\n') {
                    self.token.cancel();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let tree = sample();
        let context = Context::plain(CancellationToken::new());
        let view = View::build(&tree, 2, 0, &context.cancel).unwrap();
        let mut writer = CancelAfterLine {
            token: context.cancel.clone(),
            bytes: Vec::new(),
        };
        assert!(write_text(&tree, &view, false, &mut writer, &context).is_err());
        assert_eq!(
            writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        assert!(View::build(&tree, 2, 0, &context.cancel).is_err());
    }

    #[test]
    fn closed_output_is_reported_as_a_broken_pipe() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let tree = sample();
        let context = Context::plain(CancellationToken::new());
        let view = View::build(&tree, 1, 0, &context.cancel).unwrap();
        let error = write_text(&tree, &view, false, &mut Broken, &context).unwrap_err();
        assert!(crate::is_broken_pipe(&error));
    }

    #[test]
    fn hostile_names_are_escaped_in_text() {
        let tree = tree(directory("root", 2, vec![file("evil\n\u{1b}[31m", 2)]));
        let text = render(&tree, 1, 0);
        assert!(text.contains(r"evil\n\u{1b}[31m"), "{text}");
        assert!(!text.contains('\u{1b}'));
    }
}
