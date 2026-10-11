//! `grav history`: a directory's size across recorded scans.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use color_eyre::Result;
use color_eyre::eyre::{Context as _, eyre};
use gravityfile_analyze::AnalysisStatus;
use gravityfile_core::escape_filename;
use gravityfile_core::history::{self, Record};
use serde::Serialize;

use super::{Context, counted};
use crate::cli::{Format, HistoryCommand};
use crate::report::{Completeness, ReportEnvelope, ReportWriter, write_json_report};
use crate::style::paint;

/// Where the history lives, or an explanation of why there is none.
pub fn history_file() -> Result<PathBuf> {
    gravityfile_core::paths::Dirs::discover()
        .map(|dirs| dirs.history_file())
        .ok_or_else(|| eyre!("no home directory to keep the scan history in"))
}

/// Add `tree`'s totals to the history.
pub fn record(tree: &gravityfile_core::FileTree) -> Result<()> {
    let file = history_file()?;
    history::append(&file, &Record::of(tree))
        .wrap_err_with(|| format!("cannot record the scan in {}", file.display()))
}

pub fn run(command: &HistoryCommand, context: &Context) -> Result<Completeness> {
    let file = history_file()?;
    let complete = AnalysisStatus {
        state: gravityfile_analyze::AnalysisState::Complete,
        ..AnalysisStatus::default()
    };
    let mut stdout = std::io::stdout().lock();
    if command.all {
        let records = history::read(&file, None)
            .wrap_err_with(|| format!("cannot read {}", file.display()))?;
        let roots = summarize(&records);
        match command.output.format {
            Format::Json => {
                let envelope =
                    ReportEnvelope::for_root("history", Path::new(""), &complete, &roots);
                write_json_report(&envelope, &mut stdout, &context.cancel)?;
            }
            Format::Text => write_roots(&roots, &mut stdout, context)?,
        }
        return Ok(Completeness::Complete);
    }
    let requested = command.path.clone().unwrap_or_else(|| PathBuf::from("."));
    // A directory that has since gone still has its history.
    let root = requested
        .canonicalize()
        .or_else(|_| std::path::absolute(&requested))
        .wrap_err_with(|| format!("cannot resolve {}", escape_filename(requested.as_os_str())))?;
    let records = history::read(&file, Some(&root))
        .wrap_err_with(|| format!("cannot read {}", file.display()))?;
    let shown = if command.limit == 0 {
        &records[..]
    } else {
        &records[records.len().saturating_sub(command.limit)..]
    };
    let rows = rows(&records, shown);
    match command.output.format {
        Format::Json => {
            let data = Scans { scans: &rows };
            let mut envelope = ReportEnvelope::for_root("history", &root, &complete, &data);
            envelope.parameters = serde_json::json!({ "limit": command.limit });
            write_json_report(&envelope, &mut stdout, &context.cancel)?;
        }
        Format::Text => write_scans(&root, records.len(), &rows, &mut stdout, context)?,
    }
    Ok(Completeness::Complete)
}

#[derive(Serialize)]
struct Scans<'a> {
    scans: &'a [Row<'a>],
}

/// One recorded scan and its change since the previous comparable one.
#[derive(Serialize)]
struct Row<'a> {
    #[serde(flatten)]
    record: &'a Record,
    /// Bytes gained since the previous scan measured the same way.
    #[serde(skip_serializing_if = "Option::is_none")]
    change: Option<i128>,
    /// The top-level entry that changed most, and by how much.
    #[serde(skip_serializing_if = "Option::is_none")]
    largest_change: Option<LargestChange<'a>>,
}

#[derive(Serialize)]
struct LargestChange<'a> {
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    name: &'a Path,
    change: i128,
}

fn rows<'a>(all: &'a [Record], shown: &'a [Record]) -> Vec<Row<'a>> {
    shown
        .iter()
        .map(|record| {
            let earlier = history::previous(all, record, Duration::ZERO);
            Row {
                record,
                change: earlier.map(|earlier| i128::from(record.size) - i128::from(earlier.size)),
                largest_change: earlier
                    .and_then(|earlier| record.largest_change(earlier))
                    .map(|(name, change)| LargestChange { name, change }),
            }
        })
        .collect()
}

/// One directory with recorded scans.
#[derive(Serialize)]
struct RootSummary<'a> {
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    root: &'a Path,
    scans: usize,
    first: SystemTime,
    last: SystemTime,
    /// The size at the latest scan.
    size: u64,
}

fn summarize(records: &[Record]) -> Vec<RootSummary<'_>> {
    let mut roots: Vec<RootSummary<'_>> = Vec::new();
    for record in records {
        match roots.iter_mut().find(|summary| summary.root == record.root) {
            Some(summary) => {
                summary.scans += 1;
                summary.last = record.at;
                summary.size = record.size;
            }
            None => roots.push(RootSummary {
                root: &record.root,
                scans: 1,
                first: record.at,
                last: record.at,
                size: record.size,
            }),
        }
    }
    roots.sort_by_key(|root| std::cmp::Reverse(root.last));
    roots
}

fn date(at: SystemTime) -> String {
    let at: chrono::DateTime<chrono::Local> = at.into();
    at.format("%Y-%m-%d %H:%M").to_string()
}

fn signed(change: i128, context: &Context) -> String {
    let size = context.size(u64::try_from(change.unsigned_abs()).unwrap_or(u64::MAX));
    match change.signum() {
        1 => format!("+{size}"),
        -1 => format!("-{size}"),
        _ => size,
    }
}

fn write_scans(
    root: &Path,
    total: usize,
    rows: &[Row<'_>],
    destination: &mut impl Write,
    context: &Context,
) -> Result<()> {
    let styles = &context.out;
    let mut out = ReportWriter {
        output: destination,
        cancel: &context.cancel,
    };
    let name = escape_filename(root.as_os_str());
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        writeln!(out, "{}", paint(styles.heading, &name))?;
        writeln!(
            out,
            "{}",
            paint(
                styles.dim,
                "No scans recorded yet. The explorer records each full scan, \
                 and `grav scan PATH --record` adds one."
            )
        )?;
        out.flush()?;
        return Ok(());
    };
    writeln!(
        out,
        "{}  {}",
        paint(styles.heading, &name),
        paint(
            styles.dim,
            format!(
                "{}, {} to {}",
                counted(total as u64, "scan", "scans"),
                date(first.record.at),
                date(last.record.at)
            )
        )
    )?;
    writeln!(out)?;
    for row in rows {
        let change = row.change.map(|change| {
            let style = if change > 0 {
                styles.warning
            } else {
                styles.size
            };
            paint(style, format!("{:>10}", signed(change, context)))
        });
        let largest = row
            .largest_change
            .as_ref()
            .map(|largest| {
                format!(
                    "{} {}",
                    escape_filename(largest.name.as_os_str()),
                    signed(largest.change, context)
                )
            })
            .unwrap_or_default();
        let measure = if row.record.apparent {
            "(apparent)"
        } else {
            ""
        };
        let note = [largest.as_str(), measure]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let mut line = format!(
            "{}  {}",
            date(row.record.at),
            paint(styles.size, format!("{:>9}", context.size(row.record.size))),
        );
        // Columns stay aligned; nothing trails an empty one.
        if change.is_some() || !note.is_empty() {
            line.push_str("  ");
            line.push_str(&change.unwrap_or_else(|| " ".repeat(10)));
        }
        if !note.is_empty() {
            line.push_str("  ");
            line.push_str(&paint(styles.dim, note));
        }
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    Ok(())
}

fn write_roots(
    roots: &[RootSummary<'_>],
    destination: &mut impl Write,
    context: &Context,
) -> Result<()> {
    let styles = &context.out;
    let mut out = ReportWriter {
        output: destination,
        cancel: &context.cancel,
    };
    if roots.is_empty() {
        writeln!(out, "{}", paint(styles.dim, "No scans recorded yet."))?;
    }
    for root in roots {
        writeln!(
            out,
            "{}  {}  {}",
            paint(styles.size, format!("{:>9}", context.size(root.size))),
            paint(styles.heading, escape_filename(root.root.as_os_str())),
            paint(
                styles.dim,
                format!(
                    "{}, latest {}",
                    counted(root.scans as u64, "scan", "scans"),
                    date(root.last)
                )
            )
        )?;
    }
    out.flush()?;
    Ok(())
}
