//! Streaming, inspection-only undo review documents. Never an execution format.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};

use crate::native_path::NativePath;
use crate::{UndoEntry, UndoLog, UndoableOperation};

pub(crate) fn write(log: &UndoLog, writer: impl Write) -> Result<(), serde_json::Error> {
    #[derive(Serialize)]
    struct Document<'a> {
        schema_version: u32,
        purpose: &'static str,
        item_outcomes: &'static str,
        entries: Entries<'a>,
    }
    serde_json::to_writer_pretty(
        writer,
        &Document {
            schema_version: 1,
            purpose: "incomplete_undo_review_only",
            item_outcomes: "unverified; original records include items that may already be undone",
            entries: Entries(log),
        },
    )
}

pub(crate) fn export(log: &UndoLog, destination: &Path) -> std::io::Result<()> {
    publish(destination, |writer| {
        write(log, writer).map_err(std::io::Error::other)
    })
}

fn publish(
    destination: &Path,
    write_document: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use crate::copy::{RenameError, rename_noreplace};
    if destination.file_name().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Export destination must name a file",
        ));
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".gravityfile-undo-review-")
        .tempdir_in(parent)?;
    let staged_path = staging.path().join("review.json");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut writer = std::io::BufWriter::new(options.open(&staged_path)?);
    write_document(&mut writer)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    rename_noreplace(&staged_path, destination).map_err(|error| match error {
        RenameError::DestinationExists => std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Export destination already exists",
        ),
        RenameError::CrossDevice => {
            std::io::Error::other("Export staging unexpectedly crossed filesystems")
        }
        RenameError::Io(error) => error,
    })
}

struct Entries<'a>(&'a UndoLog);
impl Serialize for Entries<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for entry in self.0.iter().filter(|entry| entry.review_required) {
            sequence.serialize_element(&Record(entry))?;
        }
        sequence.end()
    }
}

struct Record<'a>(&'a UndoEntry);
impl Serialize for Record<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("UndoReviewRecord", 6)?;
        record.serialize_field("id", &self.0.id)?;
        record.serialize_field("timestamp", &self.0.timestamp)?;
        record.serialize_field("description", &self.0.description)?;
        record.serialize_field("review_required", &true)?;
        record.serialize_field("operation", &Operation(&self.0.operation))?;
        record.serialize_field("last_attempt", &self.0.last_attempt.as_ref().map(Attempt))?;
        record.end()
    }
}

struct Attempt<'a>(&'a crate::UndoAttempt);
impl Serialize for Attempt<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("UndoAttempt", 7)?;
        record.serialize_field("observed_at", &self.0.observed_at)?;
        record.serialize_field("cancelled", &self.0.cancelled)?;
        record.serialize_field("succeeded", &self.0.succeeded)?;
        record.serialize_field("failed", &self.0.failed)?;
        record.serialize_field("skipped", &self.0.skipped)?;
        record.serialize_field("bytes_processed", &self.0.bytes_processed)?;
        record.serialize_field("errors", &Errors(&self.0.errors))?;
        record.end()
    }
}

struct Errors<'a>(&'a [crate::OperationError]);
impl Serialize for Errors<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Error<'a> {
            path: NativePath<'a>,
            message: &'a str,
            omitted_count: usize,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for error in self.0 {
            sequence.serialize_element(&Error {
                path: NativePath(&error.path),
                message: &error.message,
                omitted_count: error.omitted_count,
            })?;
        }
        sequence.end()
    }
}

struct Paths<'a>(&'a [PathBuf]);
impl Serialize for Paths<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for path in self.0 {
            sequence.serialize_element(&NativePath(path))?;
        }
        sequence.end()
    }
}

struct Moves<'a>(&'a [(PathBuf, PathBuf)]);
impl Serialize for Moves<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        #[derive(Serialize)]
        struct Move<'a> {
            original: NativePath<'a>,
            current: NativePath<'a>,
        }
        for (original, current) in self.0 {
            sequence.serialize_element(&Move {
                original: NativePath(original),
                current: NativePath(current),
            })?;
        }
        sequence.end()
    }
}

struct Operation<'a>(&'a UndoableOperation);
impl Serialize for Operation<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let fields = if matches!(self.0, UndoableOperation::FileRenamed { .. }) {
            3
        } else {
            2
        };
        let mut record = serializer.serialize_struct("OriginalOperation", fields)?;
        match self.0 {
            UndoableOperation::FilesMoved { moves } => {
                record.serialize_field("kind", "files_moved")?;
                record.serialize_field("moves", &Moves(moves))?;
            }
            UndoableOperation::FilesCopied { created } => {
                record.serialize_field("kind", "files_copied")?;
                record.serialize_field("created", &Paths(created))?;
            }
            UndoableOperation::FilesDeleted { paths } => {
                record.serialize_field("kind", "files_deleted")?;
                record.serialize_field("paths", &Paths(paths))?;
            }
            UndoableOperation::FileRenamed { path, old_path, .. } => {
                record.serialize_field("kind", "file_renamed")?;
                record.serialize_field("current", &NativePath(path))?;
                record.serialize_field("original", &NativePath(old_path))?;
            }
            UndoableOperation::FileCreated { path } => {
                record.serialize_field("kind", "file_created")?;
                record.serialize_field("path", &NativePath(path))?;
            }
            UndoableOperation::DirectoryCreated { path } => {
                record.serialize_field("kind", "directory_created")?;
                record.serialize_field("path", &NativePath(path))?;
            }
        }
        record.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_export_preserves_structured_diagnostic_omissions() {
        let mut entry = UndoEntry::new(
            1,
            UndoableOperation::FilesCopied {
                created: Vec::new(),
            },
            "review",
        );
        entry.last_attempt = Some(crate::UndoAttempt {
            observed_at: std::time::UNIX_EPOCH,
            cancelled: false,
            succeeded: 0,
            failed: 4321,
            skipped: 0,
            bytes_processed: 0,
            errors: vec![crate::OperationError::omitted(4321)],
        });
        let mut log = UndoLog::new(1);
        log.retain_for_review(entry);
        let mut bytes = Vec::new();
        log.write_review_json(&mut bytes).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let omitted = &document["entries"][0]["last_attempt"]["errors"][0];
        assert_eq!(omitted["omitted_count"], 4321);
        assert!(omitted["message"].as_str().unwrap().contains("omitted"));
    }

    #[test]
    fn file_export_publishes_complete_private_document_and_never_clobbers() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("review.json");
        let mut log = UndoLog::new(1);
        log.retain_for_review(UndoEntry::new(
            1,
            UndoableOperation::FileCreated {
                path: "original".into(),
            },
            "created",
        ));
        log.export_review_json(&destination).unwrap();
        let bytes = std::fs::read(&destination).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["schema_version"],
            1
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&destination)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(
            log.export_review_json(&destination).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        let failed = directory.path().join("failed.json");
        assert!(
            publish(&failed, |writer| {
                writer.write_all(b"partial")?;
                Err(std::io::Error::other("injected serialization failure"))
            })
            .is_err()
        );
        assert!(!failed.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        let raced = directory.path().join("raced.json");
        assert_eq!(
            publish(&raced, |writer| {
                writer.write_all(b"complete")?;
                std::fs::write(&raced, "competing writer")
            })
            .unwrap_err()
            .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&raced).unwrap(), b"competing writer");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        assert_eq!(log.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn file_export_preserves_symlink_destinations_and_targets() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        let destination = directory.path().join("link");
        std::os::unix::fs::symlink(&target, &destination).unwrap();
        assert_eq!(
            UndoLog::new(1)
                .export_review_json(&destination)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(
            std::fs::symlink_metadata(&destination)
                .unwrap()
                .is_symlink()
        );
        assert_eq!(std::fs::read(target).unwrap(), b"keep");
    }

    #[test]
    fn review_export_preserves_native_paths_and_original_operation_directions() {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(b"original-\xff\n".to_vec()))
        };
        #[cfg(windows)]
        let path = {
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&[111, 0xd800, 10]))
        };
        let current = PathBuf::from("current 日本");
        let operations = [
            UndoableOperation::FilesMoved {
                moves: vec![(path.clone(), current.clone())],
            },
            UndoableOperation::FilesCopied {
                created: vec![path.clone()],
            },
            UndoableOperation::FilesDeleted {
                paths: vec![path.clone()],
            },
            UndoableOperation::FileRenamed {
                path: current,
                old_path: path.clone(),
                new_name: "current 日本".into(),
            },
            UndoableOperation::FileCreated { path: path.clone() },
            UndoableOperation::DirectoryCreated { path: path.clone() },
        ];
        let mut log = UndoLog::new(10);
        log.record_create_file("ordinary-not-for-review".into());
        for (id, operation) in operations.into_iter().enumerate() {
            let mut entry = UndoEntry::new(id as u64 + 10, operation, "review\noriginal");
            entry.timestamp = std::time::UNIX_EPOCH;
            if id == 0 {
                entry.last_attempt = Some(crate::UndoAttempt {
                    observed_at: std::time::UNIX_EPOCH,
                    cancelled: true,
                    succeeded: 1,
                    failed: 2,
                    skipped: 3,
                    bytes_processed: 42,
                    errors: vec![crate::OperationError::new(
                        path.clone(),
                        "native error\ncontext",
                    )],
                });
            }
            log.retain_for_review(entry);
        }
        let mut bytes = Vec::new();
        log.write_review_json(&mut bytes).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["purpose"], "incomplete_undo_review_only");
        let entries = value["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 6);
        assert_eq!(log.len(), 7);
        assert_eq!(entries[0]["id"], 10);
        assert_eq!(entries[0]["description"], "review\noriginal");
        assert_eq!(entries[0]["last_attempt"]["cancelled"], true);
        assert_eq!(entries[0]["last_attempt"]["bytes_processed"], 42);
        assert_eq!(
            entries[0]["last_attempt"]["errors"][0]["message"],
            "native error\ncontext"
        );
        assert!(entries[1]["last_attempt"].is_null());
        let paths = [
            &entries[0]["operation"]["moves"][0]["original"],
            &entries[1]["operation"]["created"][0],
            &entries[2]["operation"]["paths"][0],
            &entries[3]["operation"]["original"],
            &entries[4]["operation"]["path"],
            &entries[5]["operation"]["path"],
            &entries[0]["last_attempt"]["errors"][0]["path"],
        ];
        for encoded in paths {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                assert_eq!(encoded["platform"], "unix");
                assert_eq!(
                    serde_json::from_value::<Vec<u8>>(encoded["bytes"].clone()).unwrap(),
                    path.as_os_str().as_bytes()
                );
            }
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                assert_eq!(encoded["platform"], "windows");
                assert_eq!(
                    serde_json::from_value::<Vec<u16>>(encoded["wide"].clone()).unwrap(),
                    path.as_os_str().encode_wide().collect::<Vec<_>>()
                );
            }
        }
        assert_eq!(
            entries[0]["operation"]["moves"][0]["current"]["display"],
            "current 日本"
        );
        assert_eq!(
            entries[3]["operation"]["current"]["display"],
            "current 日本"
        );
    }

    #[test]
    fn review_export_propagates_writer_failure_without_consuming_history() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected output failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut log = UndoLog::new(1);
        log.retain_for_review(UndoEntry::new(
            1,
            UndoableOperation::FileCreated {
                path: "keep".into(),
            },
            "keep",
        ));
        assert!(log.write_review_json(FailingWriter).unwrap_err().is_io());
        assert_eq!(log.len(), 1);
        assert!(log.peek().unwrap().review_required);
        let mut bytes = Vec::new();
        log.write_review_json(&mut bytes).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["entries"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
