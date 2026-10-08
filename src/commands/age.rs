//! `grav age`: how old the files are, and which directories went stale.

use std::io::Write;

use color_eyre::Result;
use color_eyre::eyre::{Context as _, eyre};
use gravityfile_analyze::{AgeAnalyzer, AgeConfig, AgeReport, StaleDirectoryStatus, format_age};
use gravityfile_core::escape_filename;

use super::{Context, counted};
use crate::cli::{AgeCommand, Format};
use crate::report::{Completeness, ReportEnvelope, ReportWriter, completeness, write_json_report};
use crate::style::paint;

pub fn run(command: &AgeCommand, context: &Context) -> Result<Completeness> {
    let tree = super::scan_tree(&command.scan, context)?;
    let config = AgeConfig::builder()
        .stale_threshold(command.stale)
        .max_stale_dirs(if command.limit == 0 {
            usize::MAX
        } else {
            command.limit
        })
        .build()
        .wrap_err("invalid age-analysis configuration")?;
    let parameters = serde_json::to_value(&config)?;
    let report = AgeAnalyzer::with_config(config)
        .map_err(|error| eyre!(error))?
        .analyze(&tree, context.cancel.clone());
    let mut stdout = std::io::stdout().lock();
    match command.output.format {
        Format::Json => {
            let mut envelope = ReportEnvelope::new("age", &tree, &report.status, &report);
            envelope.parameters = parameters;
            write_json_report(&envelope, &mut stdout, &context.cancel)?;
        }
        Format::Text => {
            write_text(
                &report,
                &tree.root_path,
                command.stale,
                &mut stdout,
                context,
            )?;
        }
    }
    Ok(completeness(&report.status, &context.err))
}

pub(crate) fn write_text(
    report: &AgeReport,
    root: &std::path::Path,
    stale: std::time::Duration,
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
        paint(styles.size, context.size(report.total_size))
    )?;
    writeln!(
        out,
        "{}",
        paint(
            styles.dim,
            format!(
                "{}, average age {}",
                counted(report.total_files, "file", "files"),
                format_age(report.average_age)
            )
        )
    )?;
    writeln!(out)?;
    const BAR: usize = 20;
    let label_width = report
        .buckets
        .iter()
        .map(|bucket| bucket.name.chars().count())
        .max()
        .unwrap_or(0);
    for bucket in &report.buckets {
        let filled = if report.total_files > 0 {
            (bucket.file_count as f64 / report.total_files as f64 * BAR as f64).round() as usize
        } else {
            0
        };
        writeln!(
            out,
            "{:<label_width$}  {:>9}  {:>12}  {}",
            crate::terminal::escape(&bucket.name),
            context.size(bucket.total_size),
            counted(bucket.file_count, "file", "files"),
            paint(styles.directory, "█".repeat(filled))
        )?;
    }
    if report.stale_directory_status != StaleDirectoryStatus::Evaluated {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            paint(
                styles.warning,
                format!(
                    "Stale-directory recommendations {}",
                    report.stale_directory_status
                )
            )
        )?;
    }
    if !report.stale_directories.is_empty() {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            paint(
                styles.heading,
                format!(
                    "Untouched for over {}, {} in total:",
                    format_age(stale),
                    context.size(report.total_stale_size())
                )
            )
        )?;
        for directory in &report.stale_directories {
            writeln!(
                out,
                "{}  {}  {}",
                paint(styles.size, format!("{:>9}", context.size(directory.size))),
                paint(
                    styles.dim,
                    format!("newest {:>10}", format_age(directory.newest_file_age))
                ),
                paint(
                    styles.directory,
                    escape_filename(directory.path.as_os_str())
                )
            )?;
        }
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gravityfile_analyze::{DuplicateConfig, DuplicateFinder};
    use gravityfile_core::ScanConfig;
    use gravityfile_scan::JwalkScanner;
    use std::path::Path;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn analysis_text_reports_escape_names_and_stop_on_output_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one"), b"same content").unwrap();
        std::fs::write(directory.path().join("two"), b"same content").unwrap();
        let tree = JwalkScanner::new()
            .scan(&ScanConfig::new(directory.path()), CancellationToken::new())
            .unwrap();
        let duplicates = DuplicateFinder::with_config(
            DuplicateConfig::builder().min_size(0u64).build().unwrap(),
        )
        .unwrap()
        .find_duplicates(&tree, CancellationToken::new());
        let ages = AgeAnalyzer::new().analyze(&tree, CancellationToken::new());
        let stale = std::time::Duration::from_secs(86_400);
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
        for age in [false, true] {
            let context = Context::plain(CancellationToken::new());
            let mut writer = CancelAfterLine {
                token: context.cancel.clone(),
                bytes: Vec::new(),
            };
            let result = if age {
                write_text(&ages, directory.path(), stale, &mut writer, &context)
            } else {
                crate::commands::duplicates::write_text(
                    &duplicates,
                    directory.path(),
                    &mut writer,
                    &context,
                )
            };
            assert!(result.is_err());
            assert_eq!(
                writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
                1
            );

            let context = Context::plain(CancellationToken::new());
            let mut output = Vec::new();
            let path = Path::new("report\n\u{1b}[31m");
            if age {
                write_text(&ages, path, stale, &mut output, &context).unwrap();
            } else {
                crate::commands::duplicates::write_text(&duplicates, path, &mut output, &context)
                    .unwrap();
            }
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains(r"report\n\u{1b}[31m"), "{output}");
            assert!(!output.contains('\u{1b}'));
            if age {
                assert!(output.contains("2 files"), "{output}");
            } else {
                assert!(
                    output.contains("1 group of identical files, 2 files with copies"),
                    "{output}"
                );
            }
        }
    }
}
