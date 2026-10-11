//! gravityfile: disk usage analyzer and file explorer.
//!
//! ```text
//! grav [PATH]              Open the explorer
//! grav scan [PATH]         Largest entries, as a bounded tree
//! grav duplicates [PATH]   Files with identical contents
//! grav age [PATH]          File ages and stale directories
//! grav export [PATH]       Complete scan as a JSON snapshot
//! grav init <SHELL>        Shell integration (cd on exit)
//! grav completions <SHELL> Shell completions
//! ```
//!
//! Exit status: 0 success, 1 error, 2 usage error, 3 incomplete results with
//! `--strict`, 128 + signal number when interrupted.

mod atomic_export;
mod bounded_log;
mod cli;
mod commands;
#[cfg(test)]
mod duplicate_bench;
mod progress;
mod report;
mod shutdown;
mod style;
mod terminal;
mod tree_json;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use color_eyre::Result;
use color_eyre::eyre::{Context as _, eyre};
use gravityfile_core::escape_filename;

use cli::{Cli, Command, GlobalArgs};
use commands::Context;
use report::Completeness;
use style::{Styles, paint};

/// Exit status for a failed command.
const EXIT_ERROR: u8 = 1;
/// Exit status for `--strict` runs whose results are incomplete.
const EXIT_INCOMPLETE: u8 = 3;

/// First argument with which the plugin host re-executes this binary as an
/// isolated plugin worker. Not part of the command-line interface.
const PLUGIN_WORKER: &str = "__plugin-worker";

/// Process entry point: parse arguments, run, and map the outcome to an exit
/// status.
pub fn main_entry() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().is_some_and(|first| first == PLUGIN_WORKER) {
        return plugin_worker(arguments.next(), arguments.next().is_some());
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return parse_failure(&error),
    };
    let (out, err) = Styles::for_streams(cli.global.color);
    match run(cli, out, err) {
        Ok(status) => status,
        Err(error) => {
            report_error(&error, &err);
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn run(cli: Cli, out: Styles, err: Styles) -> Result<ExitCode> {
    color_eyre::install()?;
    let Cli {
        path,
        scan,
        apparent_size,
        cwd_file,
        print_cwd,
        global,
        command,
    } = cli;
    let Some(command) = command else {
        return explore(
            path,
            scan,
            apparent_size,
            cwd_file,
            print_cwd,
            &global,
            &err,
        );
    };
    let mut stdout = std::io::stdout().lock();
    match command {
        Command::Init { shell } => commands::shell::init(shell, &mut stdout)?,
        Command::Completions { shell } => commands::shell::completions(shell, &mut stdout)?,
        Command::Man => commands::shell::man(&mut stdout)?,
        Command::Scan(_) | Command::Duplicates(_) | Command::Age(_) | Command::Export(_) => {
            drop(stdout);
            return report(&command, &global, out, err);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Serve one plugin runtime over stdin/stdout for the plugin host.
fn plugin_worker(language: Option<std::ffi::OsString>, extra: bool) -> ExitCode {
    let result = match language.as_deref().map(std::ffi::OsStr::to_str) {
        Some(Some(language)) if !extra => {
            gravityfile_plugin::run_worker(language).map_err(|e| eyre!(e))
        }
        _ => Err(eyre!("usage: grav {PLUGIN_WORKER} <LANGUAGE>")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(&error, &Styles::plain());
            ExitCode::from(EXIT_ERROR)
        }
    }
}

/// Run a report command under signal handling.
fn report(command: &Command, global: &GlobalArgs, out: Styles, err: Styles) -> Result<ExitCode> {
    init_logging(global.log_file.as_deref(), false)?;
    tracing::info!("gravityfile v{}", env!("CARGO_PKG_VERSION"));
    let shutdown = shutdown::Shutdown::install()?;
    let context = Context {
        cancel: shutdown.token(),
        units: global.units(),
        out,
        err,
    };
    let (result, strict) = match command {
        Command::Scan(command) => (
            commands::scan::run(command, &context),
            command.output.strict,
        ),
        Command::Duplicates(command) => (
            commands::duplicates::run(command, &context),
            command.output.strict,
        ),
        Command::Age(command) => (commands::age::run(command, &context), command.output.strict),
        Command::Export(command) => (commands::export::run(command, &context), command.strict),
        _ => unreachable!("only report commands reach here"),
    };
    if context.cancel.is_cancelled() {
        report_error(&eyre!("interrupted"), &err);
        return Ok(ExitCode::from(shutdown.exit_status()));
    }
    match result {
        Ok(Completeness::Incomplete) if strict => Ok(ExitCode::from(EXIT_INCOMPLETE)),
        Ok(_) => Ok(ExitCode::SUCCESS),
        // A reader that stops early (`grav scan | head`) is not a failure,
        // unless the caller asked to know that every result was delivered.
        Err(error) if !strict && is_broken_pipe(&error) => Ok(ExitCode::SUCCESS),
        Err(error) => Err(error),
    }
}

fn explore(
    path: Option<PathBuf>,
    scan: bool,
    apparent_size: bool,
    cwd_file: Option<PathBuf>,
    print_cwd: bool,
    global: &GlobalArgs,
    err: &Styles,
) -> Result<ExitCode> {
    init_logging(global.log_file.as_deref(), true)?;
    match gravityfile_core::paths::migrate_legacy_config() {
        Ok(Some((from, to))) => note(
            err,
            &format!(
                "moved settings and plugins from {} to {}",
                escape_filename(from.as_os_str()),
                escape_filename(to.as_os_str())
            ),
        ),
        Ok(None) => {}
        // The old directory keeps working until it can be moved.
        Err(error) => warn(
            err,
            &format!("cannot move settings to their new location: {error}"),
        ),
    }
    let requested = path.unwrap_or_else(|| PathBuf::from("."));
    let path = requested
        .canonicalize()
        .wrap_err_with(|| format!("cannot open {}", escape_filename(requested.as_os_str())))?;
    let mut config = gravityfile_tui::TuiConfig::new()
        .with_plugin_worker_executable(std::env::current_exe()?)
        .with_cwd_file(cwd_file)
        .with_print_cwd(print_cwd)
        .with_units(global.units())
        .with_apparent_size(apparent_size);
    // Without --scan, the saved setting decides.
    if scan {
        config = config.with_scan_on_startup(true);
    }
    gravityfile_tui::run_with_config(path, config)?;
    Ok(ExitCode::SUCCESS)
}

/// Print a clap error, escaping it when arguments contain control characters
/// that clap would echo verbatim.
fn parse_failure(error: &clap::Error) -> ExitCode {
    let has_control_argument = std::env::args_os().any(|argument| {
        let text = argument.to_string_lossy();
        terminal::escape(&text) != text
    });
    let result = if error.use_stderr() && has_control_argument {
        writeln!(
            std::io::stderr().lock(),
            "{}",
            terminal::escape(&error.to_string())
        )
    } else {
        error.print()
    };
    if result.is_ok() {
        ExitCode::from(error.exit_code() as u8)
    } else {
        ExitCode::FAILURE
    }
}

/// `error: ...` on stderr, with the cause chain on one escaped line.
fn report_error(error: &color_eyre::Report, styles: &Styles) {
    let _ = write_error(error, styles, &mut std::io::stderr().lock());
}

fn write_error(
    error: &color_eyre::Report,
    styles: &Styles,
    output: &mut impl Write,
) -> std::io::Result<()> {
    // Alternate Display includes the cause chain; escape only after formatting
    // so third-party source messages cannot bypass the terminal boundary.
    writeln!(
        output,
        "{} {}",
        paint(styles.error, "error:"),
        terminal::escape(&format!("{error:#}"))
    )
}

/// `warning: ...` on stderr. Diagnostics are best-effort: a failed stderr must
/// not replace the command's outcome.
pub(crate) fn warn(styles: &Styles, message: &str) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "{} {}",
        paint(styles.warning, "warning:"),
        terminal::escape(message)
    );
}

/// An indented note on stderr, following a warning or reporting progress.
pub(crate) fn note(styles: &Styles, message: &str) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "  {}",
        paint(styles.dim, terminal::escape(message))
    );
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

/// Install logging only where it was asked for: an explicit log file, or
/// stderr for report commands when `RUST_LOG` is set. The explorer owns the
/// terminal, so it never logs to stderr.
fn init_logging(log_file: Option<&Path>, explorer: bool) -> Result<()> {
    use tracing_subscriber::EnvFilter;
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_ansi(false);
    let result = match log_file {
        Some(path) => {
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(path).wrap_err_with(|| {
                format!("cannot open log file {}", escape_filename(path.as_os_str()))
            })?;
            builder
                .with_env_filter(filter())
                .with_writer(std::sync::Arc::new(bounded_log::BoundedLog::new(file)))
                .try_init()
        }
        None if !explorer && std::env::var_os("RUST_LOG").is_some() => builder
            .with_env_filter(filter())
            .with_writer(std::io::stderr)
            .try_init(),
        None => return Ok(()),
    };
    result.map_err(|error| eyre!("failed to initialize tracing: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn fatal_error_chain_is_escaped_as_a_single_line() {
        let error = eyre!("source\n\u{1b}]52;c;payload\u{7}").wrap_err("context\u{202e}");
        let mut output = Vec::new();
        write_error(&error, &Styles::plain(), &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with("error: context"));
        assert!(text.contains(r"source\n\u{1b}]52;c;payload\u{7}"));
        assert!(text.contains(r"context\u{202e}"));
        assert_eq!(text.matches('\n').count(), 1);
        assert!(!text.contains(['\u{1b}', '\u{7}', '\u{202e}']));
    }

    #[test]
    fn cancelled_commands_stop_and_preserve_existing_export() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("report.json");
        std::fs::write(&destination, "existing report").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let context = Context::plain(cancel);
        let parse = |arguments: &[&str]| {
            let mut all = vec!["grav"];
            all.extend_from_slice(arguments);
            all.push(root.path().to_str().unwrap());
            Cli::try_parse_from(all).unwrap().command.unwrap()
        };
        let output = destination.to_str().unwrap();
        for command in [
            parse(&["scan", "-f", "json"]),
            parse(&["duplicates", "--min-size", "1"]),
            parse(&["age", "-f", "json"]),
            parse(&["export", "-o", output]),
        ] {
            let result = match &command {
                Command::Scan(command) => commands::scan::run(command, &context),
                Command::Duplicates(command) => commands::duplicates::run(command, &context),
                Command::Age(command) => commands::age::run(command, &context),
                Command::Export(command) => commands::export::run(command, &context),
                _ => unreachable!(),
            };
            assert!(result.is_err(), "{command:?}");
        }
        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "existing report"
        );
    }
}
