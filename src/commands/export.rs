//! `grav export`: the complete scan as a snapshot.

use color_eyre::Result;
use gravityfile_core::escape_filename;

use super::Context;
use crate::cli::{ExportCommand, ExportFormat};
use crate::report::{
    Completeness, ReportEnvelope, completeness, publish_atomic, scan_status, write_report_with,
};

pub fn run(command: &ExportCommand, context: &Context) -> Result<Completeness> {
    let tree = super::scan_tree(&command.scan, context)?;
    let status = scan_status(&tree);
    let envelope = ReportEnvelope::new("scan", &tree, &status, &tree);
    let encode = |writer: &mut dyn std::io::Write| match command.format {
        ExportFormat::Json => crate::tree_json::write(writer, &envelope),
        ExportFormat::Ncdu => Ok(gravityfile_scan::ncdu::write(&tree, writer)?),
    };
    match &command.output {
        Some(path) => {
            publish_atomic(path, &context.cancel, encode)?;
            crate::note(
                &context.err,
                &format!("wrote {}", escape_filename(path.as_os_str())),
            );
        }
        None => write_report_with(&mut std::io::stdout().lock(), &context.cancel, encode)?,
    }
    Ok(completeness(&status, &context.err))
}
