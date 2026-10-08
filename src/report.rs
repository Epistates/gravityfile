//! Report envelope, completeness, and safe publication of report output.

use std::io::{BufWriter, Seek, Write};
use std::path::Path;

use color_eyre::Result;
use color_eyre::eyre::Context;
use gravityfile_analyze::{AnalysisState, AnalysisStatus};
use gravityfile_core::{FileTree, ScanConfig, escape_filename};
use tokio_util::sync::CancellationToken;

use crate::atomic_export::StagedExport;

/// Version of every JSON document the CLI emits. Bump on any incompatible
/// change to an envelope or its data.
pub const SCHEMA_VERSION: u32 = 2;

/// Every JSON report: what was examined, how, whether it is complete, and the
/// command's data.
#[derive(serde::Serialize)]
pub struct ReportEnvelope<'a, T> {
    pub schema_version: u32,
    pub tool_version: &'static str,
    pub kind: &'static str,
    #[serde(serialize_with = "gravityfile_core::path_serde::serialize")]
    pub root: &'a Path,
    pub scope: &'a ScanConfig,
    pub parameters: serde_json::Value,
    pub status: &'a AnalysisStatus,
    pub data: &'a T,
}

impl<'a, T> ReportEnvelope<'a, T> {
    pub fn new(
        kind: &'static str,
        tree: &'a FileTree,
        status: &'a AnalysisStatus,
        data: &'a T,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
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

/// Completeness of a plain scan: partial when anything could not be read.
pub fn scan_status(tree: &FileTree) -> AnalysisStatus {
    AnalysisStatus {
        state: if tree.has_warnings() {
            AnalysisState::Partial
        } else {
            AnalysisState::Complete
        },
        scan_warning_count: tree.warning_count(),
        ..Default::default()
    }
}

/// Whether a command's results cover everything it was asked to examine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Incomplete,
}

/// Explain an incomplete report on stderr.
pub fn completeness(status: &AnalysisStatus, styles: &crate::style::Styles) -> Completeness {
    if status.is_complete() {
        return Completeness::Complete;
    }
    use crate::commands::counted;
    let mut problems = Vec::new();
    if status.scan_warning_count > 0 {
        problems.push(format!(
            "{} could not be read",
            counted(status.scan_warning_count, "entry", "entries")
        ));
    }
    if status.issue_count > 0 {
        problems.push(format!(
            "{} could not be analyzed",
            counted(status.issue_count, "file", "files")
        ));
    }
    if problems.is_empty() {
        problems.push(format!("report is {}", status.state));
    }
    crate::warn(
        styles,
        &format!("{}; results are a lower bound", problems.join(", ")),
    );
    for issue in &status.issues {
        crate::note(
            styles,
            &format!(
                "{}: {}",
                escape_filename(issue.path.as_os_str()),
                crate::terminal::escape(&issue.message)
            ),
        );
    }
    Completeness::Incomplete
}

/// Check cancellation at buffered I/O boundaries without using Interrupted,
/// which Write::write_all retries and could otherwise loop forever.
pub struct ReportWriter<'a, W: Write + ?Sized> {
    pub output: &'a mut W,
    pub cancel: &'a CancellationToken,
}

impl<W: Write + ?Sized> Write for ReportWriter<'_, W> {
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

/// Finish serialization before publishing any report bytes. Cancellation before
/// publication leaves the destination untouched; cancellation during copying
/// can leave a prefix, as with any interrupted output stream.
pub fn write_json_report(
    value: &impl serde::Serialize,
    destination: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<()> {
    write_report_with(destination, cancel, |writer| {
        serde_json::to_writer_pretty(writer, value)?;
        Ok(())
    })
}

pub fn write_report_with(
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

#[cfg(test)]
fn write_json_atomic(
    path: &Path,
    value: &impl serde::Serialize,
    cancel: &CancellationToken,
) -> Result<()> {
    publish_atomic(path, cancel, |writer| {
        serde_json::to_writer_pretty(writer, value)?;
        Ok(())
    })
}

/// Write what `encode` produces, plus a final newline, to `path`, publishing
/// it only once complete: a private temporary file beside `path` is written,
/// synced and renamed into place. Any failure first leaves an existing file
/// intact; a symlink at `path` is replaced, never followed. A `.zst` path is
/// compressed with zstd.
pub fn publish_atomic(
    path: &Path,
    cancel: &CancellationToken,
    encode: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    publish_atomic_with_sync(path, cancel, encode, StagedExport::sync_parent)
}

fn publish_atomic_with_sync(
    path: &Path,
    cancel: &CancellationToken,
    encode: impl FnOnce(&mut dyn Write) -> Result<()>,
    sync_parent: impl FnOnce(&StagedExport) -> std::io::Result<()>,
) -> Result<()> {
    check_report_cancel(cancel)?;
    let mut staged = StagedExport::new(path)?;
    {
        let mut writer = BufWriter::new(ReportWriter {
            output: staged.file_mut(),
            cancel,
        });
        if path.extension().is_some_and(|extension| extension == "zst") {
            let mut compressed = zstd::stream::write::Encoder::new(&mut writer, 3)?;
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
            compressed.multithread(threads as u32)?;
            encode(&mut compressed)?;
            writeln!(compressed)?;
            compressed.finish()?;
        } else {
            encode(&mut writer)?;
            writeln!(writer)?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::eyre;

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
        let error = publish_atomic_with_sync(
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
                let result = publish_atomic(&output, &CancellationToken::new(), |writer| {
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
        let error = publish_atomic_with_sync(
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
}
