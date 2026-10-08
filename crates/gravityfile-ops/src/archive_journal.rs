//! Extraction publication journals. Recovery retains data rather than deleting it.
use crate::rename_review::NativePath;
use crate::{ObjectIdentity, RecoveryIssue};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) const PREFIX: &str = ".gravityfile-extraction-journal-";
const MAX_PLAN: u64 = 32 * 1024 * 1024;
const MAX_ITEMS: usize = 100_001;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    target: NativePath,
    source: NativePath,
    identity: ObjectIdentity,
    directory: bool,
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
    destination: NativePath,
    staging: NativePath,
    staging_identity: ObjectIdentity,
    guards: Vec<Guard>,
    items: Vec<Item>,
}
struct Entry {
    target: PathBuf,
    source: PathBuf,
    identity: ObjectIdentity,
    directory: bool,
}

/// Inactive extraction journal observation. Not an instruction to replay it.
#[derive(Debug)]
pub struct ArchiveRecoveryRecord {
    pub transaction: PathBuf,
    pub destination: PathBuf,
    pub staging: PathBuf,
    pub status: &'static str,
    pub entry_count: usize,
}

pub(crate) struct ArchiveJournal {
    pub(crate) path: PathBuf,
    destination: PathBuf,
    staging: PathBuf,
    entries: Vec<Entry>,
    lookup: HashMap<PathBuf, usize>,
    guards: HashMap<PathBuf, ObjectIdentity>,
    _lock: crate::recovery::RecoveryLock,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
fn control(path: &Path, bytes: &[u8], resume: bool) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        Err(e) if resume && e.kind() == io::ErrorKind::AlreadyExists => {
            let mut budget = crate::recovery::DiscoveryBudget::default();
            if budget.read(path, 128)? != bytes {
                return Err(invalid(
                    "archive recovery checkpoint differs or is incomplete",
                ));
            }
            crate::recovery::open_regular(path, false)?.sync_all()?;
        }
        Err(e) => return Err(e),
    }
    crate::recovery::sync_directory(path.parent().unwrap())
}

impl ArchiveJournal {
    pub(crate) fn prepare(
        staging: &Path,
        destination: &Path,
        directories: &[(PathBuf, PathBuf)],
        leaves: &[(PathBuf, PathBuf)],
    ) -> io::Result<Self> {
        if directories.len().saturating_add(leaves.len()) >= MAX_ITEMS {
            return Err(invalid("archive publication journal entry limit reached"));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("archive destination has no parent"))?;
        let temp = crate::staging::private_directory(parent, PREFIX)?;
        let path = temp.keep();
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = crate::recovery::RecoveryLock::new(options.open(path.join("lock"))?)?;
        let mut journal = Self {
            path,
            destination: destination.into(),
            staging: staging.into(),
            entries: Vec::new(),
            lookup: HashMap::new(),
            guards: HashMap::new(),
            _lock: lock,
        };
        journal
            .guards
            .insert(parent.into(), ObjectIdentity::capture(parent)?);
        let targets =
            std::iter::once(destination).chain(directories.iter().map(|(_, p)| p.as_path()));
        for target in targets {
            if exists(target)? {
                let metadata = fs::symlink_metadata(target)?;
                if !metadata.is_dir() || metadata.is_symlink() {
                    return Err(invalid(
                        "archive directory changed before journal preparation",
                    ));
                }
                journal
                    .guards
                    .insert(target.into(), ObjectIdentity::capture(target)?);
            } else {
                let source = journal
                    .path
                    .join(format!("directory-{}", journal.entries.len()));
                fs::create_dir(&source)?;
                let identity = ObjectIdentity::capture(&source)?;
                journal.guards.insert(target.into(), identity.clone());
                journal.entries.push(Entry {
                    target: target.into(),
                    source,
                    identity,
                    directory: true,
                });
            }
        }
        let mut sync_dirs = HashSet::from([
            staging.to_path_buf(),
            staging.parent().unwrap().to_path_buf(),
            journal.path.clone(),
        ]);
        sync_dirs.extend(directories.iter().map(|(source, _)| source.clone()));
        for (source, target) in leaves {
            let identity = ObjectIdentity::capture(source)?;
            if fs::symlink_metadata(source)?.is_file() {
                crate::recovery::open_regular(source, false)?.sync_all()?;
            }
            sync_dirs.insert(source.parent().unwrap().to_path_buf());
            journal.entries.push(Entry {
                target: target.clone(),
                source: source.clone(),
                identity,
                directory: false,
            });
        }
        for (index, item) in journal.entries.iter().enumerate() {
            journal.lookup.insert(item.target.clone(), index);
        }
        // Decoded directory entries and prepared directory objects precede intent.
        for directory in sync_dirs {
            crate::recovery::sync_directory(&directory)?;
        }
        let plan = Plan {
            schema_version: 1,
            purpose: "archive_extraction_retain_rollback".into(),
            destination: NativePath::encode(destination),
            staging: NativePath::encode(staging),
            staging_identity: ObjectIdentity::capture(staging)?,
            guards: journal
                .guards
                .iter()
                .map(|(p, id)| Guard {
                    path: NativePath::encode(p),
                    identity: id.clone(),
                })
                .collect(),
            items: journal
                .entries
                .iter()
                .map(|i| Item {
                    target: NativePath::encode(&i.target),
                    source: NativePath::encode(&i.source),
                    identity: i.identity.clone(),
                    directory: i.directory,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&plan).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_PLAN {
            return Err(invalid("archive publication journal byte limit reached"));
        }
        // Plan bytes can exceed checkpoint bounds; install once, never rewrite.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(journal.path.join("plan.archive"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        crate::recovery::sync_directory(&journal.path)?;
        crate::recovery::sync_directory(parent)?;
        Ok(journal)
    }

    pub(crate) fn before(&self, target: &Path) -> io::Result<Option<usize>> {
        if let Some(parent) = target
            .parent()
            .and_then(|p| self.guards.get(p).map(|id| (p, id)))
        {
            parent.1.verify(parent.0)?;
        }
        let Some(&index) = self.lookup.get(target) else {
            self.guards
                .get(target)
                .ok_or_else(|| invalid("unplanned archive directory"))?
                .verify(target)?;
            return Ok(None);
        };
        self.entries[index]
            .identity
            .verify(&self.entries[index].source)?;
        control(
            &self.path.join(format!("item-{index}.pending")),
            b"publication pending v1\n",
            false,
        )?;
        Ok(Some(index))
    }

    pub(crate) fn directory(&self, target: &Path) -> io::Result<()> {
        if let Some(&index) = self.lookup.get(target) {
            crate::copy::rename_noreplace(&self.entries[index].source, target)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }

    pub(crate) fn after(&self, index: usize) -> io::Result<()> {
        let item = &self.entries[index];
        item.identity.verify(&item.target)?;
        let first = crate::recovery::sync_directory(item.source.parent().unwrap());
        let second = crate::recovery::sync_directory(item.target.parent().unwrap());
        first.and(second)?;
        control(
            &self.path.join(format!("item-{index}.done")),
            b"publication completed v1\n",
            false,
        )
    }

    pub(crate) fn finish(&self, committed: bool) -> io::Result<()> {
        // Cover final directory modes and unlink/rollback directory entries.
        for (path, identity) in &self.guards {
            if exists(path)? {
                identity.verify(path)?;
                crate::recovery::sync_directory(path)?;
            }
        }
        control(
            &self.path.join(if committed {
                "committed.archive"
            } else {
                "rolled-back.archive"
            }),
            if committed {
                b"committed v1\n"
            } else {
                b"rolled back v1\n"
            },
            false,
        )
    }

    pub(crate) fn retain_entry(&self, target: &Path) -> io::Result<()> {
        let &index = self
            .lookup
            .get(target)
            .ok_or_else(|| invalid("unplanned extraction rollback path"))?;
        self.retain_one(index, &mut |_, _| {})
    }

    fn retain_one(&self, index: usize, observer: &mut impl FnMut(usize, &str)) -> io::Result<()> {
        let item = &self.entries[index];
        let retained = self.path.join(format!("retained-{index}"));
        if !exists(&item.target)? {
            if exists(&retained)? {
                return item.identity.verify(&retained);
            }
            return Err(invalid(
                "published extraction object is missing; rollback outcome unknown",
            ));
        }
        item.identity.verify(&item.target)?;
        self.guards
            .get(item.target.parent().unwrap())
            .unwrap()
            .verify(item.target.parent().unwrap())?;
        if item.directory && fs::read_dir(&item.target)?.next().is_some() {
            return Err(invalid("extracted directory is no longer empty"));
        }
        control(
            &self.path.join(format!("retain-{index}.pending")),
            b"retention pending v1\n",
            true,
        )?;
        observer(index, "pending");
        item.identity.verify(&item.target)?;
        crate::copy::rename_noreplace(&item.target, &retained).map_err(io::Error::other)?;
        observer(index, "moved");
        item.identity.verify(&retained)?;
        if fs::symlink_metadata(&retained)?.is_file() {
            crate::recovery::open_regular(&retained, false)?.sync_all()?;
        }
        let first = crate::recovery::sync_directory(item.target.parent().unwrap());
        let second = crate::recovery::sync_directory(&self.path);
        first.and(second)?;
        control(
            &self.path.join(format!("retain-{index}.done")),
            b"retention completed v1\n",
            true,
        )?;
        observer(index, "done");
        Ok(())
    }

    pub(crate) fn clean_committed(self) {
        for (index, entry) in self.entries.iter().enumerate() {
            for suffix in ["pending", "done"] {
                let _ = fs::remove_file(self.path.join(format!("item-{index}.{suffix}")));
            }
            if entry.directory {
                let _ = fs::remove_dir(&entry.source);
            }
        }
        // Only known control files and empty directories are removed.
        let _ = fs::remove_file(self.path.join("plan.archive"));
        let _ = fs::remove_file(self.path.join("committed.archive"));
        drop(self._lock);
        let _ = fs::remove_file(self.path.join("lock"));
        if let Err(error) = fs::remove_dir(&self.path) {
            tracing::warn!(path=%self.path.display(), %error, "Committed extraction journal retained");
        }
    }
}

fn open(path: &Path, budget: &mut crate::recovery::DiscoveryBudget) -> io::Result<ArchiveJournal> {
    if !fs::symlink_metadata(path)?.is_dir() || fs::symlink_metadata(path)?.is_symlink() {
        return Err(invalid("archive transaction is not a plain directory"));
    }
    let path = path.canonicalize()?;
    let lock = crate::recovery::RecoveryLock::new(crate::recovery::open_regular(
        &path.join("lock"),
        true,
    )?)?;
    let plan: Plan = serde_json::from_slice(&budget.read(&path.join("plan.archive"), MAX_PLAN)?)
        .map_err(|e| invalid(&e.to_string()))?;
    if plan.schema_version != 1
        || plan.purpose != "archive_extraction_retain_rollback"
        || plan.items.len() > MAX_ITEMS
    {
        return Err(invalid("unsupported archive recovery plan"));
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
            return Err(invalid("invalid archive recovery path"));
        }
        Ok(path)
    }
    let destination = absolute(plan.destination)?;
    let staging = absolute(plan.staging)?;
    if staging.parent() != path.parent()
        || !staging
            .file_name()
            .is_some_and(|s| s.to_string_lossy().starts_with(".gravityfile-extract-"))
        || destination.parent() != path.parent()
        || destination == staging
        || destination == path
    {
        return Err(invalid(
            "archive recovery paths escape sibling staging scope",
        ));
    }
    // A staging root may be gone after a completed in-process rollback. Never
    // follow a replacement root when it remains present.
    if exists(&staging)? {
        plan.staging_identity.verify(&staging)?;
    }
    let mut entries = Vec::new();
    let mut lookup = HashMap::new();
    for (index, item) in plan.items.into_iter().enumerate() {
        let target = absolute(item.target)?;
        let source = absolute(item.source)?;
        if !target.starts_with(&destination)
            || (!item.directory && target == destination)
            || (item.directory && source != path.join(format!("directory-{index}")))
            || (!item.directory && (!source.starts_with(&staging) || source == staging))
            || lookup.insert(target.clone(), index).is_some()
        {
            return Err(invalid("inconsistent archive recovery item mapping"));
        }
        entries.push(Entry {
            target,
            source,
            identity: item.identity,
            directory: item.directory,
        });
    }
    let mut guards = HashMap::new();
    for guard in plan.guards {
        if guards
            .insert(absolute(guard.path)?, guard.identity)
            .is_some()
        {
            return Err(invalid("duplicate archive parent guard"));
        }
    }
    for entry in &entries {
        if !guards.contains_key(entry.target.parent().unwrap()) {
            return Err(invalid("missing archive parent guard"));
        }
    }
    Ok(ArchiveJournal {
        path,
        destination,
        staging,
        entries,
        lookup,
        guards,
        _lock: lock,
    })
}

fn status(
    journal: &ArchiveJournal,
    budget: &mut crate::recovery::DiscoveryBudget,
) -> io::Result<&'static str> {
    for (file, bytes, state) in [
        ("committed.archive", &b"committed v1\n"[..], "committed"),
        (
            "rolled-back.archive",
            &b"rolled back v1\n"[..],
            "rolled_back",
        ),
        ("retained.archive", &b"retained v1\n"[..], "retained"),
    ] {
        if exists(&journal.path.join(file))? {
            if budget.read(&journal.path.join(file), 128)? != bytes {
                return Err(invalid("invalid archive outcome marker"));
            }
            return Ok(state);
        }
    }
    Ok("interrupted_or_prepared")
}

pub(crate) fn discover(
    path: &Path,
    report: &mut crate::RecoveryDiscovery,
    budget: &mut crate::recovery::DiscoveryBudget,
) {
    let result = (|| {
        let journal = open(path, budget)?;
        let state = status(&journal, budget)?;
        Ok(ArchiveRecoveryRecord {
            transaction: journal.path.clone(),
            destination: journal.destination.clone(),
            staging: journal.staging.clone(),
            status: state,
            entry_count: journal.entries.len(),
        })
    })();
    match result {
        Ok(record) => report.archive_records.push(record),
        Err(error) => report.issues.push(RecoveryIssue {
            path: path.into(),
            error,
        }),
    }
}

/// Move extraction output into its journal, preserving all bytes and preexisting directories.
/// Committed output, replacements, new directory children and active journals are refused.
/// Directory/identity checks are not atomic with the subsequent no-clobber rename.
pub fn retain_archive_extraction(transaction: &Path) -> io::Result<PathBuf> {
    retain_observed(transaction, |_, _| {})
}

fn retain_observed(
    transaction: &Path,
    mut observer: impl FnMut(usize, &str),
) -> io::Result<PathBuf> {
    let mut budget = crate::recovery::DiscoveryBudget::default();
    let journal = open(transaction, &mut budget)?;
    let state = status(&journal, &mut budget)?;
    if state == "committed" {
        return Err(invalid(
            "archive publication was committed; retain rollback is refused",
        ));
    }
    if state == "rolled_back" {
        return Ok(journal.path.clone());
    }
    let mut present = Vec::new();
    for (index, item) in journal.entries.iter().enumerate() {
        let retained = journal.path.join(format!("retained-{index}"));
        if exists(&item.target)? {
            item.identity.verify(&item.target)?;
            journal
                .guards
                .get(item.target.parent().unwrap())
                .unwrap()
                .verify(item.target.parent().unwrap())?;
            if exists(&retained)? {
                return Err(invalid("archive retained target is occupied"));
            }
            if item.directory {
                for child in fs::read_dir(&item.target)? {
                    let child = child?.path();
                    let owned = journal.lookup.get(&child).ok_or_else(|| {
                        invalid("extracted directory contains unrelated children")
                    })?;
                    journal.entries[*owned].identity.verify(&child)?;
                }
            }
            present.push(index);
        } else if exists(&retained)? {
            item.identity.verify(&retained)?;
        } else if exists(&item.source)? {
            item.identity.verify(&item.source)?;
        } else {
            return Err(invalid(
                "archive object is missing or ambiguous; inspect manually",
            ));
        }
    }
    let work = (|| {
        for index in present.into_iter().rev() {
            journal.retain_one(index, &mut observer)?;
        }
        // Retry may find a move completed before its sync/checkpoint. Re-sync
        // retained objects and all remaining guarded directories before outcome.
        for (index, item) in journal.entries.iter().enumerate() {
            let retained = journal.path.join(format!("retained-{index}"));
            if exists(&retained)? {
                item.identity.verify(&retained)?;
                if fs::symlink_metadata(&retained)?.is_file() {
                    crate::recovery::open_regular(&retained, false)?.sync_all()?;
                }
            }
        }
        for (path, identity) in &journal.guards {
            if exists(path)? {
                identity.verify(path)?;
                crate::recovery::sync_directory(path)?;
            }
        }
        crate::recovery::sync_directory(&journal.path)?;
        control(
            &journal.path.join("retained.archive"),
            b"retained v1\n",
            true,
        )?;
        Ok(journal.path.clone())
    })();
    work.map_err(|error: io::Error| io::Error::new(error.kind(), format!("Extraction retention may be partial: {error}; data and journal preserved, inspect before retrying")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn extraction_retention_moves_the_symlink_object_and_preserves_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let staging = crate::staging::private_directory(&root, ".gravityfile-extract-")
            .unwrap()
            .keep();
        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, b"unrelated target").unwrap();
        let source = staging.join("link");
        let target = destination.join("link");
        std::os::unix::fs::symlink(&outside, &source).unwrap();
        let journal = ArchiveJournal::prepare(
            &staging,
            &destination,
            &[],
            &[(source.clone(), target.clone())],
        )
        .unwrap();
        let index = journal.before(&target).unwrap().unwrap();
        crate::copy::rename_noreplace(&source, &target).unwrap();
        journal.after(index).unwrap();
        let path = journal.path.clone();
        drop(journal);
        retain_archive_extraction(&path).unwrap();
        assert!(
            fs::symlink_metadata(path.join("retained-0"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read_link(path.join("retained-0")).unwrap(), outside);
        assert_eq!(fs::read(root.join("outside")).unwrap(), b"unrelated target");
    }

    fn published() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let staging = crate::staging::private_directory(&root, ".gravityfile-extract-")
            .unwrap()
            .keep();
        fs::create_dir(staging.join("nested")).unwrap();
        fs::write(staging.join("nested/file"), b"payload").unwrap();
        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("unrelated"), b"existing").unwrap();
        let nested = destination.join("nested");
        let leaf = nested.join("file");
        let journal = ArchiveJournal::prepare(
            &staging,
            &destination,
            &[(staging.join("nested"), nested.clone())],
            &[(staging.join("nested/file"), leaf.clone())],
        )
        .unwrap();
        let index = journal.before(&nested).unwrap().unwrap();
        journal.directory(&nested).unwrap();
        journal.after(index).unwrap();
        let index = journal.before(&leaf).unwrap().unwrap();
        fs::hard_link(staging.join("nested/file"), &leaf).unwrap();
        journal.after(index).unwrap();
        let path = journal.path.clone();
        drop(journal);
        (temp, path, destination)
    }

    #[test]
    fn extraction_retention_refuses_replacements_new_children_active_locks_and_commit() {
        for fault in 0..4 {
            let (_root, journal, destination) = published();
            let leaf = destination.join("nested/file");
            let lock = match fault {
                0 => {
                    fs::rename(&leaf, destination.join("saved")).unwrap();
                    fs::write(&leaf, b"replacement").unwrap();
                    None
                }
                1 => {
                    fs::write(destination.join("nested/new"), b"unrelated child").unwrap();
                    None
                }
                2 => Some(
                    crate::recovery::RecoveryLock::new(
                        crate::recovery::open_regular(&journal.join("lock"), true).unwrap(),
                    )
                    .unwrap(),
                ),
                _ => {
                    fs::write(journal.join("committed.archive"), b"committed v1\n").unwrap();
                    None
                }
            };
            assert!(retain_archive_extraction(&journal).is_err());
            assert_eq!(
                fs::read(&leaf).unwrap(),
                if fault == 0 {
                    &b"replacement"[..]
                } else {
                    &b"payload"[..]
                }
            );
            assert_eq!(
                fs::read(destination.join("unrelated")).unwrap(),
                b"existing"
            );
            if fault == 0 {
                assert_eq!(fs::read(destination.join("saved")).unwrap(), b"payload");
            }
            if fault == 1 {
                assert_eq!(
                    fs::read(destination.join("nested/new")).unwrap(),
                    b"unrelated child"
                );
            }
            drop(lock);
        }
    }

    #[test]
    fn archive_retention_crash_worker() {
        let Some(journal) = std::env::var_os("GRAVITYFILE_ARCHIVE_RETENTION_CRASH") else {
            return;
        };
        let phase = std::env::var("GRAVITYFILE_ARCHIVE_RETENTION_PHASE").unwrap();
        retain_observed(Path::new(&journal), |_, current| {
            if current == phase {
                std::process::exit(85);
            }
        })
        .unwrap();
        panic!("retention crash phase not reached");
    }

    #[test]
    fn extraction_retention_resumes_after_pending_and_completed_checkpoints() {
        for phase in ["pending", "moved", "done"] {
            let (_root, journal, destination) = published();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "archive_journal::tests::archive_retention_crash_worker",
                    "--nocapture",
                ])
                .env("GRAVITYFILE_ARCHIVE_RETENTION_CRASH", &journal)
                .env("GRAVITYFILE_ARCHIVE_RETENTION_PHASE", phase)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(85));
            for _ in 0..2 {
                retain_archive_extraction(&journal).unwrap();
            }
            assert_eq!(fs::read(journal.join("retained-1")).unwrap(), b"payload");
            assert!(!destination.join("nested").exists());
            assert_eq!(
                fs::read(destination.join("unrelated")).unwrap(),
                b"existing"
            );
        }
    }

    #[test]
    fn extraction_retention_preserves_edits_to_the_owned_file() {
        let (_root, journal, destination) = published();
        fs::write(destination.join("nested/file"), b"later edits").unwrap();
        retain_archive_extraction(&journal).unwrap();
        assert_eq!(
            fs::read(journal.join("retained-1")).unwrap(),
            b"later edits"
        );
    }
}
