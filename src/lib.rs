//! gravityfile - A state-of-the-art file system analyzer with TUI.
//!
//! Usage:
//!
//! ```text
//! grav [PATH]              Launch interactive TUI
//! grav scan [PATH]         Quick scan summary
//! grav duplicates [PATH]   Find duplicate files
//! grav age [PATH]          Analyze file ages
//! ```

mod atomic_export;
mod bounded_log;
#[cfg(test)]
mod duplicate_bench;
mod shutdown;
mod terminal;
mod tree_json;

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use gravityfile_analyze::{
    AgeAnalyzer, AgeConfig, AnalysisStatus, DuplicateConfig, DuplicateFinder,
};
use gravityfile_core::{ScanConfig, escape_filename};
use gravityfile_scan::JwalkScanner;
use humansize::{BINARY, format_size as format_size_human};
use tokio_util::sync::CancellationToken;

// Local CLI output macro: propagate errors so pipelines can close stdout safely.
macro_rules! println {
    () => { writeln!(std::io::stdout().lock())? };
    ($($arg:tt)*) => { writeln!(std::io::stdout().lock(), "{}", terminal::escape(&format!($($arg)*)))? };
}

// Diagnostics are best-effort: a failed stderr must not panic or replace the
// operation's outcome. Required report/acknowledgement output uses fallible writes.
macro_rules! eprintln {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr().lock(), "{}", terminal::escape(&format!($($arg)*)));
    }};
}

struct OutputPolicy {
    strict: bool,
    envelope: bool,
}

#[derive(serde::Serialize)]
struct ReportEnvelope<'a, T> {
    schema_version: u32,
    tool_version: &'static str,
    kind: &'static str,
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    root: &'a Path,
    scope: &'a ScanConfig,
    parameters: serde_json::Value,
    status: &'a AnalysisStatus,
    data: &'a T,
}

impl<'a, T> ReportEnvelope<'a, T> {
    fn new(
        kind: &'static str,
        tree: &'a gravityfile_core::FileTree,
        status: &'a AnalysisStatus,
        data: &'a T,
    ) -> Self {
        Self {
            schema_version: 1,
            tool_version: env!("CARGO_PKG_VERSION"),
            kind,
            root: &tree.root_path,
            scope: &tree.config,
            parameters: serde_json::Value::Null,
            status,
            data,
        }
    }
}

fn scan_status(tree: &gravityfile_core::FileTree) -> AnalysisStatus {
    AnalysisStatus {
        state: if tree.has_warnings() {
            gravityfile_analyze::AnalysisState::Partial
        } else {
            gravityfile_analyze::AnalysisState::Complete
        },
        scan_warning_count: tree.warning_count(),
        ..Default::default()
    }
}

fn check_completeness(policy: &OutputPolicy, status: &AnalysisStatus) -> Result<()> {
    if !status.is_complete() {
        eprintln!(
            "Report {}: {} scan warning(s), {} analysis failure(s)",
            status.state, status.scan_warning_count, status.issue_count
        );
        for issue in &status.issues {
            eprintln!(
                "  {}: {}",
                escape_filename(issue.path.as_os_str()),
                issue.message
            );
        }
        if policy.strict {
            return Err(eyre!("incomplete report (--strict)"));
        }
    }
    Ok(())
}

// Preserve diagnostic escaping while routing text reports through an injected
// writer, including cancellation and output errors.
macro_rules! reportln {
    ($output:expr) => { writeln!($output)? };
    ($output:expr, $($arg:tt)*) => {
        writeln!($output, "{}", terminal::escape(&format!($($arg)*)))?
    };
}

/// Check cancellation at buffered I/O boundaries without using Interrupted,
/// which Write::write_all retries and could otherwise loop forever.
struct ReportWriter<'a, W: Write> {
    output: &'a mut W,
    cancel: &'a CancellationToken,
}

impl<W: Write> Write for ReportWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        check_report_cancel(self.cancel)?;
        self.output.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        check_report_cancel(self.cancel)?;
        self.output.flush()
    }
}

fn check_report_cancel(cancel: &CancellationToken) -> std::io::Result<()> {
    if cancel.is_cancelled() {
        Err(std::io::Error::other("report output cancelled"))
    } else {
        Ok(())
    }
}

fn write_json_stdout(value: &impl serde::Serialize, cancel: &CancellationToken) -> Result<()> {
    write_json_report(value, &mut std::io::stdout().lock(), cancel)
}

/// Finish serialization before publishing any report bytes. Cancellation before
/// publication leaves stdout untouched; cancellation during copying can leave
/// a prefix, as with any interrupted output stream.
fn write_json_report(
    value: &impl serde::Serialize,
    destination: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<()> {
    write_json_report_with(destination, cancel, |writer| {
        serde_json::to_writer_pretty(writer, value)?;
        Ok(())
    })
}

fn write_json_report_with(
    destination: &mut impl Write,
    cancel: &CancellationToken,
    encode: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    check_report_cancel(cancel)?;
    let mut staged = tempfile::spooled_tempfile(1024 * 1024);
    {
        let mut writer = BufWriter::new(ReportWriter {
            output: &mut staged,
            cancel,
        });
        encode(&mut writer)?;
        writeln!(writer)?;
        writer.flush()?;
    }
    check_report_cancel(cancel)?;
    staged.rewind()?;
    let mut destination = ReportWriter {
        output: destination,
        cancel,
    };
    std::io::copy(&mut staged, &mut destination)?;
    destination.flush()?;
    Ok(())
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(name = "grav")]
struct Cli {
    /// Path to scan (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Start a full recursive scan immediately on startup
    #[arg(short, long)]
    scan: bool,

    /// Write final CWD to file on exit
    #[arg(long)]
    cwd_file: Option<PathBuf>,

    /// Print final CWD to stdout on exit
    #[arg(long)]
    print_cwd: bool,

    /// Exit unsuccessfully after emitting results if scan or analysis is incomplete
    #[arg(long, global = true)]
    strict: bool,

    /// Emit versioned JSON with report kind, scope, status, and data
    #[arg(long, global = true)]
    envelope: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Args)]
struct ScanOptions {
    /// Fail if traversal exceeds N descendant entries (default: unlimited)
    #[arg(long, value_name = "N")]
    max_entries: Option<u64>,
}

impl ScanOptions {
    fn config(&self, path: &Path) -> ScanConfig {
        let mut config = ScanConfig::new(path);
        config.max_entries = self.max_entries;
        config
    }
}

#[derive(Subcommand)]
enum Command {
    #[command(name = "__plugin-worker", hide = true)]
    PluginWorker { language: String },
    /// Inspect overwrite/copy-undo recovery records, or restore saved content exclusively
    Recover {
        /// Directory to inspect, or a specific transaction with --restore-original
        path: PathBuf,
        /// Restore the saved original only if its destination is unoccupied
        #[arg(long, conflicts_with = "restore_payload_to")]
        restore_original: bool,
        /// Restore a staged payload to this explicitly chosen, unoccupied path
        #[arg(long)]
        restore_payload_to: Option<PathBuf>,
        /// Restore an interrupted bulk-rename batch after validating all identities
        #[arg(long, conflicts_with_all = ["restore_original", "restore_payload_to", "json"])]
        rollback_rename: bool,
        /// Retain interrupted extraction output in its journal without deleting data
        #[arg(long, conflicts_with_all = ["restore_original", "restore_payload_to", "rollback_rename", "json"])]
        retain_extraction: bool,
        /// Emit a versioned inspection report with native paths and completeness
        #[arg(long, conflicts_with_all = ["restore_original", "restore_payload_to"])]
        json: bool,
    },
    /// Quick scan summary
    Scan {
        #[command(flatten)]
        scan_options: ScanOptions,
        /// Path to scan
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Maximum depth to display
        #[arg(short, long)]
        max_depth: Option<u32>,
        /// Number of top items to show per directory
        #[arg(short, long, default_value = "5")]
        top: usize,
    },
    /// Find duplicate files
    Duplicates {
        #[command(flatten)]
        scan_options: ScanOptions,
        /// Path to scan
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Minimum file size to consider (e.g. 1MB, 100KB)
        #[arg(short, long, default_value = "1MB")]
        min_size: String,
        /// Maximum number of groups to show
        #[arg(short, long, default_value = "20")]
        top: usize,
        /// Output format
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
    },
    /// Analyze file ages
    Age {
        #[command(flatten)]
        scan_options: ScanOptions,
        /// Path to scan
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Stale threshold (e.g. 1y, 6m, 30d)
        #[arg(short, long, default_value = "1y")]
        stale: String,
        /// Output format
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
    },
    /// Export scan result to JSON
    Export {
        #[command(flatten)]
        scan_options: ScanOptions,
        /// Path to scan
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

/// Executable entry point with inert error-chain output and conventional exit codes.
///
/// `run` remains available to callers that need the original typed report.
pub fn main_entry() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            if let Some(parser_error) = error.downcast_ref::<clap::Error>() {
                let has_control_argument = std::env::args_os().any(|argument| {
                    let text = argument.to_string_lossy();
                    terminal::escape(&text) != text
                });
                let result = if parser_error.use_stderr() && has_control_argument {
                    writeln!(
                        std::io::stderr().lock(),
                        "{}",
                        terminal::escape(&parser_error.to_string())
                    )
                } else {
                    parser_error.print()
                };
                return if result.is_ok() {
                    std::process::ExitCode::from(parser_error.exit_code() as u8)
                } else {
                    std::process::ExitCode::FAILURE
                };
            }
            let _ = write_cli_failure(&error, &mut std::io::stderr().lock());
            std::process::ExitCode::FAILURE
        }
    }
}

fn write_cli_failure(error: &color_eyre::Report, output: &mut impl Write) -> std::io::Result<()> {
    // Alternate Display includes the cause chain; escape only after formatting
    // so third-party source messages cannot bypass the terminal boundary.
    writeln!(output, "Error: {}", terminal::escape(&format!("{error:#}")))
}

/// Run the gravityfile command-line application.
///
/// This process entry point installs logging and, for observation commands,
/// process-wide signal handlers. It is not an embeddable command dispatcher;
/// library consumers should use the scanner/analyzer APIs with their own token.
pub fn run() -> Result<()> {
    color_eyre::install()?;
    // Help, version, and argument errors must work without writable log storage.
    let cli = Cli::try_parse()?;
    if let Some(Command::PluginWorker { language }) = &cli.command {
        return gravityfile_plugin::run_worker(language).map_err(Into::into);
    }
    if cli.command.is_none() && (cli.strict || cli.envelope) {
        return Err(eyre!(
            "--strict and --envelope require a scan, duplicates, age, or export command"
        ));
    }
    let output = OutputPolicy {
        strict: cli.strict,
        envelope: cli.envelope,
    };

    // Initialize tracing
    let (log_file, file) = create_log_file()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::sync::Arc::new(bounded_log::BoundedLog::new(file)))
        .with_ansi(false)
        .try_init()
        .map_err(|error| eyre!("failed to initialize tracing: {error}"))?;

    tracing::info!("Starting gravityfile v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Log file: {}", escape_filename(log_file.as_os_str()));

    let observation = matches!(
        &cli.command,
        Some(
            Command::Scan { .. }
                | Command::Duplicates { .. }
                | Command::Age { .. }
                | Command::Export { .. }
        )
    );
    let shutdown = if observation {
        Some(shutdown::Shutdown::install()?)
    } else {
        None
    };
    let cancel = shutdown
        .as_ref()
        .map(shutdown::Shutdown::token)
        .unwrap_or_default();

    let result: Result<()> = (|| {
        match cli.command {
            Some(Command::PluginWorker { .. }) => return Err(eyre!("invalid worker dispatch")),
            Some(Command::Recover {
                path,
                restore_original,
                restore_payload_to,
                rollback_rename,
                retain_extraction,
                json,
            }) => {
                if output.envelope || output.strict {
                    return Err(eyre!("recover does not support --envelope or --strict"));
                }
                if retain_extraction {
                    let retained = gravityfile_ops::retain_archive_extraction(&path)?;
                    let mut writer = std::io::stdout().lock();
                    writeln!(writer, "Extraction output retained; inspect {}", escape_filename(retained.as_os_str()))
                        .and_then(|_| writer.flush())
                        .wrap_err("Recovery completed, but its acknowledgement could not be written; inspect retained data before retrying")?;
                } else if rollback_rename {
                    let restored = gravityfile_ops::restore_rename_batch(&path)?;
                    let mut writer = std::io::stdout().lock();
                    writeln!(writer, "Restored {} bulk rename items; retained records in {}",
                        restored.restored_items, escape_filename(restored.coordinator.as_os_str()))
                        .and_then(|_| writer.flush())
                        .wrap_err("Recovery completed, but its acknowledgement could not be written; inspect original paths before retrying")?;
                } else if restore_original {
                    let destination = gravityfile_ops::restore_original(&path)?;
                    write_recovery_acknowledgement(
                        &mut std::io::stdout().lock(),
                        &destination,
                        &path,
                        false,
                    )?;
                } else if let Some(destination) = restore_payload_to {
                    gravityfile_ops::restore_payload(&path, &destination)?;
                    write_recovery_acknowledgement(
                        &mut std::io::stdout().lock(),
                        &destination,
                        &path,
                        true,
                    )?;
                } else {
                    let report = gravityfile_ops::discover_recovery_report(&path)?;
                    if json {
                        let mut writer = BufWriter::new(std::io::stdout().lock());
                        report.write_json(&path, &mut writer)?;
                        writeln!(writer)?;
                        writer.flush()?;
                    } else if report.records.is_empty()
                        && report.rename_intents.is_empty()
                        && report.archive_records.is_empty()
                        && report.issues.is_empty()
                    {
                        println!(
                            "No recovery records in {}",
                            escape_filename(path.as_os_str())
                        );
                    }
                    for record in report.records.iter().filter(|_| !json) {
                        println!("{}", escape_filename(record.transaction.as_os_str()));
                        println!("  purpose: {:?}", record.purpose);
                        println!(
                            "  destination: {}",
                            escape_filename(record.destination.as_os_str())
                        );
                        println!(
                            "  original: {} | payload: {} | destination occupied: {} | publication recorded: {}",
                            record.original_available,
                            record.payload_available,
                            record.destination_occupied,
                            record.published
                        );
                        if let Some(source) = &record.move_source {
                            println!("  move source: {}", escape_filename(source.as_os_str()));
                        }
                        if record.published && record.original_available {
                            println!(
                                "  saved original may be incomplete: committed cleanup may have started"
                            );
                        }
                    }
                    for intent in report.rename_intents.iter().filter(|_| !json) {
                        println!("{}", escape_filename(intent.intent.as_os_str()));
                        println!(
                            "  bulk rename intent: outcome unverified; inspect manually, do not replay"
                        );
                        println!(
                            "  locked during inspection: {}",
                            intent.locked_during_inspection
                        );
                        if let Some(phase) = intent.last_recorded_phase {
                            println!(
                                "  last recorded phase: {phase:?} (historical; verify current paths)"
                            );
                        }
                        println!("  source: {}", escape_filename(intent.source.as_os_str()));
                        println!("  staged: {}", escape_filename(intent.staged.as_os_str()));
                        println!(
                            "  destination: {}",
                            escape_filename(intent.destination.as_os_str())
                        );
                    }
                    for record in report.archive_records.iter().filter(|_| !json) {
                        println!("{}", escape_filename(record.transaction.as_os_str()));
                        println!(
                            "  extraction journal: {} ({} planned items)",
                            record.status, record.entry_count
                        );
                        println!(
                            "  destination: {}",
                            escape_filename(record.destination.as_os_str())
                        );
                        println!(
                            "  staged data: {}",
                            escape_filename(record.staging.as_os_str())
                        );
                    }
                    for issue in &report.issues {
                        eprintln!(
                            "Cannot inspect {}: {}",
                            escape_filename(issue.path.as_os_str()),
                            issue.error
                        );
                    }
                    if !report.issues.is_empty() {
                        return Err(eyre!(
                            "Recovery listing incomplete: {} inspection errors",
                            report.issues.len()
                        ));
                    }
                }
            }
            Some(Command::Scan {
                scan_options,
                path,
                max_depth,
                top,
            }) => {
                run_scan(&path, max_depth, top, &output, &cancel, &scan_options)?;
            }
            Some(Command::Duplicates {
                scan_options,
                path,
                min_size,
                top,
                format,
            }) => {
                run_duplicates(
                    &path,
                    &min_size,
                    top,
                    format,
                    &output,
                    &cancel,
                    &scan_options,
                )?;
            }
            Some(Command::Age {
                scan_options,
                path,
                stale,
                format,
            }) => {
                run_age(&path, &stale, format, &output, &cancel, &scan_options)?;
            }
            Some(Command::Export {
                path,
                output,
                scan_options,
            }) => {
                run_export(
                    &path,
                    output,
                    &OutputPolicy {
                        strict: cli.strict,
                        envelope: cli.envelope,
                    },
                    &cancel,
                    &scan_options,
                )?;
            }
            None => {
                // Launch TUI
                let path = cli.path.canonicalize().context("Invalid path")?;
                let mut config = gravityfile_tui::TuiConfig::new()
                    .with_plugin_worker_executable(std::env::current_exe()?)
                    .with_cwd_file(cli.cwd_file)
                    .with_print_cwd(cli.print_cwd);
                if cli.scan {
                    config = config.with_scan_on_startup(true);
                }
                gravityfile_tui::run_with_config(path, config)?;
            }
        }

        Ok(())
    })();
    if cancel.is_cancelled() {
        return result.and_then(|()| Err(eyre!("operation interrupted")));
    }
    match result {
        Err(error) if observation && !output.strict && is_broken_pipe(&error) => Ok(()),
        result => result,
    }
}

/// Publication has already completed; an output failure must not imply rollback.
fn write_recovery_acknowledgement(
    writer: &mut impl Write,
    destination: &Path,
    transaction: &Path,
    payload: bool,
) -> Result<()> {
    let result = (|| -> std::io::Result<()> {
        writeln!(
            writer,
            "{} {}. Recovery record retained at {} for review.",
            if payload {
                "Restored payload to"
            } else {
                "Restored"
            },
            escape_filename(destination.as_os_str()),
            escape_filename(transaction.as_os_str()),
        )?;
        writer.flush()
    })();
    result.wrap_err("Recovery completed, but its acknowledgement could not be written; inspect the restored destination before retrying")
}

fn is_broken_pipe(error: &color_eyre::Report) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
            || cause
                .downcast_ref::<serde_json::Error>()
                .is_some_and(|json| json.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe))
    })
}

fn create_log_file() -> Result<(PathBuf, File)> {
    let directory = std::env::temp_dir();
    let process_id = std::process::id();

    for attempt in 0..100 {
        let path = directory.join(format!("gravityfile-{process_id}-{attempt}.log"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).wrap_err_with(|| {
                    format!(
                        "failed to create log file {}",
                        escape_filename(path.as_os_str())
                    )
                });
            }
        }
    }

    Err(eyre!(
        "failed to reserve a unique log file in {}",
        escape_filename(directory.as_os_str())
    ))
}

/// Run a quick scan and display summary.
fn run_scan(
    path: &Path,
    max_depth: Option<u32>,
    top_n: usize,
    policy: &OutputPolicy,
    cancel: &CancellationToken,
    scan_options: &ScanOptions,
) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;

    eprintln!("Scanning {}...", escape_filename(path.as_os_str()));

    let config = scan_options.config(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner
        .scan(&config, cancel.clone())
        .context("Scan failed")?;

    if policy.envelope {
        let status = scan_status(&tree);
        let envelope = ReportEnvelope::new("scan", &tree, &status, &tree);
        write_json_report_with(&mut std::io::stdout().lock(), cancel, |writer| {
            tree_json::write(writer, &tree, Some(&envelope))
        })?;
        return check_completeness(policy, &status);
    }

    // Print summary
    println!();
    println!("{}", "─".repeat(60));
    println!(
        " {} - {}",
        escape_filename(path.as_os_str()),
        format_size(tree.stats.total_size)
    );
    println!(
        " {} files, {} directories",
        tree.stats.total_files, tree.stats.total_dirs
    );
    println!(" Scanned in {:.2}s", tree.scan_duration.as_secs_f64());
    println!("{}", "─".repeat(60));
    println!();

    // Print tree
    print_node(
        &tree.root,
        &mut std::io::stdout().lock(),
        0,
        max_depth.unwrap_or(u32::MAX),
        top_n,
        tree.root.size,
        cancel,
    )?;

    if !tree.warnings.is_empty() {
        println!();
        println!("{} warning(s) during scan", tree.warning_count());
    }

    check_completeness(policy, &scan_status(&tree))
}

/// Run duplicate detection.
fn run_duplicates(
    path: &Path,
    min_size: &str,
    top_n: usize,
    format: OutputFormat,
    policy: &OutputPolicy,
    cancel: &CancellationToken,
    scan_options: &ScanOptions,
) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;
    let min_bytes = parse_size(min_size)?;

    eprintln!("Scanning {}...", escape_filename(path.as_os_str()));

    let config = scan_options.config(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner
        .scan(&config, cancel.clone())
        .context("Scan failed")?;

    eprintln!("Finding duplicates (min size: {})...", min_size);

    let dup_config = DuplicateConfig::builder()
        .min_size(min_bytes)
        .max_groups(top_n)
        .build()
        .context("Invalid duplicate-analysis configuration")?;

    let parameters = serde_json::to_value(&dup_config)?;
    let finder = DuplicateFinder::with_config(dup_config).map_err(|error| eyre!(error))?;
    let report = finder.find_duplicates(&tree, cancel.clone());

    if policy.envelope {
        let mut envelope = ReportEnvelope::new("duplicates", &tree, &report.status, &report);
        envelope.parameters = parameters;
        write_json_stdout(&envelope, cancel)?;
        return check_completeness(policy, &report.status);
    }
    match format {
        OutputFormat::Text => {
            write_duplicate_text(&report, &path, &mut std::io::stdout().lock(), cancel)?;
        }
        OutputFormat::Json => {
            write_json_stdout(&report, cancel)?;
        }
    }

    check_completeness(policy, &report.status)
}

/// Run age analysis.
fn run_age(
    path: &Path,
    stale_threshold: &str,
    format: OutputFormat,
    policy: &OutputPolicy,
    cancel: &CancellationToken,
    scan_options: &ScanOptions,
) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;
    let stale_duration = parse_duration(stale_threshold)?;

    eprintln!("Scanning {}...", escape_filename(path.as_os_str()));

    let config = scan_options.config(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner
        .scan(&config, cancel.clone())
        .context("Scan failed")?;

    eprintln!("Analyzing file ages...");

    let age_config = AgeConfig::builder()
        .stale_threshold(stale_duration)
        .build()
        .context("Invalid age-analysis configuration")?;

    let parameters = serde_json::to_value(&age_config)?;
    let analyzer = AgeAnalyzer::with_config(age_config).map_err(|error| eyre!(error))?;
    let report = analyzer.analyze(&tree, cancel.clone());

    if policy.envelope {
        let mut envelope = ReportEnvelope::new("age", &tree, &report.status, &report);
        envelope.parameters = parameters;
        write_json_stdout(&envelope, cancel)?;
        return check_completeness(policy, &report.status);
    }
    match format {
        OutputFormat::Text => {
            write_age_text(
                &report,
                &path,
                stale_threshold,
                &mut std::io::stdout().lock(),
                cancel,
            )?;
        }
        OutputFormat::Json => {
            write_json_stdout(&report, cancel)?;
        }
    }

    check_completeness(policy, &report.status)
}

fn write_duplicate_text(
    report: &gravityfile_analyze::DuplicateReport,
    path: &Path,
    destination: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut output = ReportWriter {
        output: destination,
        cancel,
    };

    reportln!(output);
    reportln!(output, "{}", "─".repeat(60));
    reportln!(
        output,
        " DUPLICATE REPORT - {}",
        escape_filename(path.as_os_str())
    );
    reportln!(
        output,
        " {} groups | {} files | {} wasted",
        report.group_count,
        report.files_with_duplicates,
        format_size(report.total_wasted_space)
    );
    reportln!(output, "{}", "─".repeat(60));
    reportln!(output);

    for (i, group) in report.groups.iter().enumerate() {
        reportln!(
            output,
            " [{}] {} ({} × {})",
            i + 1,
            format_size(group.wasted_bytes),
            group.paths.len(),
            format_size(group.size)
        );
        for path in &group.paths {
            reportln!(output, "   {}", escape_filename(path.as_os_str()));
        }
        reportln!(output);
    }

    output.flush()?;
    Ok(())
}

fn write_age_text(
    report: &gravityfile_analyze::AgeReport,
    path: &Path,
    stale_threshold: &str,
    destination: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut output = ReportWriter {
        output: destination,
        cancel,
    };

    reportln!(output);
    reportln!(output, "{}", "─".repeat(60));
    reportln!(
        output,
        " AGE ANALYSIS - {}",
        escape_filename(path.as_os_str())
    );
    reportln!(
        output,
        " {} files | {} | Avg age: {}",
        report.total_files,
        format_size(report.total_size),
        gravityfile_analyze::format_age(report.average_age)
    );
    reportln!(output, "{}", "─".repeat(60));
    reportln!(output);

    reportln!(output, " DISTRIBUTION:");
    for bucket in &report.buckets {
        let bar_width = 20;
        let filled = if report.total_files > 0 {
            (bucket.file_count as f64 / report.total_files as f64 * bar_width as f64).round()
                as usize
        } else {
            0
        };
        let bar = "█".repeat(filled);
        reportln!(
            output,
            " {:<12} {:>8} files  {}",
            bucket.name,
            bucket.file_count,
            bar
        );
    }

    if report.stale_directory_status != gravityfile_analyze::StaleDirectoryStatus::Evaluated {
        reportln!(
            output,
            " Stale-directory recommendations {}",
            report.stale_directory_status
        );
    }
    if !report.stale_directories.is_empty() {
        reportln!(output);
        reportln!(output, " STALE DIRECTORIES (> {}):", stale_threshold);
        for (i, dir) in report.stale_directories.iter().enumerate().take(10) {
            reportln!(
                output,
                " [{}] {} - {} (newest: {})",
                i + 1,
                escape_filename(dir.path.as_os_str()),
                format_size(dir.size),
                gravityfile_analyze::format_age(dir.newest_file_age)
            );
        }
    }

    output.flush()?;
    Ok(())
}

/// Run export to JSON.
fn run_export(
    path: &Path,
    output: Option<PathBuf>,
    policy: &OutputPolicy,
    cancel: &CancellationToken,
    scan_options: &ScanOptions,
) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;

    eprintln!("Scanning {}...", escape_filename(path.as_os_str()));

    let config = scan_options.config(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner
        .scan(&config, cancel.clone())
        .context("Scan failed")?;

    let status = scan_status(&tree);
    let envelope = ReportEnvelope::new("scan", &tree, &status, &tree);
    let encode = |writer: &mut dyn Write| {
        tree_json::write(writer, &tree, policy.envelope.then_some(&envelope))
    };
    match output {
        Some(output_path) => {
            write_json_atomic_with(&output_path, cancel, encode)?;
            eprintln!("Exported to {}", escape_filename(output_path.as_os_str()));
        }
        None => write_json_report_with(&mut std::io::stdout().lock(), cancel, encode)?,
    }

    check_completeness(policy, &status)
}

/// Publish a complete JSON document from the same filesystem. Serialization or
/// write failures leave an existing report intact; a final symlink is replaced,
/// never followed. Keep the temporary file private because scans expose paths.
#[cfg(test)]
fn write_json_atomic(
    path: &Path,
    value: &impl serde::Serialize,
    cancel: &CancellationToken,
) -> Result<()> {
    write_json_atomic_with(path, cancel, |writer| {
        serde_json::to_writer_pretty(writer, value)?;
        Ok(())
    })
}

fn write_json_atomic_with(
    path: &Path,
    cancel: &CancellationToken,
    encode: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    write_json_atomic_with_sync(
        path,
        cancel,
        encode,
        atomic_export::StagedExport::sync_parent,
    )
}

fn write_json_atomic_with_sync(
    path: &Path,
    cancel: &CancellationToken,
    encode: impl FnOnce(&mut dyn Write) -> Result<()>,
    sync_parent: impl FnOnce(&atomic_export::StagedExport) -> std::io::Result<()>,
) -> Result<()> {
    check_report_cancel(cancel)?;
    let mut staged = atomic_export::StagedExport::new(path)?;
    {
        let mut writer = BufWriter::new(ReportWriter {
            output: staged.file_mut(),
            cancel,
        });
        encode(&mut writer)?;
        writeln!(writer)?;
        writer.flush()?;
    }
    check_report_cancel(cancel)?;
    staged.file_mut().sync_all()?;
    check_report_cancel(cancel)?;
    staged.persist().wrap_err_with(|| {
        format!(
            "failed to publish export {}",
            escape_filename(path.as_os_str())
        )
    })?;
    // Publication has committed: do not skip this step on cancellation and do
    // not roll back or remove the visible report if synchronization fails.
    sync_parent(&staged).wrap_err_with(|| {
        format!(
            "export {} was published, but directory synchronization failed; durability is uncertain; inspect the destination before retrying",
            escape_filename(path.as_os_str())
        )
    })?;
    Ok(())
}

fn print_node(
    node: &gravityfile_core::FileNode,
    output: &mut impl Write,
    depth: u32,
    max_depth: u32,
    top_n: usize,
    total_size: u64,
    cancel: &CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(eyre!("operation interrupted"));
    }
    if depth > max_depth {
        return Ok(());
    }
    // One iterator per ancestor, rather than recursive calls or a pending
    // allocation for every sibling. Keep the omission line after its children.
    let mut ancestors = Vec::new();
    let mut current = Some((node, depth));
    loop {
        if cancel.is_cancelled() {
            return Err(eyre!("operation interrupted"));
        }
        if let Some((node, depth)) = current.take() {
            let percentage = if total_size > 0 {
                (node.size as f64 / total_size as f64) * 100.0
            } else {
                0.0
            };
            writeln!(
                output,
                "{}",
                terminal::escape(&format!(
                    "{}{:<30} {:>10} [{:>5.1}%]",
                    "  ".repeat(depth as usize),
                    node.display_name(),
                    format_size(node.size),
                    percentage
                ))
            )?;
            if depth < max_depth && node.is_dir() {
                ancestors.push((
                    node.children.iter().take(top_n),
                    depth,
                    node.children.len().saturating_sub(top_n),
                ));
            }
        }
        loop {
            if cancel.is_cancelled() {
                return Err(eyre!("operation interrupted"));
            }
            let Some((children, parent_depth, omitted)) = ancestors.last_mut() else {
                return Ok(());
            };
            if let Some(child) = children.next() {
                // The frame is created only when parent_depth < max_depth.
                current = Some((child, *parent_depth + 1));
                break;
            }
            if *omitted > 0 {
                writeln!(
                    output,
                    "{}  ... ({} more items)",
                    "  ".repeat(*parent_depth as usize),
                    omitted
                )?;
            }
            ancestors.pop();
        }
    }
}

fn format_size(bytes: u64) -> String {
    format_size_human(bytes, BINARY)
}

/// Parse a non-negative byte count, validating the scaled value before the
/// `as u64` cast (which would otherwise silently saturate negatives/NaN to 0).
fn scale_bytes(num: f64, multiplier: f64) -> Result<u64> {
    if !num.is_finite() || num < 0.0 {
        return Err(eyre!("size must be a non-negative number"));
    }
    let scaled = num * multiplier;
    if !scaled.is_finite() || scaled >= u64::MAX as f64 {
        return Err(eyre!("size is too large"));
    }
    Ok(scaled as u64)
}

fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim().to_uppercase();
    let (number, multiplier) = [
        ("GIB", 1u64 << 30),
        ("GB", 1 << 30),
        ("G", 1 << 30),
        ("MIB", 1 << 20),
        ("MB", 1 << 20),
        ("M", 1 << 20),
        ("KIB", 1 << 10),
        ("KB", 1 << 10),
        ("K", 1 << 10),
        ("B", 1),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| {
        s.strip_suffix(suffix)
            .map(|number| (number.trim(), multiplier))
    })
    .unwrap_or((&s, 1));
    // Preserve all 64 bits for integer arguments instead of rounding through f64.
    if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
        return number
            .parse::<u64>()
            .ok()
            .and_then(|value| value.checked_mul(multiplier))
            .ok_or_else(|| eyre!("size is too large"));
    }
    scale_bytes(parse_size_number(number)?, multiplier as f64)
}

fn parse_size_number(value: &str) -> Result<f64> {
    value
        .parse()
        .map_err(|_| eyre!("invalid size: expected a number followed by B, KB, MB, or GB"))
}

fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let s = s.trim().to_lowercase();
    let (num, multiplier) = if s.ends_with('y') {
        let num: f64 = s.strip_suffix('y').unwrap().trim().parse()?;
        (num, 365.25 * 24.0 * 60.0 * 60.0)
    } else if s.ends_with("mo") || s.ends_with('m') {
        let num: f64 = s
            .strip_suffix("mo")
            .or_else(|| s.strip_suffix('m'))
            .unwrap()
            .trim()
            .parse()?;
        (num, 30.44 * 24.0 * 60.0 * 60.0)
    } else if s.ends_with('w') {
        let num: f64 = s.strip_suffix('w').unwrap().trim().parse()?;
        (num, 7.0 * 24.0 * 60.0 * 60.0)
    } else if s.ends_with('d') {
        let num: f64 = s.strip_suffix('d').unwrap().trim().parse()?;
        (num, 24.0 * 60.0 * 60.0)
    } else if s.ends_with('h') {
        let num: f64 = s.strip_suffix('h').unwrap().trim().parse()?;
        (num, 60.0 * 60.0)
    } else {
        let num: f64 = s.parse()?;
        (num, 24.0 * 60.0 * 60.0) // Default to days
    };

    // `Duration::from_secs_f64` panics on negative/NaN/infinite input.
    if !num.is_finite() || num < 0.0 {
        return Err(eyre!("duration must be a non-negative number"));
    }
    let seconds = num * multiplier;
    if !seconds.is_finite() || seconds > std::time::Duration::MAX.as_secs_f64() {
        return Err(eyre!("duration is too large"));
    }

    std::time::Duration::try_from_secs_f64(seconds).map_err(|_| eyre!("duration is too large"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn recovery_acknowledgement_reports_flush_failure_after_successful_write() {
        struct FailFlush(Vec<u8>);
        impl std::io::Write for FailFlush {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("injected flush failure"))
            }
        }
        let mut writer = FailFlush(Vec::new());
        let error = super::write_recovery_acknowledgement(
            &mut writer,
            std::path::Path::new("restored"),
            std::path::Path::new("journal"),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Recovery completed"));
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains("injected flush failure"))
        );
        assert_eq!(
            writer.0,
            b"Restored restored. Recovery record retained at journal for review.\n"
        );
    }

    #[test]
    fn analysis_text_reports_preserve_escaping_and_stop_on_output_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one"), b"same content").unwrap();
        std::fs::write(directory.path().join("two"), b"same content").unwrap();
        let tree = JwalkScanner::new()
            .scan(&ScanConfig::new(directory.path()), CancellationToken::new())
            .unwrap();
        let duplicate_config = DuplicateConfig::builder().min_size(0u64).build().unwrap();
        let duplicates = DuplicateFinder::with_config(duplicate_config)
            .unwrap()
            .find_duplicates(&tree, CancellationToken::new());
        let ages = AgeAnalyzer::new().analyze(&tree, CancellationToken::new());
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
            let token = CancellationToken::new();
            let mut writer = CancelAfterLine {
                token: token.clone(),
                bytes: Vec::new(),
            };
            let result = if age {
                write_age_text(&ages, directory.path(), "1y", &mut writer, &token)
            } else {
                write_duplicate_text(&duplicates, directory.path(), &mut writer, &token)
            };
            assert!(result.is_err());
            assert_eq!(writer.bytes, b"\n");
            let mut output = Vec::new();
            let path = Path::new("report\n\u{1b}[31m");
            if age {
                write_age_text(&ages, path, "1y", &mut output, &CancellationToken::new()).unwrap();
            } else {
                write_duplicate_text(&duplicates, path, &mut output, &CancellationToken::new())
                    .unwrap();
            }
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains(r"report\n\u{1b}[31m"));
            assert!(!output.contains('\u{1b}'));
            if age {
                assert!(output.contains("2 files"));
            } else {
                assert!(output.contains("1 groups | 2 files"));
            }
        }
    }

    #[test]
    fn serialization_cancellation_preserves_stdout_and_existing_export() {
        struct CancelAfterLargeValue<'a>(&'a CancellationToken);
        impl serde::Serialize for CancelAfterLargeValue<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(&"x".repeat(2 * 1024 * 1024))?;
                self.0.cancel();
                seq.serialize_element(&"cancelled")?;
                seq.end()
            }
        }
        let token = CancellationToken::new();
        let mut stdout = Vec::new();
        assert!(write_json_report(&CancelAfterLargeValue(&token), &mut stdout, &token).is_err());
        assert!(stdout.is_empty());
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("report.json");
        std::fs::write(&destination, b"old report").unwrap();
        let token = CancellationToken::new();
        assert!(write_json_atomic(&destination, &CancelAfterLargeValue(&token), &token).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"old report");
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
    }

    #[test]
    fn cancellation_during_stdout_copy_returns_error_after_a_partial_prefix() {
        struct CancelAfterWrite {
            token: CancellationToken,
            bytes: Vec<u8>,
        }
        impl Write for CancelAfterWrite {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                self.token.cancel();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let token = CancellationToken::new();
        let mut destination = CancelAfterWrite {
            token: token.clone(),
            bytes: Vec::new(),
        };
        let value = "x".repeat(2 * 1024 * 1024);
        assert!(write_json_report(&value, &mut destination, &token).is_err());
        assert!(!destination.bytes.is_empty());
        assert!(destination.bytes.len() < value.len());
    }

    #[test]
    fn cancelled_report_writer_returns_a_non_retryable_io_error() {
        let token = CancellationToken::new();
        token.cancel();
        let mut bytes = Vec::new();
        let mut writer = ReportWriter {
            output: &mut bytes,
            cancel: &token,
        };
        assert_eq!(
            writer.write_all(b"do not publish").unwrap_err().kind(),
            std::io::ErrorKind::Other
        );
        assert!(writer.flush().is_err());
        assert!(bytes.is_empty());
    }

    #[test]
    fn scan_summary_stops_after_cancellation_during_output() {
        use gravityfile_core::{FileNode, NodeId, Timestamps};
        use std::io::Write;
        let cancel = tokio_util::sync::CancellationToken::new();
        struct CancelAfterLine {
            token: tokio_util::sync::CancellationToken,
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
        let mut root = FileNode::new_directory(
            NodeId::new(0),
            "root",
            Timestamps::with_modified(std::time::UNIX_EPOCH),
        );
        root.children.push(FileNode::new_directory(
            NodeId::new(1),
            "child",
            root.timestamps,
        ));
        for top in [0, 1] {
            let token = cancel.child_token();
            let mut writer = CancelAfterLine {
                token: token.clone(),
                bytes: Vec::new(),
            };
            let error =
                super::print_node(&root, &mut writer, 0, u32::MAX, top, 0, &token).unwrap_err();
            assert_eq!(error.to_string(), "operation interrupted");
            assert_eq!(
                writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
                1
            );
            assert!(
                !String::from_utf8(writer.bytes)
                    .unwrap()
                    .contains("more items")
            );
        }
        cancel.cancel();
        let mut output = Vec::new();
        assert!(super::print_node(&root, &mut output, 0, u32::MAX, 1, 0, &cancel).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn scan_summary_preserves_order_depth_and_omission_lines() {
        use gravityfile_core::{FileNode, NodeId, Timestamps};
        let make = |id, name: &str| {
            FileNode::new_directory(
                NodeId::new(id),
                name,
                Timestamps::with_modified(std::time::UNIX_EPOCH),
            )
        };
        let mut root = make(0, "root");
        let mut child = make(1, "child");
        child.children = vec![make(2, "grandchild"), make(3, "hidden")];
        root.children = vec![child, make(4, "hidden-root")];
        let mut output = Vec::new();
        super::print_node(
            &root,
            &mut output,
            0,
            2,
            1,
            0,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("root"));
        assert!(lines[1].starts_with("  child"));
        assert!(lines[2].starts_with("    grandchild"));
        assert_eq!(lines[3], "    ... (1 more items)");
        assert_eq!(lines[4], "  ... (1 more items)");
        output = Vec::new();
        super::print_node(
            &root,
            &mut output,
            0,
            0,
            1,
            0,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        output.clear();
        super::print_node(
            &root,
            &mut output,
            0,
            2,
            0,
            0,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .ends_with("  ... (2 more items)\n")
        );
    }

    #[test]
    fn scan_summary_handles_deep_trees_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                use gravityfile_core::{FileNode, NodeId, Timestamps};
                let mut node = FileNode::new_directory(
                    NodeId::new(0),
                    "leaf",
                    Timestamps::with_modified(std::time::UNIX_EPOCH),
                );
                for id in 1..=5000 {
                    let mut parent = FileNode::new_directory(
                        NodeId::new(id),
                        "parent",
                        Timestamps::with_modified(std::time::UNIX_EPOCH),
                    );
                    parent.children.push(node);
                    node = parent;
                }
                super::print_node(
                    &node,
                    &mut std::io::sink(),
                    0,
                    u32::MAX,
                    usize::MAX,
                    0,
                    &tokio_util::sync::CancellationToken::new(),
                )
                .unwrap();
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn scan_summary_returns_output_failure_without_visiting_descendants() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let node = gravityfile_core::FileNode::new_directory(
            gravityfile_core::NodeId::new(0),
            "root",
            gravityfile_core::Timestamps::with_modified(std::time::UNIX_EPOCH),
        );
        let error = super::print_node(
            &node,
            &mut Broken,
            0,
            u32::MAX,
            1,
            0,
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap_err();
        assert!(super::is_broken_pipe(&error));
    }
    #[test]
    fn omitted_scan_warning_roundtrips_with_its_observed_count() {
        let warning = gravityfile_core::ScanWarning::new(
            std::path::PathBuf::new(),
            gravityfile_core::WarningKind::Omitted { count: 101 },
            "101 warning details omitted",
        );
        let json = serde_json::to_string(&warning).unwrap();
        let restored: gravityfile_core::ScanWarning = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, warning);
        assert_eq!(restored.represented_count(), 101);
    }

    use super::*;

    #[test]
    fn cancelled_commands_stop_and_preserve_existing_export() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("report.json");
        std::fs::write(&destination, "existing report").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let policy = OutputPolicy {
            strict: false,
            envelope: true,
        };
        assert!(
            run_scan(
                root.path(),
                None,
                10,
                &policy,
                &cancel,
                &ScanOptions { max_entries: None }
            )
            .is_err()
        );
        assert!(
            run_duplicates(
                root.path(),
                "1",
                10,
                OutputFormat::Json,
                &policy,
                &cancel,
                &ScanOptions { max_entries: None }
            )
            .is_err()
        );
        assert!(
            run_age(
                root.path(),
                "1d",
                OutputFormat::Json,
                &policy,
                &cancel,
                &ScanOptions { max_entries: None }
            )
            .is_err()
        );
        assert!(
            run_export(
                root.path(),
                Some(destination.clone()),
                &policy,
                &cancel,
                &ScanOptions { max_entries: None }
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "existing report"
        );
    }

    #[test]
    fn parse_size_accepts_common_units_and_whitespace() {
        assert_eq!(parse_size(" 1.5 MB ").unwrap(), 1_572_864);
        assert_eq!(parse_size("2k").unwrap(), 2048);
        assert_eq!(parse_size("7").unwrap(), 7);
    }

    #[test]
    fn parse_size_rejects_non_finite_negative_and_overflowing_values() {
        assert!(parse_size("-1MB").is_err());
        assert!(parse_size("NaNGB").is_err());
        assert!(parse_size("1e300GB").is_err());
        assert!(parse_size("18446744073709551616").is_err());
        assert!(parse_size("18446744073709551616.0").is_err());
        assert!(parse_size("1GBGB").is_err());
        assert!(parse_size("1BB").is_err());
        assert_eq!(parse_size("18446744073709551615").unwrap(), u64::MAX);
        assert_eq!(
            parse_size("9007199254740993B").unwrap(),
            9_007_199_254_740_993
        );
        assert_eq!(parse_size("1MiB").unwrap(), 1_048_576);
    }

    #[test]
    fn fatal_error_chain_is_escaped_as_a_single_line() {
        let error = eyre!("source\n\u{1b}]52;c;payload\u{7}").wrap_err("context\u{202e}");
        let mut output = Vec::new();
        write_cli_failure(&error, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(r"source\n\u{1b}]52;c;payload\u{7}"));
        assert!(text.contains(r"context\u{202e}"));
        assert_eq!(text.matches('\n').count(), 1);
        assert!(!text.contains(['\u{1b}', '\u{7}', '\u{202e}']));
    }

    #[test]
    fn parse_duration_rejects_values_that_would_panic_duration_constructor() {
        assert!(parse_duration("-1d").is_err());
        assert!(parse_duration("NaNy").is_err());
        assert!(parse_duration("1e300y").is_err());
        assert!(parse_duration("5124095576030431h").is_err());
        assert!(parse_duration("1dd").is_err());
        assert_eq!(
            parse_duration("6m").unwrap(),
            parse_duration("6mo").unwrap()
        );
    }

    #[test]
    fn json_serialization_failure_publishes_nothing_even_after_spilling() {
        struct Invalid;
        impl serde::Serialize for Invalid {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut sequence = serializer.serialize_seq(Some(2))?;
                sequence.serialize_element(&"x".repeat(2 * 1024 * 1024))?;
                Err(serde::ser::Error::custom("late serialization failure"))
            }
        }
        let mut destination = Vec::new();
        let error =
            write_json_report(&Invalid, &mut destination, &CancellationToken::new()).unwrap_err();
        assert!(error.to_string().contains("late serialization failure"));
        assert!(destination.is_empty());
    }

    #[test]
    fn staged_json_preserves_large_output_and_propagates_sink_failure() {
        let value = vec!["x".repeat(2 * 1024 * 1024)];
        let mut destination = Vec::new();
        write_json_report(&value, &mut destination, &CancellationToken::new()).unwrap();
        assert_eq!(destination.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<Vec<String>>(&destination).unwrap(),
            value
        );
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "closed sink",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let error = write_json_report(&value, &mut Broken, &CancellationToken::new()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn failed_serialization_preserves_existing_export_and_cleans_staging() {
        struct Invalid;
        impl serde::Serialize for Invalid {
            fn serialize<S: serde::Serializer>(
                &self,
                _serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("injected serialization failure"))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.json");
        std::fs::write(&output, b"original").unwrap();
        assert!(write_json_atomic(&output, &Invalid, &CancellationToken::new()).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn export_replaces_symlink_without_modifying_its_target() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("valuable");
        let output = directory.path().join("report.json");
        std::fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink(&target, &output).unwrap();
        write_json_atomic(&output, &vec![1, 2, 3], &CancellationToken::new()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert!(!output.symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            serde_json::from_slice::<Vec<u32>>(&std::fs::read(output).unwrap()).unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn export_sync_failure_reports_committed_destination() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.json");
        std::fs::write(&output, b"old report").unwrap();
        let token = CancellationToken::new();
        let error = write_json_atomic_with_sync(
            &output,
            &token,
            |writer| Ok(writer.write_all(b"[1,2,3]")?),
            |_| {
                assert_eq!(std::fs::read(&output).unwrap(), b"[1,2,3]\n");
                // Cancellation after publication must not suppress sync errors.
                token.cancel();
                Err(std::io::Error::other("injected directory sync failure"))
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("was published"));
        assert!(error.to_string().contains("durability is uncertain"));
        assert!(
            error
                .to_string()
                .contains("inspect the destination before retrying")
        );
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Other
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"[1,2,3]\n");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn export_publication_and_cleanup_remain_in_opened_parent() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for fail_encoding in [false, true] {
            for substitute_symlink in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let parent = directory.path().join("parent");
                let moved = directory.path().join("moved");
                let decoy = directory.path().join("decoy");
                std::fs::create_dir(&parent).unwrap();
                std::fs::create_dir(&decoy).unwrap();
                let output = parent.join("report.json");
                std::fs::write(&output, b"original").unwrap();
                let mut staging_name = None;
                let result = write_json_atomic_with(&output, &CancellationToken::new(), |writer| {
                    let name = std::fs::read_dir(&parent)
                        .unwrap()
                        .map(|entry| entry.unwrap().file_name())
                        .find(|name| name.to_string_lossy().starts_with(".gravityfile-export-"))
                        .unwrap();
                    assert_eq!(
                        std::fs::metadata(parent.join(&name))
                            .unwrap()
                            .permissions()
                            .mode()
                            & 0o077,
                        0
                    );
                    std::fs::rename(&parent, &moved).unwrap();
                    if substitute_symlink {
                        symlink(&decoy, &parent).unwrap();
                    } else {
                        std::fs::create_dir(&parent).unwrap();
                    }
                    std::fs::write(parent.join(&name), b"unrelated staging name").unwrap();
                    std::fs::write(&output, b"unrelated report").unwrap();
                    staging_name = Some(name);
                    writer.write_all(b"[1,2,3]")?;
                    if fail_encoding {
                        return Err(eyre!("injected encoding failure"));
                    }
                    Ok(())
                });
                assert_eq!(result.is_err(), fail_encoding);
                let expected: &[u8] = if fail_encoding {
                    b"original"
                } else {
                    b"[1,2,3]\n"
                };
                assert_eq!(std::fs::read(moved.join("report.json")).unwrap(), expected);
                assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 1);
                assert_eq!(std::fs::read(&output).unwrap(), b"unrelated report");
                assert_eq!(
                    std::fs::read(parent.join(staging_name.unwrap())).unwrap(),
                    b"unrelated staging name"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn export_rejects_directory_syntax_without_normalizing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report");
        std::fs::write(&output, b"original").unwrap();
        for suffix in ["report/", "report/.", "report/.."] {
            assert!(
                write_json_atomic(
                    &directory.path().join(suffix),
                    &vec![1],
                    &CancellationToken::new()
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&output).unwrap(), b"original");
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn export_does_not_sync_parent_after_failed_publication() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.json");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("keep"), b"original").unwrap();
        let error = write_json_atomic_with_sync(
            &output,
            &CancellationToken::new(),
            |writer| Ok(writer.write_all(b"[]")?),
            |_| panic!("directory sync must follow successful publication"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("failed to publish export"));
        assert_eq!(std::fs::read(output.join("keep")).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
