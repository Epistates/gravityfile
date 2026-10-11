//! One module per subcommand.

pub mod age;
pub mod diff;
pub mod duplicates;
pub mod export;
pub mod history;
pub mod scan;
pub mod shell;

use color_eyre::Result;
use color_eyre::eyre::{Context as _, eyre};
use gravityfile_core::units::Units;
use gravityfile_core::{FileTree, escape_filename};
use tokio_util::sync::CancellationToken;

use crate::cli::ScanArgs;
use crate::style::Styles;

/// What every report command shares.
pub struct Context {
    pub cancel: CancellationToken,
    pub units: Units,
    /// Styles for standard output.
    pub out: Styles,
    /// Styles for standard error.
    pub err: Styles,
}

impl Context {
    #[cfg(test)]
    pub fn plain(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            units: Units::Si,
            out: Styles::plain(),
            err: Styles::plain(),
        }
    }

    pub fn size(&self, bytes: u64) -> String {
        gravityfile_core::units::format_size(bytes, self.units)
    }
}

/// `count` followed by the singular or plural noun: `1 file`, `2,048 files`.
pub fn counted(count: u64, one: &str, many: &str) -> String {
    let noun = if count == 1 { one } else { many };
    format!("{} {noun}", gravityfile_core::units::format_count(count))
}

/// Whether `args` names a saved snapshot rather than a directory to scan.
pub fn is_snapshot(args: &ScanArgs) -> bool {
    std::fs::metadata(&args.path).is_ok_and(|metadata| metadata.is_file())
}

/// Scan the directory named by `args`, or read the snapshot it names.
pub fn scan_tree(args: &ScanArgs, context: &Context) -> Result<FileTree> {
    if is_snapshot(args) {
        let ignored = args.walk_only_options();
        if !ignored.is_empty() {
            return Err(eyre!(
                "{} cannot be used with a snapshot, which was already scanned",
                ignored.join(", ")
            ));
        }
        let options = gravityfile_scan::snapshot::ImportOptions {
            apparent: args.apparent_size,
            count_hard_links: args.count_links,
            max_entries: args.max_entries,
        };
        return gravityfile_scan::snapshot::read(&args.path, &options)
            .wrap_err_with(|| format!("cannot load {}", escape_filename(args.path.as_os_str())));
    }
    let root = args
        .path
        .canonicalize()
        .wrap_err_with(|| format!("cannot open {}", escape_filename(args.path.as_os_str())))?;
    let config = args.config(&root);
    crate::progress::scan(&config, &context.cancel, context.units)
        .wrap_err_with(|| format!("cannot scan {}", escape_filename(root.as_os_str())))
}
