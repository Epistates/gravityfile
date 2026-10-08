//! One module per subcommand.

pub mod age;
pub mod duplicates;
pub mod export;
pub mod scan;
pub mod shell;

use color_eyre::Result;
use color_eyre::eyre::Context as _;
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

/// Scan the directory named by `args`.
pub fn scan_tree(args: &ScanArgs, context: &Context) -> Result<FileTree> {
    let root = args
        .path
        .canonicalize()
        .wrap_err_with(|| format!("cannot open {}", escape_filename(args.path.as_os_str())))?;
    let config = args.config(&root);
    crate::progress::scan(&config, &context.cancel, context.units)
        .wrap_err_with(|| format!("cannot scan {}", escape_filename(root.as_os_str())))
}
