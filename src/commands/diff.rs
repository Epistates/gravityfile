//! `grav diff`: what changed between two scans of the same place.

use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

use color_eyre::Result;
use color_eyre::eyre::{Context as _, eyre};
use gravityfile_analyze::{
    AnalysisState, AnalysisStatus, Change, DiffEntry, DiffOptions, TreeDiff,
};
use gravityfile_core::{FileTree, ScanConfig, escape_filename};
use serde::Serialize;

use super::{Context, counted};
use crate::cli::{DiffCommand, Format};
use crate::report::{Completeness, ReportEnvelope, ReportWriter, completeness, write_json_report};
use crate::style::paint;

const DELTA_WIDTH: usize = 11;

pub fn run(command: &DiffCommand, context: &Context) -> Result<Completeness> {
    let (old, new) = load_sides(command, context)?;
    let options = DiffOptions {
        depth: command.depth,
        limit: command.limit,
    };
    let diff = gravityfile_analyze::diff(&old.tree.root, &new.tree.root, options, &context.cancel)
        .ok_or_else(|| eyre!("operation interrupted"))?;
    let status = status(&old.tree, &new.tree);
    let mut stdout = std::io::stdout().lock();
    match command.output.format {
        Format::Json => {
            let report = Report {
                old: old.summary(),
                new: new.summary(),
                diff: &diff,
            };
            let mut envelope = ReportEnvelope::new("diff", &new.tree, &status, &report);
            envelope.parameters = serde_json::json!({
                "depth": command.depth,
                "limit": command.limit,
            });
            write_json_report(&envelope, &mut stdout, &context.cancel)?;
        }
        Format::Text => write_text(&old, &new, &diff, &mut stdout, context)?,
    }
    Ok(completeness(&status, &context.err))
}

/// One side of the comparison and where it came from.
struct Side {
    tree: FileTree,
    snapshot: bool,
}

impl Side {
    fn summary(&self) -> SideSummary<'_> {
        SideSummary {
            root: &self.tree.root_path,
            scanned_at: self.tree.scanned_at,
            snapshot: self.snapshot,
            size: self.tree.root.size,
            files: self.tree.stats.total_files,
        }
    }
}

#[derive(Serialize)]
struct SideSummary<'a> {
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    root: &'a Path,
    scanned_at: SystemTime,
    /// Read from a saved snapshot rather than scanned now.
    snapshot: bool,
    size: u64,
    files: u64,
}

#[derive(Serialize)]
struct Report<'a> {
    old: SideSummary<'a>,
    new: SideSummary<'a>,
    #[serde(flatten)]
    diff: &'a TreeDiff,
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Read both sides. A directory is scanned with the other side's recorded
/// scope when that side is a snapshot, so the comparison shows changes on
/// disk rather than differences in what was counted.
fn load_sides(command: &DiffCommand, context: &Context) -> Result<(Side, Side)> {
    let read = |path: &Path| -> Result<Side> {
        let options = gravityfile_scan::snapshot::ImportOptions {
            apparent: command.apparent_size,
            count_hard_links: command.count_links,
            max_entries: None,
        };
        let tree = gravityfile_scan::snapshot::read(path, &options)
            .wrap_err_with(|| format!("cannot load {}", escape_filename(path.as_os_str())))?;
        Ok(Side {
            tree,
            snapshot: true,
        })
    };
    let scan = |path: &Path, scope: Option<&ScanConfig>| -> Result<Side> {
        let root = path
            .canonicalize()
            .wrap_err_with(|| format!("cannot open {}", escape_filename(path.as_os_str())))?;
        let mut config = scope.cloned().unwrap_or_else(|| ScanConfig::new(&root));
        config.root = root.clone();
        config.apparent_size = command.apparent_size;
        config.count_hard_links = command.count_links;
        let tree = crate::progress::scan(&config, &context.cancel, context.units)
            .wrap_err_with(|| format!("cannot scan {}", escape_filename(root.as_os_str())))?;
        Ok(Side {
            tree,
            snapshot: false,
        })
    };

    let old = is_file(&command.old)
        .then(|| read(&command.old))
        .transpose()?;
    let new_path = match (&command.new, &old) {
        (Some(new), _) => new.clone(),
        // Compare a snapshot with the same place today.
        (None, Some(old)) => old.tree.root_path.clone(),
        (None, None) => {
            return Err(eyre!(
                "give a second directory or snapshot to compare {} with",
                escape_filename(command.old.as_os_str())
            ));
        }
    };
    let new = if is_file(&new_path) {
        read(&new_path)?
    } else {
        scan(&new_path, old.as_ref().map(|old| &old.tree.config))?
    };
    let old = match old {
        Some(old) => old,
        None => scan(&command.old, new.snapshot.then_some(&new.tree.config))?,
    };
    Ok((old, new))
}

/// Partial when either side could not be read completely.
fn status(old: &FileTree, new: &FileTree) -> AnalysisStatus {
    let warnings = old.warning_count().saturating_add(new.warning_count());
    AnalysisStatus {
        state: if warnings > 0 {
            AnalysisState::Partial
        } else {
            AnalysisState::Complete
        },
        scan_warning_count: warnings,
        ..Default::default()
    }
}

/// A signed size: `+3.20 GB`, `-150 MB`, `0 B`.
fn signed(delta: i64, context: &Context) -> String {
    let size = context.size(delta.unsigned_abs());
    match delta.signum() {
        1 => format!("+{size}"),
        -1 => format!("-{size}"),
        _ => size,
    }
}

fn describe(side: &Side) -> String {
    let when: chrono::DateTime<chrono::Local> = side.tree.scanned_at.into();
    if side.snapshot {
        format!("a snapshot taken {}", when.format("%Y-%m-%d %H:%M"))
    } else {
        "a scan now".to_string()
    }
}

fn write_text(
    old: &Side,
    new: &Side,
    diff: &TreeDiff,
    destination: &mut impl Write,
    context: &Context,
) -> Result<()> {
    let styles = &context.out;
    let mut out = ReportWriter {
        output: destination,
        cancel: &context.cancel,
    };
    let same_root = old.tree.root_path == new.tree.root_path;
    let heading = if same_root {
        escape_filename(new.tree.root_path.as_os_str()).into_owned()
    } else {
        format!(
            "{} → {}",
            escape_filename(old.tree.root_path.as_os_str()),
            escape_filename(new.tree.root_path.as_os_str())
        )
    };
    writeln!(
        out,
        "{}  {} → {}  {}",
        paint(styles.heading, heading),
        context.size(diff.old_size),
        paint(styles.size, context.size(diff.new_size)),
        paint(
            change_style(diff.delta, styles),
            signed(diff.delta, context)
        ),
    )?;
    let files = |side: &Side| counted(side.tree.stats.total_files, "file", "files");
    writeln!(
        out,
        "{}",
        paint(
            styles.dim,
            format!(
                "from {} ({}) to {} ({})",
                describe(old),
                files(old),
                describe(new),
                files(new)
            )
        )
    )?;
    if diff.entries.is_empty() && diff.omitted.is_none() {
        writeln!(out, "\n{}", paint(styles.dim, "No changes in size."))?;
        out.flush()?;
        return Ok(());
    }
    writeln!(out)?;
    // Omission lines follow the listed changes of their directory.
    let mut pending: Vec<(u32, gravityfile_analyze::DiffOmitted)> = diff
        .omitted
        .map(|omitted| (0, omitted))
        .into_iter()
        .collect();
    for entry in &diff.entries {
        while let Some(&(depth, omitted)) = pending.last()
            && depth >= entry.depth
        {
            write_omitted(&mut out, depth, omitted, context)?;
            pending.pop();
        }
        write_entry(&mut out, entry, context)?;
        if let Some(omitted) = entry.omitted {
            pending.push((entry.depth, omitted));
        }
    }
    while let Some((depth, omitted)) = pending.pop() {
        write_omitted(&mut out, depth, omitted, context)?;
    }
    out.flush()?;
    Ok(())
}

/// Growth stands out as a warning; freed space does not.
fn change_style(delta: i64, styles: &crate::style::Styles) -> anstyle::Style {
    if delta > 0 {
        styles.warning
    } else {
        styles.size
    }
}

fn write_entry(out: &mut impl Write, entry: &DiffEntry, context: &Context) -> std::io::Result<()> {
    let styles = &context.out;
    let name = entry
        .path
        .file_name()
        .map(|name| escape_filename(name).into_owned())
        .unwrap_or_default();
    let name = if entry.is_dir {
        paint(styles.directory, format!("{name}/"))
    } else {
        name
    };
    let detail = match (entry.change, entry.old_size, entry.new_size) {
        (Change::Added, _, _) => "new".to_string(),
        (Change::Removed, _, _) => "removed".to_string(),
        (Change::Changed, Some(old), Some(new)) => {
            format!("{} → {}", context.size(old), context.size(new))
        }
        _ => String::new(),
    };
    writeln!(
        out,
        "{}  {}{name}  {}",
        paint(
            change_style(entry.delta, styles),
            format!("{:>DELTA_WIDTH$}", signed(entry.delta, context))
        ),
        "  ".repeat(entry.depth as usize - 1),
        paint(styles.dim, detail),
    )
}

fn write_omitted(
    out: &mut impl Write,
    parent_depth: u32,
    omitted: gravityfile_analyze::DiffOmitted,
    context: &Context,
) -> std::io::Result<()> {
    writeln!(
        out,
        "{:DELTA_WIDTH$}  {}{}",
        "",
        "  ".repeat(parent_depth as usize),
        paint(
            context.out.dim,
            format!(
                "… {} more, {}",
                counted(omitted.entries as u64, "change", "changes"),
                signed(omitted.delta, context)
            )
        )
    )
}
