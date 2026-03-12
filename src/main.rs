//! gravityfile - A state-of-the-art file system analyzer with TUI.
//!
//! Usage:
//!   grav [PATH]              Launch interactive TUI
//!   grav scan [PATH]         Quick scan summary
//!   grav duplicates [PATH]   Find duplicate files
//!   grav age [PATH]          Analyze file ages

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::Result;
use color_eyre::eyre::Context;
use gravityfile_analyze::{AgeAnalyzer, AgeConfig, DuplicateConfig, DuplicateFinder};
use gravityfile_core::ScanConfig;
use gravityfile_scan::JwalkScanner;
use humansize::{BINARY, format_size as format_size_human};
use tokio_util::sync::CancellationToken;

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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Quick scan summary
    Scan {
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

fn main() -> Result<()> {
    color_eyre::install()?;

    // Initialize tracing
    let log_file = std::env::temp_dir().join("gravityfile.log");
    let file = std::fs::File::create(&log_file)?;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::sync::Arc::new(file))
        .with_ansi(false)
        .init();

    tracing::info!("Starting gravityfile v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Log file: {}", log_file.display());

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Scan {
            path,
            max_depth,
            top,
        }) => {
            run_scan(&path, max_depth, top)?;
            std::process::exit(0);
        }
        Some(Command::Duplicates {
            path,
            min_size,
            top,
            format,
        }) => {
            run_duplicates(&path, &min_size, top, format)?;
            std::process::exit(0);
        }
        Some(Command::Age {
            path,
            stale,
            format,
        }) => {
            run_age(&path, &stale, format)?;
            std::process::exit(0);
        }
        Some(Command::Export { path, output }) => {
            run_export(&path, output)?;
            std::process::exit(0);
        }
        None => {
            // Launch TUI
            let path = cli.path.canonicalize().context("Invalid path")?;
            let config = gravityfile_tui::TuiConfig::new()
                .with_scan_on_startup(cli.scan)
                .with_cwd_file(cli.cwd_file)
                .with_print_cwd(cli.print_cwd);
            gravityfile_tui::run_with_config(path, config)?;
            std::process::exit(0);
        }
    }
}

/// Run a quick scan and display summary.
fn run_scan(path: &Path, max_depth: Option<u32>, top_n: usize) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config, CancellationToken::new()).context("Scan failed")?;

    // Print summary
    println!();
    println!("{}", "─".repeat(60));
    println!(
        " {} - {}",
        path.display(),
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
        &path,
        0,
        max_depth.unwrap_or(u32::MAX),
        top_n,
        tree.root.size,
    );

    if !tree.warnings.is_empty() {
        println!();
        println!("{} warning(s) during scan", tree.warnings.len());
    }

    Ok(())
}

/// Run duplicate detection.
fn run_duplicates(path: &Path, min_size: &str, top_n: usize, format: OutputFormat) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;
    let min_bytes = parse_size(min_size)?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config, CancellationToken::new()).context("Scan failed")?;

    eprintln!("Finding duplicates (min size: {})...", min_size);

    let dup_config = DuplicateConfig::builder()
        .min_size(min_bytes)
        .max_groups(top_n)
        .build()
        .unwrap();

    let finder = DuplicateFinder::with_config(dup_config);
    let report = finder.find_duplicates(&tree, CancellationToken::new());

    match format {
        OutputFormat::Text => {
            println!();
            println!("{}", "─".repeat(60));
            println!(" DUPLICATE REPORT - {}", path.display());
            println!(
                " {} groups | {} files | {} wasted",
                report.group_count,
                report.files_with_duplicates,
                format_size(report.total_wasted_space)
            );
            println!("{}", "─".repeat(60));
            println!();

            for (i, group) in report.groups.iter().enumerate() {
                println!(
                    " [{}] {} ({} × {})",
                    i + 1,
                    format_size(group.wasted_bytes),
                    group.paths.len(),
                    format_size(group.size)
                );
                for path in &group.paths {
                    println!("   {}", path.display());
                }
                println!();
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }

    Ok(())
}

/// Run age analysis.
fn run_age(path: &Path, stale_threshold: &str, format: OutputFormat) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;
    let stale_duration = parse_duration(stale_threshold)?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config, CancellationToken::new()).context("Scan failed")?;

    eprintln!("Analyzing file ages...");

    let age_config = AgeConfig::builder()
        .stale_threshold(stale_duration)
        .build()
        .unwrap();

    let analyzer = AgeAnalyzer::with_config(age_config);
    let report = analyzer.analyze(&tree, CancellationToken::new());

    match format {
        OutputFormat::Text => {
            println!();
            println!("{}", "─".repeat(60));
            println!(" AGE ANALYSIS - {}", path.display());
            println!(
                " {} files | {} | Avg age: {}",
                report.total_files,
                format_size(report.total_size),
                gravityfile_analyze::format_age(report.average_age)
            );
            println!("{}", "─".repeat(60));
            println!();

            println!(" DISTRIBUTION:");
            for bucket in &report.buckets {
                let bar_width = 20;
                let filled = if report.total_files > 0 {
                    (bucket.file_count as f64 / report.total_files as f64 * bar_width as f64).round()
                        as usize
                } else {
                    0
                };
                let bar = "█".repeat(filled);
                println!(
                    " {:<12} {:>8} files  {}",
                    bucket.name, bucket.file_count, bar
                );
            }

            if !report.stale_directories.is_empty() {
                println!();
                println!(" STALE DIRECTORIES (> {}):", stale_threshold);
                for (i, dir) in report.stale_directories.iter().enumerate().take(10) {
                    println!(
                        " [{}] {} - {} (newest: {})",
                        i + 1,
                        dir.path.display(),
                        format_size(dir.size),
                        gravityfile_analyze::format_age(dir.newest_file_age)
                    );
                }
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }

    Ok(())
}

/// Run export to JSON.
fn run_export(path: &Path, output: Option<PathBuf>) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config, CancellationToken::new()).context("Scan failed")?;

    let json = serde_json::to_string_pretty(&tree)?;

    match output {
        Some(output_path) => {
            std::fs::write(&output_path, json)?;
            eprintln!("Exported to {}", output_path.display());
        }
        None => {
            println!("{}", json);
        }
    }

    Ok(())
}

fn print_node(
    node: &gravityfile_core::FileNode,
    _path: &Path,
    depth: u32,
    max_depth: u32,
    top_n: usize,
    total_size: u64,
) {
    if depth > max_depth {
        return;
    }

    let indent = "  ".repeat(depth as usize);
    let percentage = if total_size > 0 {
        (node.size as f64 / total_size as f64) * 100.0
    } else {
        0.0
    };

    println!(
        "{}{:<30} {:>10} [{:>5.1}%]",
        indent,
        node.name,
        format_size(node.size),
        percentage
    );

    if depth < max_depth && node.is_dir() {
        for child in node.children.iter().take(top_n) {
            print_node(child, _path, depth + 1, max_depth, top_n, total_size);
        }
        if node.children.len() > top_n {
            println!("{}  ... ({} more items)", indent, node.children.len() - top_n);
        }
    }
}

fn format_size(bytes: u64) -> String {
    format_size_human(bytes, BINARY)
}

fn parse_size(s: &str) -> Result<u64> {
    let s = s.to_uppercase();
    if s.ends_with("GB") || s.ends_with("G") {
        let num: f64 = s.trim_end_matches("GB").trim_end_matches('G').parse()?;
        Ok((num * 1024.0 * 1024.0 * 1024.0) as u64)
    } else if s.ends_with("MB") || s.ends_with("M") {
        let num: f64 = s.trim_end_matches("MB").trim_end_matches('M').parse()?;
        Ok((num * 1024.0 * 1024.0) as u64)
    } else if s.ends_with("KB") || s.ends_with("K") {
        let num: f64 = s.trim_end_matches("KB").trim_end_matches('K').parse()?;
        Ok((num * 1024.0) as u64)
    } else if s.ends_with('B') {
        let num: u64 = s.trim_end_matches('B').parse()?;
        Ok(num)
    } else {
        let num: u64 = s.parse()?;
        Ok(num)
    }
}

fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let s = s.to_lowercase();
    let (num, multiplier) = if s.ends_with('y') {
        let num: f64 = s.trim_end_matches('y').parse()?;
        (num, 365.25 * 24.0 * 60.0 * 60.0)
    } else if s.ends_with("mo") {
        let num: f64 = s.trim_end_matches("mo").parse()?;
        (num, 30.44 * 24.0 * 60.0 * 60.0)
    } else if s.ends_with('w') {
        let num: f64 = s.trim_end_matches('w').parse()?;
        (num, 7.0 * 24.0 * 60.0 * 60.0)
    } else if s.ends_with('d') {
        let num: f64 = s.trim_end_matches('d').parse()?;
        (num, 24.0 * 60.0 * 60.0)
    } else if s.ends_with('h') {
        let num: f64 = s.trim_end_matches('h').parse()?;
        (num, 60.0 * 60.0)
    } else {
        let num: f64 = s.parse()?;
        (num, 24.0 * 60.0 * 60.0) // Default to days
    };

    Ok(std::time::Duration::from_secs_f64(num * multiplier))
}
