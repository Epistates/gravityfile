//! Versioned recovery inspection output. This document is never a replay plan.

use std::io::Write;
use std::path::Path;

use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};

use crate::native_path::NativePath;
use crate::{RecoveryDiscovery, RecoveryIssue, RecoveryRecord};

pub(crate) fn write(
    report: &RecoveryDiscovery,
    parent: &Path,
    writer: impl Write,
) -> Result<(), serde_json::Error> {
    #[derive(Serialize)]
    struct Status {
        state: &'static str,
        issue_count: usize,
    }
    #[derive(Serialize)]
    struct Document<'a> {
        schema_version: u32,
        kind: &'static str,
        purpose: &'static str,
        parent: NativePath<'a>,
        status: Status,
        records: Records<'a>,
        rename_intents: RenameIntents<'a>,
        archive_records: Archives<'a>,
        issues: Issues<'a>,
    }
    serde_json::to_writer_pretty(
        writer,
        &Document {
            schema_version: 1,
            kind: "recovery",
            purpose: "recovery_inspection_only",
            parent: NativePath(parent),
            status: Status {
                state: if report.issues.is_empty() {
                    "complete"
                } else {
                    "partial"
                },
                issue_count: report.issues.len(),
            },
            records: Records(&report.records),
            rename_intents: RenameIntents(&report.rename_intents),
            archive_records: Archives(&report.archive_records),
            issues: Issues(&report.issues),
        },
    )
}

struct Records<'a>(&'a [RecoveryRecord]);
struct Archives<'a>(&'a [crate::ArchiveRecoveryRecord]);
impl Serialize for Archives<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Record<'a> {
            transaction: NativePath<'a>,
            destination: NativePath<'a>,
            staging: NativePath<'a>,
            status: &'static str,
            entry_count: usize,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            sequence.serialize_element(&Record {
                transaction: NativePath(&record.transaction),
                destination: NativePath(&record.destination),
                staging: NativePath(&record.staging),
                status: record.status,
                entry_count: record.entry_count,
            })?;
        }
        sequence.end()
    }
}
impl Serialize for Records<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            sequence.serialize_element(&Record(record))?;
        }
        sequence.end()
    }
}

struct Record<'a>(&'a RecoveryRecord);
impl Serialize for Record<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let r = self.0;
        let mut record = serializer.serialize_struct("RecoveryRecord", 8)?;
        record.serialize_field("purpose", &r.purpose)?;
        record.serialize_field("transaction", &NativePath(&r.transaction))?;
        record.serialize_field("destination", &NativePath(&r.destination))?;
        record.serialize_field("move_source", &r.move_source.as_deref().map(NativePath))?;
        record.serialize_field("published", &r.published)?;
        record.serialize_field("original_available", &r.original_available)?;
        record.serialize_field("payload_available", &r.payload_available)?;
        record.serialize_field("destination_occupied", &r.destination_occupied)?;
        record.end()
    }
}

struct Issues<'a>(&'a [RecoveryIssue]);
impl Serialize for Issues<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        #[derive(Serialize)]
        struct Issue<'a> {
            path: NativePath<'a>,
            message: String,
            error_kind: &'static str,
            raw_os_error: Option<i32>,
        }
        for issue in self.0 {
            sequence.serialize_element(&Issue {
                path: NativePath(&issue.path),
                message: issue.error.to_string(),
                error_kind: error_kind_code(issue.error.kind()),
                raw_os_error: issue.error.raw_os_error(),
            })?;
        }
        sequence.end()
    }
}

// Stable report vocabulary, deliberately independent of ErrorKind's Debug text.
// Additional/unknown OS error kinds retain their diagnostic and raw code as "other".
fn error_kind_code(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::AlreadyExists => "already_exists",
        ErrorKind::InvalidData => "invalid_data",
        ErrorKind::InvalidInput => "invalid_input",
        ErrorKind::WouldBlock => "would_block",
        ErrorKind::Interrupted => "interrupted",
        ErrorKind::TimedOut => "timed_out",
        ErrorKind::UnexpectedEof => "unexpected_eof",
        ErrorKind::Unsupported => "unsupported",
        _ => "other",
    }
}

struct RenameIntents<'a>(&'a [crate::RenameIntentRecord]);
impl Serialize for RenameIntents<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Intent<'a> {
            locked_during_inspection: bool,
            transaction: NativePath<'a>,
            intent: NativePath<'a>,
            source: NativePath<'a>,
            staged: NativePath<'a>,
            destination: NativePath<'a>,
            outcome: &'static str,
            last_recorded_phase: Option<crate::RenamePhase>,
            object_identity: &'a Option<crate::ObjectIdentity>,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            sequence.serialize_element(&Intent {
                locked_during_inspection: record.locked_during_inspection,
                transaction: NativePath(&record.transaction),
                intent: NativePath(&record.intent),
                source: NativePath(&record.source),
                staged: NativePath(&record.staged),
                destination: NativePath(&record.destination),
                outcome: "unverified; inspection only, not a replay plan",
                last_recorded_phase: record.last_recorded_phase,
                object_identity: &record.object_identity,
            })?;
        }
        sequence.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RecoveryPurpose;
    use std::io;
    use std::path::PathBuf;

    #[test]
    fn issue_categories_preserve_diagnostics_and_unknown_fallback() {
        let cases = [
            (io::ErrorKind::PermissionDenied, "permission_denied"),
            (io::ErrorKind::WouldBlock, "would_block"),
            (io::ErrorKind::Unsupported, "unsupported"),
            (io::ErrorKind::ConnectionReset, "other"),
        ];
        let report = RecoveryDiscovery {
            issues: cases
                .iter()
                .map(|(kind, _)| RecoveryIssue {
                    path: PathBuf::from("journal"),
                    error: io::Error::new(*kind, "diagnostic without classification hints"),
                })
                .collect(),
            ..RecoveryDiscovery::default()
        };
        let mut bytes = Vec::new();
        report.write_json(Path::new("parent"), &mut bytes).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["status"]["state"], "partial");
        for (issue, (_, expected)) in doc["issues"].as_array().unwrap().iter().zip(cases) {
            assert_eq!(issue["error_kind"], expected);
            assert_eq!(issue["message"], "diagnostic without classification hints");
            assert!(issue["raw_os_error"].is_null());
        }
    }

    #[test]
    fn report_preserves_native_paths_and_partial_evidence() {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(b"item-\xff\n".to_vec()))
        };
        #[cfg(windows)]
        let path = {
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&[0x69, 0xd800, 0x0a]))
        };
        let report = RecoveryDiscovery {
            archive_records: Vec::new(),
            rename_intents: Vec::new(),
            records: vec![RecoveryRecord {
                purpose: RecoveryPurpose::CopyUndo,
                transaction: path.clone(),
                destination: path.clone(),
                move_source: Some(path.clone()),
                published: false,
                original_available: true,
                payload_available: false,
                destination_occupied: false,
            }],
            issues: vec![RecoveryIssue {
                path: path.clone(),
                error: io::Error::from_raw_os_error(2),
            }],
        };
        let mut bytes = Vec::new();
        report.write_json(&path, &mut bytes).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["kind"], "recovery");
        assert_eq!(doc["purpose"], "recovery_inspection_only");
        assert_eq!(
            doc["status"],
            serde_json::json!({"state":"partial", "issue_count":1})
        );
        assert_eq!(doc["records"][0]["purpose"], "copy_undo");
        assert_eq!(doc["issues"][0]["raw_os_error"], 2);
        for value in [
            &doc["parent"],
            &doc["records"][0]["transaction"],
            &doc["records"][0]["destination"],
            &doc["records"][0]["move_source"],
            &doc["issues"][0]["path"],
        ] {
            #[cfg(unix)]
            {
                assert_eq!(value["platform"], "unix");
                assert_eq!(value["bytes"], serde_json::json!(b"item-\xff\n".to_vec()));
            }
            #[cfg(windows)]
            {
                assert_eq!(value["platform"], "windows");
                assert_eq!(value["wide"], serde_json::json!([0x69, 0xd800, 0x0a]));
            }
        }
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected write failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(report.write_json(&path, Failing).is_err());
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.issues.len(), 1);
    }
}
