//! gravityfile - A state-of-the-art file system analyzer with TUI.
//!
//! Usage:
//!   grav [PATH]              Launch interactive TUI
//!   grav scan [PATH]         Quick scan summary
//!   grav duplicates [PATH]   Find duplicate files
//!   grav age [PATH]          Analyze file ages
//!   grav export [PATH]       Export scan to JSON
//!   grav --help              Show help

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{Context, Result};

use gravityfile_analyze::{AgeAnalyzer, AgeConfig, DuplicateConfig, DuplicateFinder, format_age};
use gravityfile_scan::{JwalkScanner, ScanConfig};

#[derive(Parser)]
#[command(
    name = "gravityfile",
    version,
    about = "A state-of-the-art file system analyzer",
    long_about = "gravityfile helps you understand where your disk space goes.\n\n\
                  Launch the interactive TUI by running `grav [PATH]`, or use \
                  subcommands for quick operations."
)]
struct Cli {
    /// Path to analyze (defaults to current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Start scanning immediately on startup (default: show quick listing only)
    #[arg(short = 'S', long)]
    scan: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Quick scan and show summary
    Scan {
        /// Path to scan
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Maximum depth to display
        #[arg(short, long, default_value = "3")]
        depth: u32,

        /// Show all entries (no depth limit on display)
        #[arg(short, long)]
        all: bool,

        /// Number of top entries to show per directory
        #[arg(short = 'n', long, default_value = "10")]
        top: usize,
    },

    /// Find duplicate files
    Duplicates {
        /// Path to scan
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Minimum file size to consider (e.g., "1KB", "1MB")
        #[arg(short, long, default_value = "1KB")]
        min_size: String,

        /// Maximum number of duplicate groups to show
        #[arg(short = 'n', long, default_value = "20")]
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

        /// Show stale directories older than this (e.g., "1y", "6m", "30d")
        #[arg(short, long, default_value = "1y")]
        stale: String,

        /// Output format
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
    },

    /// Export scan results to JSON
    Export {
        /// Path to scan
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum OutputFormat {
    #[default]
    Text,
    Json,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Scan {
            path,
            depth,
            all,
            top,
        }) => {
            run_scan(&path, if all { None } else { Some(depth) }, top)?;
        }
        Some(Command::Duplicates {
            path,
            min_size,
            top,
            format,
        }) => {
            run_duplicates(&path, &min_size, top, format)?;
        }
        Some(Command::Age {
            path,
            stale,
            format,
        }) => {
            run_age(&path, &stale, format)?;
        }
        Some(Command::Export { path, output }) => {
            run_export(&path, output)?;
        }
        None => {
            // Launch TUI
            let path = cli.path.canonicalize().context("Invalid path")?;
            let config = gravityfile_tui::TuiConfig::new().with_scan_on_startup(cli.scan);
            gravityfile_tui::run_with_config(path, config)?;
        }
    }

    Ok(())
}

/// Run a quick scan and display summary.
fn run_scan(path: &PathBuf, max_depth: Option<u32>, top_n: usize) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config).context("Scan failed")?;

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
    print_node(&tree.root, &path, 0, max_depth.unwrap_or(u32::MAX), top_n, tree.root.size);

    if !tree.warnings.is_empty() {
        println!();
        println!("{} warning(s) during scan", tree.warnings.len());
    }

    Ok(())
}

/// Run duplicate detection.
fn run_duplicates(path: &PathBuf, min_size: &str, top_n: usize, format: OutputFormat) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;
    let min_bytes = parse_size(min_size)?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config).context("Scan failed")?;

    eprintln!("Finding duplicates (min size: {})...", min_size);

    let dup_config = DuplicateConfig::builder()
        .min_size(min_bytes)
        .max_groups(top_n)
        .build()
        .unwrap();

    let finder = DuplicateFinder::with_config(dup_config);
    let report = finder.find_duplicates(&tree);

    match format {
        OutputFormat::Text => {
            println!();
            println!("{}", "─".repeat(70));
            println!(" Duplicate File Report");
            println!("{}", "─".repeat(70));
            println!();

            if report.groups.is_empty() {
                println!(" No duplicate files found.");
            } else {
                println!(
                    " Found {} duplicate groups ({} files)",
                    report.group_count,
                    report.files_with_duplicates
                );
                println!(
                    " Total wasted space: {}",
                    format_size(report.total_wasted_space)
                );
                println!();

                for (i, group) in report.groups.iter().enumerate() {
                    println!(
                        " Group {} ({} files, {} each, {} wasted)",
                        i + 1,
                        group.count(),
                        format_size(group.size),
                        format_size(group.wasted_bytes)
                    );
                    for path in &group.paths {
                        println!("   {}", path.display());
                    }
                    println!();
                }
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }

    Ok(())
}

/// Run age analysis.
fn run_age(path: &PathBuf, stale_threshold: &str, format: OutputFormat) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;
    let stale_duration = parse_duration(stale_threshold)?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config).context("Scan failed")?;

    eprintln!("Analyzing file ages...");

    let age_config = AgeConfig::builder()
        .stale_threshold(stale_duration)
        .build()
        .unwrap();

    let analyzer = AgeAnalyzer::with_config(age_config);
    let report = analyzer.analyze(&tree);

    match format {
        OutputFormat::Text => {
            println!();
            println!("{}", "─".repeat(70));
            println!(" Age Distribution Report");
            println!("{}", "─".repeat(70));
            println!();

            // Age buckets
            println!(" Age Distribution:");
            let max_size = report.buckets.iter().map(|b| b.total_size).max().unwrap_or(1);
            for bucket in &report.buckets {
                let bar_len = ((bucket.total_size as f64 / max_size as f64) * 30.0) as usize;
                let bar = "█".repeat(bar_len);
                println!(
                    "   {:<12} {:>10} {:>8} files  {}",
                    bucket.name,
                    format_size(bucket.total_size),
                    bucket.file_count,
                    bar
                );
            }
            println!();

            // Stale directories
            if !report.stale_directories.is_empty() {
                println!(
                    " Stale Directories (no changes in {}):",
                    stale_threshold
                );
                println!(
                    " Total stale: {}",
                    format_size(report.total_stale_size())
                );
                println!();

                for dir in &report.stale_directories {
                    println!(
                        "   {} ({}, {} ago)",
                        dir.path.display(),
                        format_size(dir.size),
                        format_age(dir.newest_file_age)
                    );
                }
            } else {
                println!(" No stale directories found.");
            }
            println!();
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }

    Ok(())
}

/// Export scan results to JSON.
fn run_export(path: &PathBuf, output: Option<PathBuf>) -> Result<()> {
    let path = path.canonicalize().context("Invalid path")?;

    eprintln!("Scanning {}...", path.display());

    let config = ScanConfig::new(&path);
    let scanner = JwalkScanner::new();
    let tree = scanner.scan(&config).context("Scan failed")?;

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

/// Print a node and its children.
fn print_node(
    node: &gravityfile_core::FileNode,
    path: &PathBuf,
    depth: u32,
    max_depth: u32,
    top_n: usize,
    root_size: u64,
) {
    let indent = "  ".repeat(depth as usize);
    let ratio = if root_size > 0 {
        node.size as f64 / root_size as f64 * 100.0
    } else {
        0.0
    };

    let bar = make_bar(ratio / 100.0, 10);

    let name = if depth == 0 {
        path.display().to_string()
    } else {
        node.name.to_string()
    };

    let dir_marker = if node.is_dir() { "/" } else { "" };

    println!(
        "{}{}{:<40} {:>10} {:>5.1}% {}",
        indent,
        if node.is_dir() { "▼ " } else { "  " },
        truncate(&format!("{}{}", name, dir_marker), 40),
        format_size(node.size),
        ratio,
        bar
    );

    if node.is_dir() && depth < max_depth {
        let children_to_show = node.children.iter().take(top_n);
        let remaining = node.children.len().saturating_sub(top_n);

        for child in children_to_show {
            let child_path = path.join(&*child.name);
            print_node(child, &child_path, depth + 1, max_depth, top_n, root_size);
        }

        if remaining > 0 {
            let indent = "  ".repeat((depth + 1) as usize);
            println!("{}  ... and {} more", indent, remaining);
        }
    }
}

/// Create a simple ASCII bar.
fn make_bar(ratio: f64, width: usize) -> String {
    let filled = (ratio * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

/// Format size in human-readable form.
fn format_size(bytes: u64) -> String {
    humansize::format_size(bytes, humansize::BINARY)
}

/// Truncate a string to max length.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len - 1])
    }
}

/// Parse a size string (e.g., "1KB", "10MB", "1GB").
fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim().to_uppercase();

    let (num, multiplier) = if s.ends_with("GB") || s.ends_with("G") {
        let num: f64 = s.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.').parse()?;
        (num, 1024 * 1024 * 1024)
    } else if s.ends_with("MB") || s.ends_with("M") {
        let num: f64 = s.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.').parse()?;
        (num, 1024 * 1024)
    } else if s.ends_with("KB") || s.ends_with("K") {
        let num: f64 = s.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.').parse()?;
        (num, 1024)
    } else if s.ends_with('B') {
        let num: f64 = s.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.').parse()?;
        (num, 1)
    } else {
        let num: f64 = s.parse()?;
        (num, 1)
    };

    Ok((num * multiplier as f64) as u64)
}

/// Parse a duration string (e.g., "1y", "6m", "30d", "1w").
fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let s = s.trim().to_lowercase();

    let (num, multiplier) = if s.ends_with('y') {
        let num: f64 = s.trim_end_matches('y').parse()?;
        (num, 365.0 * 24.0 * 60.0 * 60.0)
    } else if s.ends_with('m') {
        let num: f64 = s.trim_end_matches('m').parse()?;
        (num, 30.0 * 24.0 * 60.0 * 60.0)
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
