//! Coordinated native-path plans and explicit, resumable no-clobber restoration.
use crate::rename_review::NativePath;
use crate::{ObjectIdentity, ObjectKind};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) const PLAN: &str = "plan.batch";
const MAX_PLAN: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_ITEMS: usize = 10_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    source: NativePath,
    staged: NativePath,
    destination: NativePath,
    identity: ObjectIdentity,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Guard {
    path: NativePath,
    identity: ObjectIdentity,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    purpose: String,
    coordinator: NativePath,
    participants: Vec<NativePath>,
    guards: Vec<Guard>,
    items: Vec<Item>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn occupied(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Install immutable bytes, or on recovery verify an identical existing record.
fn persist(path: &Path, bytes: &[u8], resume: bool) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if resume && error.kind() == io::ErrorKind::AlreadyExists => {
            let mut budget = crate::recovery::DiscoveryBudget::default();
            if budget.read(path, MAX_PLAN)? != bytes {
                return Err(invalid(
                    "existing rename recovery record differs or is incomplete",
                ));
            }
            // An earlier sync may have failed; retry it before permitting a move.
            let file = crate::recovery::open_regular(path, false)?;
            file.sync_all()?;
            return crate::recovery::sync_directory(path.parent().expect("control parent"));
        }
        Err(error) => return Err(error),
    };
    file.write_all(bytes)?;
    file.sync_all()?;
    crate::recovery::sync_directory(path.parent().expect("control parent"))
}

pub(crate) fn prepare(
    items: &[(PathBuf, PathBuf, PathBuf)],
    identities: &HashMap<PathBuf, ObjectIdentity>,
) -> io::Result<Vec<PathBuf>> {
    if items.len() > MAX_ITEMS {
        return Err(invalid("bulk rename exceeds recoverable item limit"));
    }
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let unique: HashSet<_> = items
        .iter()
        .filter_map(|(_, staged, _)| identities.get(staged))
        .collect();
    if unique.len() != items.len() {
        return Err(invalid("bulk rename has ambiguous hard-link identities"));
    }
    let mut participants: Vec<_> = items
        .iter()
        .map(|(_, staged, _)| staged.parent().unwrap().to_path_buf())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    participants.sort();
    let mut parents: Vec<_> = items
        .iter()
        .flat_map(|(source, staged, destination)| {
            [
                source.parent().unwrap().to_path_buf(),
                staged.parent().unwrap().to_path_buf(),
                destination.parent().unwrap().to_path_buf(),
            ]
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    parents.sort();
    let plan = Plan {
        schema_version: 1,
        purpose: "bulk_rename_explicit_rollback".into(),
        coordinator: NativePath::encode(&participants[0]),
        participants: participants.iter().map(|p| NativePath::encode(p)).collect(),
        guards: parents
            .iter()
            .map(|path| {
                Ok(Guard {
                    path: NativePath::encode(path),
                    identity: ObjectIdentity::capture(path)?,
                })
            })
            .collect::<io::Result<_>>()?,
        items: items
            .iter()
            .map(|(source, staged, destination)| {
                Ok(Item {
                    source: NativePath::encode(source),
                    staged: NativePath::encode(staged),
                    destination: NativePath::encode(destination),
                    identity: identities
                        .get(staged)
                        .ok_or_else(|| invalid("missing batch identity"))?
                        .clone(),
                })
            })
            .collect::<io::Result<_>>()?,
    };
    let bytes = serde_json::to_vec(&plan).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_PLAN {
        return Err(invalid("bulk rename plan exceeds byte limit"));
    }
    let intent_bytes = items.iter().try_fold(0u64, |sum, (_, staged, _)| {
        Ok::<_, io::Error>(sum.saturating_add(fs::metadata(staged.with_extension("json"))?.len()))
    })?;
    if (bytes.len() as u64)
        .saturating_mul(participants.len() as u64 + 1)
        .saturating_add(intent_bytes)
        > 64 * 1024 * 1024
    {
        return Err(invalid(
            "batch control files exceed the recovery input budget",
        ));
    }
    let mut written = Vec::new();
    for participant in participants {
        let path = participant.join(PLAN);
        // Failure retains all existing plans; no source has moved yet.
        persist(&path, &bytes, false)?;
        written.push(path);
    }
    Ok(written)
}

pub(crate) fn commit(plans: &[PathBuf]) -> io::Result<Option<PathBuf>> {
    let Some(first) = plans.first() else {
        return Ok(None);
    };
    let path = first.with_file_name("committed.batch");
    persist(&path, b"gravityfile bulk rename committed v1\n", false)?;
    Ok(Some(path))
}

/// Result of explicit restoration. Journals remain available for inspection.
#[derive(Debug)]
pub struct RenameRecovery {
    /// Coordinator holding the plan and recovery records.
    pub coordinator: PathBuf,
    /// Number of items verified at their original sources after recovery.
    pub restored_items: usize,
}

struct DecodedItem {
    source: PathBuf,
    staged: PathBuf,
    destination: PathBuf,
    identity: ObjectIdentity,
}
struct OpenBatch {
    coordinator: PathBuf,
    guards: HashMap<PathBuf, ObjectIdentity>,
    items: Vec<DecodedItem>,
    _locks: Vec<crate::recovery::RecoveryLock>,
}

fn absolute(path: NativePath) -> io::Result<PathBuf> {
    let path = path.decode()?;
    if !path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(invalid("batch paths must be absolute and normalized"));
    }
    Ok(path)
}

fn open(transaction: &Path) -> io::Result<OpenBatch> {
    if !fs::symlink_metadata(transaction)?.is_dir()
        || fs::symlink_metadata(transaction)?.is_symlink()
    {
        return Err(invalid(
            "rename transaction must be a directory, not a symlink",
        ));
    }
    let transaction = transaction.canonicalize()?;
    let mut budget = crate::recovery::DiscoveryBudget::default();
    let bytes = budget.read(&transaction.join(PLAN), MAX_PLAN)?;
    let plan: Plan = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if plan.schema_version != 1
        || plan.purpose != "bulk_rename_explicit_rollback"
        || plan.items.len() > MAX_ITEMS
        || plan.items.is_empty()
    {
        return Err(invalid("unsupported or oversized bulk rename plan"));
    }
    let coordinator = absolute(plan.coordinator)?;
    let mut participants: Vec<_> = plan
        .participants
        .into_iter()
        .map(absolute)
        .collect::<io::Result<_>>()?;
    let original_count = participants.len();
    participants.sort();
    participants.dedup();
    if participants.len() != original_count
        || !participants.contains(&transaction)
        || participants.first() != Some(&coordinator)
        || participants.len() > MAX_ITEMS
    {
        return Err(invalid("invalid batch coordinator or participants"));
    }
    let mut locks = Vec::new();
    for directory in &participants {
        if !directory.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with(crate::rename_review::PREFIX)
        }) || !fs::symlink_metadata(directory)?.is_dir()
            || fs::symlink_metadata(directory)?.is_symlink()
        {
            return Err(invalid("invalid batch participant directory"));
        }
        locks.push(crate::recovery::RecoveryLock::new(
            crate::recovery::open_regular(&directory.join("lock"), true)?,
        )?);
        if budget.read(&directory.join(PLAN), MAX_PLAN)? != bytes {
            return Err(invalid("batch participant plans disagree"));
        }
    }
    let mut guards = HashMap::new();
    for guard in plan.guards {
        if guards
            .insert(absolute(guard.path)?, guard.identity)
            .is_some()
        {
            return Err(invalid("duplicate batch parent guard"));
        }
    }
    let items: Vec<_> = plan
        .items
        .into_iter()
        .map(|item| {
            Ok(DecodedItem {
                source: absolute(item.source)?,
                staged: absolute(item.staged)?,
                destination: absolute(item.destination)?,
                identity: item.identity,
            })
        })
        .collect::<io::Result<_>>()?;
    let mut sources = HashSet::new();
    let mut destinations = HashSet::new();
    let mut identities = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let parent = item.staged.parent().unwrap();
        if !participants.contains(&parent.to_path_buf())
            || item.staged != parent.join(format!("item-{index}"))
            || item.source.parent() != parent.parent()
            || !sources.insert(item.source.clone())
            || !destinations.insert(item.destination.clone())
            || !identities.insert(item.identity.clone())
        {
            return Err(invalid("ambiguous or inconsistent batch item mapping"));
        }
        for path in [&item.source, &item.staged, &item.destination] {
            if !guards.contains_key(path.parent().unwrap()) {
                return Err(invalid("batch is missing a parent identity guard"));
            }
        }
        if [&item.source, &item.destination]
            .iter()
            .any(|path| participants.iter().any(|parent| path.starts_with(parent)))
        {
            return Err(invalid(
                "batch data paths must not target transaction control directories",
            ));
        }
        // Bind every item to its separately recorded intent before recovery.
        let (intent, _) = crate::rename_review::inspect(
            parent,
            &item.staged.with_extension("json"),
            true,
            &mut budget,
        )?;
        if intent.source != item.source
            || intent.staged != item.staged
            || intent.destination != item.destination
            || intent.object_identity.as_ref() != Some(&item.identity)
        {
            return Err(invalid("batch and item intent disagree"));
        }
    }
    for source in &sources {
        if source
            .ancestors()
            .skip(1)
            .any(|ancestor| sources.contains(ancestor))
        {
            return Err(invalid("nested batch sources"));
        }
    }
    for item in &items {
        if item
            .destination
            .ancestors()
            .skip(1)
            .any(|ancestor| sources.contains(ancestor))
        {
            return Err(invalid("destination beneath batch source"));
        }
    }
    let batch = OpenBatch {
        coordinator,
        guards,
        items,
        _locks: locks,
    };
    batch.verify_guards()?;
    Ok(batch)
}

impl OpenBatch {
    fn verify_guards(&self) -> io::Result<()> {
        for (path, identity) in &self.guards {
            identity.verify(path)?;
            let kind = match identity {
                ObjectIdentity::Unix { kind, .. } | ObjectIdentity::Windows { kind, .. } => kind,
            };
            if *kind != ObjectKind::Directory {
                return Err(invalid("batch parent is not a directory"));
            }
        }
        Ok(())
    }

    fn move_item(
        &self,
        index: usize,
        from: &Path,
        to: &Path,
        label: &str,
        observer: &mut impl FnMut(usize, &str),
    ) -> io::Result<()> {
        self.verify_guards()?;
        let item = &self.items[index];
        item.identity.verify(from)?;
        let record = serde_json::to_vec(&Item {
            source: NativePath::encode(from),
            staged: NativePath::encode(to),
            destination: NativePath::encode(to),
            identity: item.identity.clone(),
        })
        .map_err(io::Error::other)?;
        persist(
            &self
                .coordinator
                .join(format!("recover-{index}-{label}.pending")),
            &record,
            true,
        )?;
        observer(index, &format!("{label}_pending"));
        item.identity.verify(from)?;
        crate::copy::rename_noreplace(from, to).map_err(io::Error::other)?;
        observer(index, &format!("{label}_moved"));
        item.identity.verify(to)?;
        let first = crate::recovery::sync_directory(from.parent().unwrap());
        let second = crate::recovery::sync_directory(to.parent().unwrap());
        first.and(second)?;
        persist(
            &self
                .coordinator
                .join(format!("recover-{index}-{label}.done")),
            &record,
            true,
        )
    }
}

/// Explicitly restore an interrupted batch without overwriting occupied paths.
/// All participants are locked and mappings/identities preflighted before moves.
/// Committed batches and ambiguous/missing objects are rejected. Journals remain.
/// Path checks do not exclude replacement between verification and rename.
pub fn restore_rename_batch(transaction: &Path) -> io::Result<RenameRecovery> {
    restore_observed(transaction, |_, _| {})
}

fn restore_observed(
    transaction: &Path,
    mut observer: impl FnMut(usize, &str),
) -> io::Result<RenameRecovery> {
    let batch = open(transaction)?;
    if occupied(&batch.coordinator.join("committed.batch"))? {
        return Err(invalid(
            "batch was committed; automatic rollback is refused",
        ));
    }
    let mut positions = Vec::new();
    for item in &batch.items {
        let mut matches = HashSet::new();
        for path in [&item.source, &item.staged, &item.destination] {
            match ObjectIdentity::capture(path) {
                Ok(identity) if identity == item.identity => {
                    matches.insert(path.clone());
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if matches.len() != 1 {
            return Err(invalid(
                "batch object missing or ambiguous; inspect manually",
            ));
        }
        positions.push(matches.into_iter().next().unwrap());
    }
    let moving: HashSet<_> = positions
        .iter()
        .zip(&batch.items)
        .filter(|(path, item)| **path != item.source)
        .map(|(path, _)| path.as_path())
        .collect();
    for (item, position) in batch.items.iter().zip(&positions) {
        if *position == item.source {
            continue;
        }
        if occupied(&item.source)? && !moving.contains(item.source.as_path()) {
            return Err(invalid(
                "original rename source is occupied by unrelated data",
            ));
        }
        if *position != item.staged && occupied(&item.staged)? {
            return Err(invalid("rename staging target is occupied"));
        }
    }
    let work = (|| {
        for (index, item) in batch.items.iter().enumerate() {
            if positions[index] != item.source && positions[index] != item.staged {
                batch.move_item(
                    index,
                    &positions[index],
                    &item.staged,
                    "restage",
                    &mut observer,
                )?;
                positions[index] = item.staged.clone();
                observer(index, "restaged");
            }
        }
        for (index, item) in batch.items.iter().enumerate() {
            if positions[index] != item.source {
                batch.move_item(index, &item.staged, &item.source, "restore", &mut observer)?;
                observer(index, "restored");
            }
            item.identity.verify(&item.source)?;
        }
        // A previous recovery may have died after rename but before sync.
        batch.verify_guards()?;
        for path in batch.guards.keys() {
            crate::recovery::sync_directory(path)?;
        }
        persist(
            &batch.coordinator.join("restored.batch"),
            b"gravityfile bulk rename restored v1\n",
            true,
        )?;
        Ok(RenameRecovery {
            coordinator: batch.coordinator.clone(),
            restored_items: batch.items.len(),
        })
    })();
    work.map_err(|error: io::Error| io::Error::new(error.kind(), format!("Bulk rename recovery may be partial: {error}; retained journals require inspection before retrying")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinated_recovery_restores_a_swap_across_source_parents() {
        let root = tempfile::tempdir().unwrap();
        for name in ["left", "right"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        fs::write(root.path().join("left/first"), b"first").unwrap();
        fs::write(root.path().join("right/second"), b"second").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rename::tests::rename_progress_crash_worker",
                "--nocapture",
            ])
            .env("GRAVITYFILE_RENAME_PROGRESS_ROOT", root.path())
            .env("GRAVITYFILE_RENAME_MULTI_PARENT", "1")
            .env("GRAVITYFILE_RENAME_PROGRESS_PHASE", "\"installed\"")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(79));
        let journal = fs::read_dir(root.path().join("right"))
            .unwrap()
            .map(Result::unwrap)
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(crate::rename_review::PREFIX)
            })
            .unwrap()
            .path();
        restore_rename_batch(&journal).unwrap();
        assert_eq!(fs::read(root.path().join("left/first")).unwrap(), b"first");
        assert_eq!(
            fs::read(root.path().join("right/second")).unwrap(),
            b"second"
        );
    }

    #[test]
    fn recovery_refuses_disagreeing_plans_and_committed_markers() {
        for committed in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let journal = interrupted(root.path(), crate::RenamePhase::Installed);
            if committed {
                fs::write(journal.join("committed.batch"), b"committed").unwrap();
            } else {
                let bytes = fs::read(journal.join(PLAN)).unwrap();
                let mut plan: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                plan["items"][0]["identity"] = plan["items"][1]["identity"].clone();
                fs::write(journal.join(PLAN), serde_json::to_vec(&plan).unwrap()).unwrap();
            }
            assert!(restore_rename_batch(&journal).is_err());
            assert_eq!(
                fs::read(root.path().join("second")).unwrap(),
                b"first payload"
            );
            assert_eq!(fs::read(journal.join("item-1")).unwrap(), b"second payload");
        }
    }

    fn interrupted(root: &Path, phase: crate::RenamePhase) -> PathBuf {
        fs::write(root.join("first"), b"first payload").unwrap();
        fs::write(root.join("second"), b"second payload").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rename::tests::rename_progress_crash_worker",
                "--nocapture",
            ])
            .env("GRAVITYFILE_RENAME_PROGRESS_ROOT", root)
            .env(
                "GRAVITYFILE_RENAME_PROGRESS_PHASE",
                serde_json::to_string(&phase).unwrap(),
            )
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(79));
        fs::read_dir(root)
            .unwrap()
            .map(Result::unwrap)
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(crate::rename_review::PREFIX)
            })
            .unwrap()
            .path()
    }

    #[test]
    fn interrupted_batches_restore_by_identity_and_second_call_is_idempotent() {
        for phase in [
            crate::RenamePhase::Staged,
            crate::RenamePhase::InstallPending,
            crate::RenamePhase::Installed,
        ] {
            let root = tempfile::tempdir().unwrap();
            let transaction = interrupted(root.path(), phase);
            for _ in 0..2 {
                let result = restore_rename_batch(&transaction).unwrap();
                assert_eq!(result.restored_items, 2);
                assert_eq!(
                    fs::read(root.path().join("first")).unwrap(),
                    b"first payload"
                );
                assert_eq!(
                    fs::read(root.path().join("second")).unwrap(),
                    b"second payload"
                );
                assert!(transaction.join(PLAN).exists());
            }
        }
    }

    #[test]
    fn recovery_refuses_replaced_payloads_occupied_sources_and_live_locks_before_moves() {
        for fault in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let transaction = interrupted(root.path(), crate::RenamePhase::Staged);
            let payload = transaction.join("item-0");
            let displaced = root.path().join("saved");
            let lock = match fault {
                0 => {
                    fs::rename(&payload, &displaced).unwrap();
                    fs::write(&payload, b"unrelated").unwrap();
                    None
                }
                1 => {
                    fs::write(root.path().join("first"), b"unrelated").unwrap();
                    None
                }
                _ => Some(
                    crate::recovery::RecoveryLock::new(
                        crate::recovery::open_regular(&transaction.join("lock"), true).unwrap(),
                    )
                    .unwrap(),
                ),
            };
            assert!(restore_rename_batch(&transaction).is_err());
            assert_eq!(
                fs::read(&payload).unwrap(),
                if fault == 0 {
                    &b"unrelated"[..]
                } else {
                    &b"first payload"[..]
                }
            );
            assert_eq!(
                fs::read(root.path().join("second")).unwrap(),
                b"second payload"
            );
            if fault == 0 {
                assert_eq!(fs::read(displaced).unwrap(), b"first payload");
            }
            if fault == 1 {
                assert_eq!(fs::read(root.path().join("first")).unwrap(), b"unrelated");
            }
            drop(lock);
        }
    }

    #[test]
    fn recovery_crash_worker() {
        let Some(transaction) = std::env::var_os("GRAVITYFILE_RENAME_RECOVERY_CRASH") else {
            return;
        };
        let phase = std::env::var("GRAVITYFILE_RENAME_RECOVERY_PHASE").unwrap();
        restore_observed(Path::new(&transaction), |_, current| {
            if current == phase {
                std::process::exit(81);
            }
        })
        .unwrap();
        panic!("recovery crash phase not reached");
    }

    #[test]
    fn recovery_resumes_after_exit_during_restaging_and_restoration() {
        for phase in [
            "restage_pending",
            "restage_moved",
            "restaged",
            "restore_pending",
            "restore_moved",
            "restored",
        ] {
            let root = tempfile::tempdir().unwrap();
            let transaction = interrupted(root.path(), crate::RenamePhase::Installed);
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "rename_batch::tests::recovery_crash_worker",
                    "--nocapture",
                ])
                .env("GRAVITYFILE_RENAME_RECOVERY_CRASH", &transaction)
                .env("GRAVITYFILE_RENAME_RECOVERY_PHASE", phase)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(81),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            restore_rename_batch(&transaction).unwrap();
            assert_eq!(
                fs::read(root.path().join("first")).unwrap(),
                b"first payload"
            );
            assert_eq!(
                fs::read(root.path().join("second")).unwrap(),
                b"second payload"
            );
        }
    }
}
