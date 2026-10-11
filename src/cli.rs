//! Command-line interface definition.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use gravityfile_core::ScanConfig;
use gravityfile_core::units::{Units, parse_age, parse_size};

const AFTER_HELP: &str = "\
Run `grav` with no command to open the explorer.

Exit status: 0 success, 1 error, 2 usage error, 3 incomplete results with
--strict, 130 interrupted.";

/// Disk usage analyzer and file explorer.
#[derive(Debug, Parser)]
#[command(
    name = "grav",
    version,
    about,
    after_help = AFTER_HELP,
    args_conflicts_with_subcommands = true,
    max_term_width = 100
)]
pub struct Cli {
    /// Directory to open in the explorer, or a snapshot to browse [default: current directory]
    pub path: Option<PathBuf>,

    /// Start a full scan as soon as the explorer opens
    #[arg(short, long)]
    pub scan: bool,

    /// Show file lengths instead of disk usage (toggle with `b`)
    #[arg(short = 'A', long)]
    pub apparent_size: bool,

    /// Write the explorer's directory at exit to FILE (see `grav init`)
    #[arg(long, value_name = "FILE")]
    pub cwd_file: Option<PathBuf>,

    /// Print the explorer's directory at exit
    #[arg(long)]
    pub print_cwd: bool,

    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// When to use colour
    #[arg(long, global = true, value_enum, default_value_t = ColorWhen::Auto, value_name = "WHEN")]
    pub color: ColorWhen,

    /// Show sizes in powers of 1024 (KiB, MiB) instead of 1000 (kB, MB)
    #[arg(long, global = true)]
    pub binary: bool,

    /// Append diagnostic logs to FILE (filter with RUST_LOG)
    #[arg(long, global = true, value_name = "FILE", env = "GRAVITYFILE_LOG_FILE")]
    pub log_file: Option<PathBuf>,
}

impl GlobalArgs {
    pub fn units(&self) -> Units {
        if self.binary {
            Units::Binary
        } else {
            Units::Si
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show what takes up space, largest first
    Scan(ScanCommand),
    /// Find files with identical contents
    Duplicates(DuplicatesCommand),
    /// Break files down by age and find stale directories
    Age(AgeCommand),
    /// Show what grew, shrank, appeared or went between two scans
    Diff(DiffCommand),
    /// Show how a directory's size changed across recorded scans
    History(HistoryCommand),
    /// Write a complete scan as a JSON snapshot
    Export(ExportCommand),
    /// Print shell integration that follows the explorer's directory on exit
    #[command(after_help = "Add to your shell's startup file:\n  \
        bash:  eval \"$(command grav init bash)\"\n  \
        zsh:   eval \"$(command grav init zsh)\"\n  \
        fish:  command grav init fish | source")]
    Init {
        #[arg(value_enum)]
        shell: InitShell,
    },
    /// Print shell completions
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the manual page in roff format
    #[command(hide = true)]
    Man,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InitShell {
    Bash,
    Zsh,
    Fish,
}

/// What to scan and how to count it.
#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Scanning")]
pub struct ScanArgs {
    /// Directory to scan, or a snapshot saved by `grav export` or `ncdu -o`
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Count file lengths instead of disk usage
    #[arg(short = 'A', long)]
    pub apparent_size: bool,

    /// Count every hard link to a file, not only the first
    #[arg(short = 'l', long)]
    pub count_links: bool,

    /// Follow symbolic links
    #[arg(short = 'L', long)]
    pub follow_symlinks: bool,

    /// Descend into other filesystems [default: stay on the scanned one]
    #[arg(long)]
    pub cross_fs: bool,

    /// Skip entries whose names start with "."
    #[arg(long)]
    pub no_hidden: bool,

    /// Skip entries matching GLOB, by name or by path from the root (repeatable)
    #[arg(short, long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Scanning threads [default: one per CPU]
    #[arg(short = 'j', long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
    pub threads: Option<u16>,

    /// Annotate entries with Git status
    #[arg(long)]
    pub git: bool,

    /// Fail if the scan reaches more than N entries
    #[arg(long, value_name = "N")]
    pub max_entries: Option<u64>,
}

impl ScanArgs {
    /// Options given that only make sense when walking a filesystem.
    pub fn walk_only_options(&self) -> Vec<&'static str> {
        [
            (self.follow_symlinks, "--follow-symlinks"),
            (self.cross_fs, "--cross-fs"),
            (self.no_hidden, "--no-hidden"),
            (!self.exclude.is_empty(), "--exclude"),
            (self.threads.is_some(), "--threads"),
            (self.git, "--git"),
        ]
        .into_iter()
        .filter_map(|(given, name)| given.then_some(name))
        .collect()
    }

    pub fn config(&self, root: &Path) -> ScanConfig {
        let mut config = ScanConfig::new(root);
        config.apparent_size = self.apparent_size;
        config.count_hard_links = self.count_links;
        config.follow_symlinks = self.follow_symlinks;
        config.cross_filesystems = self.cross_fs;
        config.include_hidden = !self.no_hidden;
        config.ignore_patterns = self.exclude.clone();
        config.threads = self.threads.map_or(0, usize::from);
        config.git_status = self.git;
        config.max_entries = self.max_entries;
        config
    }
}

/// How results are written.
#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Output")]
pub struct OutputArgs {
    /// Output format
    #[arg(short, long, value_enum, default_value_t = Format::Text)]
    pub format: Format,

    /// Exit with status 3 if anything could not be read or analyzed
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Human-readable text
    Text,
    /// Versioned JSON (see `schema_version`)
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    /// The complete tree in versioned JSON, readable by every grav command
    Json,
    /// An ncdu dump, readable by `ncdu -f` and `gdu -f`
    Ncdu,
}

#[derive(Debug, Args)]
pub struct ScanCommand {
    #[command(flatten)]
    pub scan: ScanArgs,

    /// Add this scan's totals to the history shown by `grav history`
    #[arg(long)]
    pub record: bool,

    /// Levels of directories to show below the root
    #[arg(
        short,
        long,
        value_name = "N",
        default_value_t = 1,
        help_heading = "Output"
    )]
    pub depth: u32,

    /// Entries to show per directory, largest first (0 shows all)
    #[arg(
        short = 'n',
        long,
        value_name = "N",
        default_value_t = 20,
        help_heading = "Output"
    )]
    pub limit: usize,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct DuplicatesCommand {
    #[command(flatten)]
    pub scan: ScanArgs,

    /// Ignore files smaller than SIZE (e.g. 100kB, 1MiB)
    #[arg(long, value_name = "SIZE", default_value = "1MB", value_parser = parse_size)]
    pub min_size: u64,

    /// Groups to show, most wasted space first (0 shows all)
    #[arg(
        short = 'n',
        long,
        value_name = "N",
        default_value_t = 20,
        help_heading = "Output"
    )]
    pub limit: usize,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct AgeCommand {
    #[command(flatten)]
    pub scan: ScanArgs,

    /// Directories untouched for longer than AGE are stale (e.g. 30d, 6mo, 1y)
    #[arg(long, value_name = "AGE", default_value = "1y", value_parser = parse_age)]
    pub stale: std::time::Duration,

    /// Stale directories to show, largest first (0 shows all)
    #[arg(
        short = 'n',
        long,
        value_name = "N",
        default_value_t = 10,
        help_heading = "Output"
    )]
    pub limit: usize,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct HistoryCommand {
    /// Directory whose history to show [default: current directory]
    pub path: Option<PathBuf>,

    /// List every directory with recorded scans instead
    #[arg(long, conflicts_with = "path")]
    pub all: bool,

    /// Most recent scans to show (0 shows all)
    #[arg(
        short = 'n',
        long,
        value_name = "N",
        default_value_t = 20,
        help_heading = "Output"
    )]
    pub limit: usize,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct DiffCommand {
    /// The earlier side: a snapshot, or a directory scanned now
    pub old: PathBuf,

    /// The later side: a snapshot or directory [default: OLD's directory, scanned now]
    pub new: Option<PathBuf>,

    /// Count file lengths instead of disk usage
    #[arg(short = 'A', long)]
    pub apparent_size: bool,

    /// Count every hard link to a file, not only the first
    #[arg(short = 'l', long)]
    pub count_links: bool,

    /// Levels of directories to show below the root
    #[arg(
        short,
        long,
        value_name = "N",
        default_value_t = 2,
        help_heading = "Output"
    )]
    pub depth: u32,

    /// Changes to show per directory, largest first (0 shows all)
    #[arg(
        short = 'n',
        long,
        value_name = "N",
        default_value_t = 10,
        help_heading = "Output"
    )]
    pub limit: usize,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct ExportCommand {
    #[command(flatten)]
    pub scan: ScanArgs,

    /// Snapshot format
    #[arg(short, long, value_enum, default_value_t = ExportFormat::Json, help_heading = "Output")]
    pub format: ExportFormat,

    /// Write to FILE instead of standard output, replacing it atomically;
    /// compressed with zstd if FILE ends in .zst
    #[arg(short, long, value_name = "FILE", help_heading = "Output")]
    pub output: Option<PathBuf>,

    /// Exit with status 3 if anything could not be read
    #[arg(long, help_heading = "Output")]
    pub strict: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn scan_flags_map_onto_the_scan_configuration() {
        let cli = Cli::try_parse_from([
            "grav",
            "scan",
            "/r",
            "-A",
            "-l",
            "-L",
            "--cross-fs",
            "--no-hidden",
            "-e",
            "*.o",
            "-e",
            "target",
            "-j",
            "4",
            "--git",
            "--max-entries",
            "9",
        ])
        .unwrap();
        let Some(Command::Scan(command)) = cli.command else {
            panic!("scan command expected");
        };
        let config = command.scan.config(Path::new("/r"));
        assert!(config.apparent_size && config.count_hard_links && config.follow_symlinks);
        assert!(config.cross_filesystems && !config.include_hidden && config.git_status);
        assert_eq!(config.ignore_patterns, ["*.o", "target"]);
        assert_eq!((config.threads, config.max_entries), (4, Some(9)));
    }

    #[test]
    fn sizes_and_ages_are_validated_while_parsing() {
        assert!(Cli::try_parse_from(["grav", "duplicates", "--min-size", "2x"]).is_err());
        assert!(Cli::try_parse_from(["grav", "age", "--stale", "6m"]).is_err());
        assert!(Cli::try_parse_from(["grav", "scan", "-j", "0"]).is_err());
        let cli = Cli::try_parse_from(["grav", "duplicates", "--min-size", "1MiB"]).unwrap();
        let Some(Command::Duplicates(command)) = cli.command else {
            panic!("duplicates command expected");
        };
        assert_eq!(command.min_size, 1 << 20);
    }

    #[test]
    fn explorer_path_and_commands_do_not_mix() {
        assert!(
            Cli::try_parse_from(["grav", "/tmp"])
                .unwrap()
                .command
                .is_none()
        );
        // Once an explorer option is given, a command name is a directory.
        let cli = Cli::try_parse_from(["grav", "--print-cwd", "scan"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.path.as_deref(), Some(Path::new("scan")));
        assert!(Cli::try_parse_from(["grav", "scan", "--print-cwd"]).is_err());
    }
}
