//! `grav duplicates`: files with identical contents.

use std::io::Write;

use color_eyre::Result;
use color_eyre::eyre::{Context as _, eyre};
use gravityfile_analyze::{DuplicateConfig, DuplicateFinder, DuplicateReport};
use gravityfile_core::escape_filename;

use super::{Context, counted};
use crate::cli::{DuplicatesCommand, Format};
use crate::report::{Completeness, ReportEnvelope, ReportWriter, completeness, write_json_report};
use crate::style::paint;

pub fn run(command: &DuplicatesCommand, context: &Context) -> Result<Completeness> {
    let tree = super::scan_tree(&command.scan, context)?;
    let config = DuplicateConfig::builder()
        .min_size(command.min_size)
        .max_groups(command.limit)
        .build()
        .wrap_err("invalid duplicate-analysis configuration")?;
    let parameters = serde_json::to_value(&config)?;
    let report = DuplicateFinder::with_config(config)
        .map_err(|error| eyre!(error))?
        .find_duplicates(&tree, context.cancel.clone());
    let mut stdout = std::io::stdout().lock();
    match command.output.format {
        Format::Json => {
            let mut envelope = ReportEnvelope::new("duplicates", &tree, &report.status, &report);
            envelope.parameters = parameters;
            write_json_report(&envelope, &mut stdout, &context.cancel)?;
        }
        Format::Text => write_text(&report, &tree.root_path, &mut stdout, context)?,
    }
    Ok(completeness(&report.status, &context.err))
}

pub(crate) fn write_text(
    report: &DuplicateReport,
    root: &std::path::Path,
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
        paint(styles.heading, escape_filename(root.as_os_str())),
        paint(
            styles.size,
            format!("{} wasted", context.size(report.total_wasted_space))
        )
    )?;
    writeln!(
        out,
        "{}",
        paint(
            styles.dim,
            format!(
                "{} of identical files, {} with copies, {} compared",
                counted(report.group_count as u64, "group", "groups"),
                counted(report.files_with_duplicates, "file", "files"),
                counted(report.files_analyzed, "file", "files")
            )
        )
    )?;
    for group in &report.groups {
        writeln!(out)?;
        writeln!(
            out,
            "{}  {}",
            paint(
                styles.size,
                format!("{} wasted", context.size(group.wasted_bytes))
            ),
            paint(
                styles.dim,
                format!(
                    "{} copies of {}",
                    group.paths.len(),
                    context.size(group.size)
                )
            )
        )?;
        for path in &group.paths {
            writeln!(out, "  {}", escape_filename(path.as_os_str()))?;
        }
    }
    if report.groups_omitted > 0 {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            paint(
                styles.dim,
                format!(
                    "… {} more. Use --limit 0 to list every group.",
                    counted(report.groups_omitted as u64, "group", "groups")
                )
            )
        )?;
    }
    out.flush()?;
    Ok(())
}
