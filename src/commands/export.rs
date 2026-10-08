//! `grav export`: the complete scan as a JSON snapshot.

use color_eyre::Result;
use gravityfile_core::escape_filename;

use super::Context;
use crate::cli::ExportCommand;
use crate::report::{
    Completeness, ReportEnvelope, completeness, scan_status, write_json_atomic_with,
    write_json_report_with,
};

pub fn run(command: &ExportCommand, context: &Context) -> Result<Completeness> {
    let tree = super::scan_tree(&command.scan, context)?;
    let status = scan_status(&tree);
    let envelope = ReportEnvelope::new("scan", &tree, &status, &tree);
    let encode = |writer: &mut dyn std::io::Write| crate::tree_json::write(writer, &envelope);
    match &command.output {
        Some(path) => {
            write_json_atomic_with(path, &context.cancel, encode)?;
            crate::note(
                &context.err,
                &format!("wrote {}", escape_filename(path.as_os_str())),
            );
        }
        None => write_json_report_with(&mut std::io::stdout().lock(), &context.cancel, encode)?,
    }
    Ok(completeness(&status, &context.err))
}
