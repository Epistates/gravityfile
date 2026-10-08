//! Read-only inspection of bulk-rename intents. Never an execution format.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{RecoveryDiscovery, RecoveryIssue};
use serde::{Deserialize, Serialize};

pub(crate) const PREFIX: &str = ".gravityfile-rename-";
const MAX_INTENT_BYTES: u64 = 1024 * 1024;

/// Recorded rename paths. Their current contents and item outcomes are unverified.
#[derive(Debug, Clone)]
pub struct RenameIntentRecord {
    /// Identity observed before mutation, absent in legacy records.
    pub object_identity: Option<crate::ObjectIdentity>,
    /// Historical progress only; None means legacy or invalid/unreadable events.
    /// Never use this field as proof of the current object location or identity.
    pub last_recorded_phase: Option<crate::RenamePhase>,
    /// Whether an exclusive transaction lock was held while reading this record.
    /// False identifies older intents without lock coordination.
    pub locked_during_inspection: bool,
    /// Directory retaining the intent, not necessarily an inactive transaction.
    pub transaction: PathBuf,
    /// Intent file inspected.
    pub intent: PathBuf,
    /// Original source path.
    pub source: PathBuf,
    /// Reserved staging payload path.
    pub staged: PathBuf,
    /// Intended destination path.
    pub destination: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "lowercase")]
pub(crate) enum NativePath {
    Unix { bytes: Vec<u8> },
    Windows { wide: Vec<u16> },
}

impl NativePath {
    pub(crate) fn encode(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix {
                bytes: path.as_os_str().as_bytes().to_vec(),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows {
                wide: path.as_os_str().encode_wide().collect(),
            }
        }
    }

    pub(crate) fn decode(self) -> io::Result<PathBuf> {
        match self {
            Self::Unix { bytes } => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStringExt;
                    if !bytes.contains(&0) {
                        return Ok(std::ffi::OsString::from_vec(bytes).into());
                    }
                }
                let _ = bytes;
            }
            Self::Windows { wide } => {
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStringExt;
                    if !wide.contains(&0) {
                        return Ok(std::ffi::OsString::from_wide(&wide).into());
                    }
                }
                let _ = wide;
            }
        }
        Err(invalid("invalid or foreign native path encoding"))
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn inspect(
    directory: &Path,
    path: &Path,
    locked: bool,
    budget: &mut crate::recovery::DiscoveryBudget,
) -> io::Result<(RenameIntentRecord, bool)> {
    #[derive(Deserialize)]
    struct Intent {
        schema_version: u32,
        #[serde(default)]
        journal_version: Option<u32>,
        #[serde(default)]
        lock_required: bool,
        purpose: String,
        source: NativePath,
        staged: NativePath,
        destination: NativePath,
        #[serde(default)]
        object_identity: Option<crate::ObjectIdentity>,
    }
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| {
            name.strip_prefix("item-").is_some_and(|index| {
                !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
        .ok_or_else(|| invalid("invalid rename intent filename"))?;
    let data = budget.read(path, MAX_INTENT_BYTES)?;
    let intent: Intent = serde_json::from_slice(&data)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if intent.schema_version != 1 || intent.purpose != "bulk_rename_inspection_only" {
        return Err(invalid("unsupported rename intent schema or purpose"));
    }
    if intent.lock_required && !locked {
        return Err(invalid("rename intent requires a missing transaction lock"));
    }
    if intent.journal_version.is_some_and(|version| version != 1)
        || (intent.journal_version.is_some() && !intent.lock_required)
    {
        return Err(invalid(
            "unsupported or uncoordinated rename progress journal",
        ));
    }
    let source = intent.source.decode()?;
    let staged = intent.staged.decode()?;
    let destination = intent.destination.decode()?;
    if !source.is_absolute() || !destination.is_absolute() || staged != directory.join(stem) {
        return Err(invalid(
            "rename intent paths do not match their staging scope",
        ));
    }
    Ok((
        RenameIntentRecord {
            object_identity: intent.object_identity,
            last_recorded_phase: None,
            locked_during_inspection: locked,
            transaction: directory.into(),
            intent: path.into(),
            source,
            staged,
            destination,
        },
        intent.journal_version.is_some(),
    ))
}

pub(crate) fn discover(
    directory: &Path,
    report: &mut RecoveryDiscovery,
    budget: &mut crate::recovery::DiscoveryBudget,
) {
    let result = (|| -> io::Result<()> {
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return Err(invalid("rename staging must be a directory, not a symlink"));
        }
        let directory = directory.canonicalize()?;
        let lock_path = directory.join("lock");
        let lock = match fs::symlink_metadata(&lock_path) {
            Ok(_) => Some(crate::recovery::RecoveryLock::new(
                crate::recovery::open_regular(&lock_path, true)?,
            )?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let mut paths = Vec::new();
        let mut events: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for entry in fs::read_dir(&directory)? {
            if !budget.entry() {
                report.issues.push(RecoveryIssue {
                    path: directory.clone(),
                    error: invalid("recovery discovery entry limit reached; inspection incomplete"),
                });
                break;
            }
            match entry {
                Ok(entry)
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json") =>
                {
                    paths.push(entry.path())
                }
                Ok(entry)
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "event") =>
                {
                    let path = entry.path();
                    let owner = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|name| name.split_once('.').map(|(owner, _)| owner));
                    if let Some(owner) = owner {
                        events.entry(owner.into()).or_default().push(path);
                    } else {
                        report.issues.push(RecoveryIssue {
                            path,
                            error: invalid("invalid rename event filename"),
                        });
                    }
                }
                Ok(_) => {}
                Err(error) => report.issues.push(RecoveryIssue {
                    path: directory.clone(),
                    error,
                }),
            }
        }
        paths.sort();
        if paths.is_empty() {
            return Err(invalid(
                "rename staging has no readable intent files; inspect manually",
            ));
        }
        for path in paths {
            if budget.exhausted() {
                return Err(invalid(
                    "recovery discovery byte limit reached; inspection incomplete",
                ));
            }
            match inspect(&directory, &path, lock.is_some(), budget) {
                Ok((mut record, journal)) => {
                    let stem = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .expect("validated intent name");
                    let mut event_paths = events.remove(stem).unwrap_or_default();
                    if journal {
                        match crate::rename_journal::inspect(
                            &record.staged,
                            &mut event_paths,
                            budget,
                        ) {
                            Ok(phase) => record.last_recorded_phase = Some(phase),
                            Err(error) => report.issues.push(RecoveryIssue {
                                path: path.clone(),
                                error,
                            }),
                        }
                    } else if !event_paths.is_empty() {
                        report.issues.push(RecoveryIssue {
                            path: path.clone(),
                            error: invalid("legacy intent has unexpected progress events"),
                        });
                    }
                    report.rename_intents.push(record);
                }
                Err(error) => report.issues.push(RecoveryIssue { path, error }),
            }
        }
        let mut orphan_events: Vec<_> = events.into_values().flatten().collect();
        orphan_events.sort();
        for path in orphan_events {
            report.issues.push(RecoveryIssue {
                path,
                error: invalid("rename event has no readable owning intent"),
            });
        }
        Ok(())
    })();
    if let Err(error) = result {
        report.issues.push(RecoveryIssue {
            path: directory.into(),
            error,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_path::NativePath as EncodedPath;

    #[test]
    fn progress_inspection_keeps_intents_when_event_chains_are_invalid() {
        use crate::rename_journal::{Event, RenamePhase, event_path};
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        // Construct a coordinated inspection fixture without mutating source data.
        let source = root.join("source");
        fs::write(&source, b"unchanged").unwrap();
        let transaction = root.join(".gravityfile-rename-progress");
        fs::create_dir(&transaction).unwrap();
        fs::File::create(transaction.join("lock")).unwrap();
        let payload = transaction.join("item-0");
        let intent = serde_json::json!({"schema_version":1,"journal_version":1,
            "lock_required":true,"purpose":"bulk_rename_inspection_only",
            "source":EncodedPath(&source),"staged":EncodedPath(&payload),
            "destination":EncodedPath(&root.join("destination"))});
        fs::write(
            transaction.join("item-0.json"),
            serde_json::to_vec(&intent).unwrap(),
        )
        .unwrap();
        let event = event_path(&payload, 0);
        let valid = serde_json::to_vec(&Event {
            schema_version: 1,
            sequence: 0,
            phase: RenamePhase::Staged,
        })
        .unwrap();
        for bytes in [
            b"partial {".to_vec(),
            serde_json::to_vec(&Event {
                schema_version: 99,
                sequence: 0,
                phase: RenamePhase::Staged,
            })
            .unwrap(),
            serde_json::to_vec(&Event {
                schema_version: 1,
                sequence: 0,
                phase: RenamePhase::Installed,
            })
            .unwrap(),
            vec![b' '; crate::rename_journal::MAX_EVENT_BYTES as usize + 1],
        ] {
            fs::write(&event, &bytes).unwrap();
            let report = crate::discover_recovery_report(&root).unwrap();
            assert_eq!(report.rename_intents.len(), 1);
            assert_eq!(report.issues.len(), 1);
            assert_eq!(report.rename_intents[0].last_recorded_phase, None);
            assert_eq!(fs::read(&source).unwrap(), b"unchanged");
            assert_eq!(fs::read(&event).unwrap(), bytes);
        }
        fs::write(&event, &valid).unwrap();
        let report = crate::discover_recovery_report(&root).unwrap();
        assert!(report.issues.is_empty());
        assert_eq!(
            report.rename_intents[0].last_recorded_phase,
            Some(RenamePhase::Staged)
        );
        // A missing prefix, even with a plausible later event, is incomplete.
        fs::rename(&event, event_path(&payload, 1)).unwrap();
        let report = crate::discover_recovery_report(&root).unwrap();
        assert_eq!(report.rename_intents.len(), 1);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.rename_intents[0].last_recorded_phase, None);
        fs::remove_file(event_path(&payload, 1)).unwrap();
        // Orphan records cannot be attached to the only known item.
        fs::write(transaction.join("item-99.00.event"), &valid).unwrap();
        let report = crate::discover_recovery_report(&root).unwrap();
        assert_eq!(
            report.rename_intents[0].last_recorded_phase,
            Some(RenamePhase::StagePending)
        );
        assert_eq!(report.issues.len(), 1);
    }

    #[test]
    fn discovery_preserves_valid_intents_beside_invalid_records_without_mutating_paths() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let staging = root.join(".gravityfile-rename-review");
        fs::create_dir(&staging).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, b"keep").unwrap();
        let valid = serde_json::json!({"schema_version":1,"purpose":"bulk_rename_inspection_only",
            "source": EncodedPath(&source), "staged": EncodedPath(&staging.join("item-0")),
            "destination": EncodedPath(&destination)});
        fs::write(
            staging.join("item-0.json"),
            serde_json::to_vec(&valid).unwrap(),
        )
        .unwrap();
        let bad_path = staging.join("item-1.json");
        for invalid_data in [
            b"not json".to_vec(),
            vec![b' '; MAX_INTENT_BYTES as usize + 1],
            serde_json::to_vec(&{
                let mut bad = valid.clone();
                bad["schema_version"] = 99.into();
                bad
            })
            .unwrap(),
            serde_json::to_vec(&valid).unwrap(), // item-0 payload cannot be claimed by item-1 intent
        ] {
            fs::write(&bad_path, &invalid_data).unwrap();
            let report = crate::discover_recovery_report(&root).unwrap();
            assert_eq!(report.rename_intents.len(), 1);
            assert_eq!(report.issues.len(), 1);
            assert_eq!(report.issues[0].path, bad_path);
            assert_eq!(report.rename_intents[0].source, source);
            assert_eq!(report.rename_intents[0].last_recorded_phase, None);
            assert_eq!(fs::read(&bad_path).unwrap(), invalid_data);
            assert_eq!(fs::read(&source).unwrap(), b"keep");
            assert!(!destination.exists());
            // The legacy API remains scoped to overwrite/copy-undo journals.
            assert!(crate::discover_recovery(&root).unwrap().is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn intent_inspection_rejects_symlinks_and_keeps_native_source_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let staging = root.join(".gravityfile-rename-native");
        fs::create_dir(&staging).unwrap();
        let source = root.join(std::ffi::OsString::from_vec(b"source-\xff".to_vec()));
        let payload = staging.join("item-0");
        let value = serde_json::json!({"schema_version":1,"purpose":"bulk_rename_inspection_only",
            "source": EncodedPath(&source), "staged": EncodedPath(&payload),
            "destination": EncodedPath(&root.join("destination"))});
        let intent = staging.join("item-0.json");
        fs::write(&intent, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            crate::discover_recovery_report(&root)
                .unwrap()
                .rename_intents[0]
                .source,
            source
        );
        let external = root.join("external.json");
        fs::rename(&intent, &external).unwrap();
        std::os::unix::fs::symlink(&external, &intent).unwrap();
        let report = crate::discover_recovery_report(&root).unwrap();
        assert!(report.rename_intents.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert!(fs::symlink_metadata(intent).unwrap().is_symlink());
        assert_eq!(
            fs::read(external).unwrap(),
            serde_json::to_vec(&value).unwrap()
        );
    }
}
