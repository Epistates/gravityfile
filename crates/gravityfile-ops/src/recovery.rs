//! Persistent overwrite and copy-undo records with explicit, exclusive recovery.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::copy::{remove_path, rename_noreplace};

const PREFIX: &str = ".gravityfile-recovery-";
const MANIFEST: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Shared by parent enumeration, rename enumeration, and control-file reads.
pub(crate) struct DiscoveryBudget {
    entries: usize,
    bytes: u64,
}

impl Default for DiscoveryBudget {
    fn default() -> Self {
        Self {
            entries: 10_000,
            bytes: 64 * 1024 * 1024,
        }
    }
}

impl DiscoveryBudget {
    pub(crate) fn entry(&mut self) -> bool {
        if self.entries == 0 {
            return false;
        }
        self.entries -= 1;
        true
    }

    pub(crate) fn read(&mut self, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        if self.bytes == 0 {
            return Err(invalid(
                "recovery discovery byte limit reached; inspection incomplete",
            ));
        }
        let mut data = Vec::new();
        let result = open_regular(path, false)?
            .take(limit.min(self.bytes) + 1)
            .read_to_end(&mut data);
        let exceeded = data.len() as u64 > self.bytes;
        self.bytes = self.bytes.saturating_sub(data.len() as u64);
        result?;
        if exceeded {
            return Err(invalid(
                "recovery discovery byte limit reached; inspection incomplete",
            ));
        }
        if data.len() as u64 > limit {
            return Err(invalid("journal control file exceeds size limit"));
        }
        Ok(data)
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.bytes == 0
    }
}

/// On-disk path encoding preserves non-UTF-8 Unix names and Windows UTF-16.
#[derive(Debug, Serialize, Deserialize)]
enum NativePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl NativePath {
    fn encode(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }

    fn decode(&self) -> io::Result<PathBuf> {
        match self {
            #[cfg(unix)]
            Self::Unix(bytes) if !bytes.contains(&0) => {
                use std::os::unix::ffi::OsStringExt;
                Ok(OsString::from_vec(bytes.clone()).into())
            }
            #[cfg(windows)]
            Self::Windows(wide) if !wide.contains(&0) => {
                use std::os::windows::ffi::OsStringExt;
                Ok(OsString::from_wide(wide).into())
            }
            _ => Err(invalid("unsupported or invalid journal path encoding")),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    #[serde(default)]
    purpose: RecoveryPurpose,
    destination_name: NativePath,
    source: Option<NativePath>,
    published: bool,
}

/// Why data was retained in a recovery journal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPurpose {
    /// An overwrite transaction, including older manifests without a purpose.
    #[default]
    Overwrite,
    /// A copied destination retained instead of permanently deleting it on undo.
    CopyUndo,
}

/// A retained transaction and the paths available for manual recovery.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryRecord {
    /// Whether this journal belongs to an overwrite or a copy undo.
    pub purpose: RecoveryPurpose,
    /// Private transaction directory. Retained until recovery is reviewed.
    pub transaction: PathBuf,
    /// Original destination, constrained to the transaction's parent directory.
    pub destination: PathBuf,
    /// Original source for a move. Informational; recovery never writes here.
    pub move_source: Option<PathBuf>,
    /// Whether publication was recorded before interruption.
    pub published: bool,
    /// Whether a saved original still exists (including a symlink).
    pub original_available: bool,
    /// Whether an unpublished payload still exists.
    pub payload_available: bool,
    /// Whether a path currently occupies the destination.
    pub destination_occupied: bool,
}

/// Holds the lock for a live overwrite. Drop releases only the lock, never data.
pub(crate) struct OverwriteJournal {
    directory: PathBuf,
    destination: PathBuf,
    manifest: Manifest,
    lock: RecoveryLock,
}

/// Explicit unlocking prevents a duplicated/inherited descriptor from extending
/// the lock beyond this owner's lifetime (for example during another thread's spawn).
pub(crate) struct RecoveryLock(File);

impl RecoveryLock {
    pub(crate) fn new(file: File) -> io::Result<Self> {
        acquire(&file)?;
        Ok(Self(file))
    }
}

impl Drop for RecoveryLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "Failed to explicitly release recovery lock");
        }
    }
}

impl OverwriteJournal {
    pub(crate) fn begin(destination: &Path, move_source: Option<&Path>) -> io::Result<Self> {
        Self::begin_for(destination, move_source, RecoveryPurpose::Overwrite)
    }

    fn begin_for(
        destination: &Path,
        move_source: Option<&Path>,
        purpose: RecoveryPurpose,
    ) -> io::Result<Self> {
        let name = destination
            .file_name()
            .ok_or_else(|| invalid("destination needs a filename"))?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()?;
        let temporary = crate::staging::private_directory(&parent, PREFIX)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(temporary.path().join("lock"))?;
        acquire(&lock)?;
        let lock = RecoveryLock(lock);
        let source = move_source.map(std::path::absolute).transpose()?;
        let manifest = Manifest {
            version: 1,
            purpose,
            destination_name: NativePath::encode(Path::new(name)),
            source: source.as_deref().map(NativePath::encode),
            published: false,
        };
        write_manifest(temporary.path(), &manifest)?;
        sync_directory(&parent)?;
        Ok(Self {
            directory: temporary.keep(),
            destination: parent.join(name),
            manifest,
            lock,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn payload(&self) -> PathBuf {
        self.directory.join("payload")
    }
    fn original(&self) -> PathBuf {
        self.directory.join("original")
    }

    /// Save the old destination, install the payload, and persist publication.
    /// Failed rollback retains both the journal and every remaining data path.
    pub(crate) fn publish(&mut self) -> Result<(), String> {
        self.publish_with_sync(Self::sync)
    }

    fn publish_with_sync(
        &mut self,
        mut sync: impl FnMut(&Self) -> io::Result<()>,
    ) -> Result<(), String> {
        rename_noreplace(&self.destination, &self.original()).map_err(|e| self.error(e))?;
        sync(self).map_err(|e| self.error(e))?;
        if let Err(error) = rename_noreplace(&self.payload(), &self.destination) {
            let rollback = rename_noreplace(&self.original(), &self.destination);
            let rollback_sync = sync(self);
            let durability = match rollback_sync {
                Ok(()) => "rollback directory synchronization succeeded".to_owned(),
                Err(error) => format!(
                    "rollback directory synchronization failed: {error}; durability is uncertain; inspect the destination before retrying"
                ),
            };
            return Err(self.error(format!(
                "publication failed: {error}; original rollback: {rollback:?}; {durability}"
            )));
        }
        sync(self).map_err(|e| self.error(e))?;
        self.manifest.published = true;
        write_manifest(&self.directory, &self.manifest).map_err(|e| self.error(e))?;
        Ok(())
    }

    pub(crate) fn error(&self, error: impl std::fmt::Display) -> String {
        format!(
            "{error}; recovery data retained at '{}'",
            self.directory.display()
        )
    }

    fn sync(&self) -> io::Result<()> {
        self.sync_with(sync_directory)
    }

    fn sync_with(&self, mut sync: impl FnMut(&Path) -> io::Result<()>) -> io::Result<()> {
        let parent = self
            .directory
            .parent()
            .ok_or_else(|| invalid("journal has no parent"))?;
        let mut failures = Vec::new();
        // Both entries changed during publication/rollback. A failure persisting
        // one must not prevent best-effort persistence of the other.
        for path in [self.directory.as_path(), parent] {
            if let Err(error) = sync(path) {
                failures.push((path, error));
            }
        }
        if let Some((_, first)) = failures.first() {
            let message = failures
                .iter()
                .map(|(path, error)| {
                    format!(
                        "directory synchronization failed for '{}': {error}",
                        path.display()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            Err(io::Error::new(first.kind(), message))
        } else {
            Ok(())
        }
    }

    /// Successful publication makes the old backup disposable. Failed cleanup
    /// leaves a discoverable record; it is never performed from Drop.
    pub(crate) fn finish(self) -> io::Result<()> {
        if self.manifest.purpose != RecoveryPurpose::Overwrite
            || !self.manifest.published
            || path_exists(&self.payload())?
        {
            return Err(invalid("cannot clean up an unpublished overwrite"));
        }
        remove_path(&self.original())?;
        let directory = self.directory;
        drop(self.lock);
        fs::remove_dir_all(&directory)?;
        sync_directory(
            directory
                .parent()
                .ok_or_else(|| invalid("journal has no parent"))?,
        )
    }
}

/// Remove a copied destination from its visible location without deleting data.
/// The journal is durable before the rename; neither failure nor Drop cleans it
/// up. This retains later edits and directory children, but does not prove that
/// the current path still identifies the originally copied object.
pub(crate) fn retain_copy_undo(path: &Path) -> Result<(), String> {
    fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    let journal = OverwriteJournal::begin_for(path, None, RecoveryPurpose::CopyUndo)
        .map_err(|error| error.to_string())?;
    rename_noreplace(&journal.destination, &journal.original())
        .map_err(|error| journal.error(error))?;
    journal.sync().map_err(|error| journal.error(error))?;
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn acquire(file: &File) -> io::Result<()> {
    file.try_lock().map_err(lock_error)
}

fn lock_error(error: fs::TryLockError) -> io::Error {
    match error {
        fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "recovery record is active; retry inspection after its owner releases the lock",
        ),
        // Preserve the original kind, OS code and source for diagnosis/reporting.
        fs::TryLockError::Error(error) => error,
    }
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn write_manifest(directory: &Path, manifest: &Manifest) -> io::Result<()> {
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut staged, manifest).map_err(io::Error::other)?;
    staged.flush()?;
    staged.as_file().sync_all()?;
    staged
        .persist(directory.join(MANIFEST))
        .map_err(|error| error.error)?;
    sync_directory(directory)
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Require a directory at open time. A metadata probe followed by a
        // plain open can follow a replacement symlink or block on a FIFO.
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
            .sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn open_regular(path: &Path, write: bool) -> io::Result<File> {
    let before = fs::symlink_metadata(path)?;
    open_regular_checked(path, write, &before)
}

fn open_regular_checked(path: &Path, write: bool, before: &fs::Metadata) -> io::Result<File> {
    if !before.is_file() {
        return Err(invalid("journal control file is not regular"));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(invalid("journal control file changed type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != opened.dev() || before.ino() != opened.ino() {
            return Err(invalid("journal control file was replaced while opening"));
        }
    }
    Ok(file)
}

fn open_record(transaction: &Path) -> io::Result<(RecoveryRecord, RecoveryLock)> {
    open_record_with_budget(transaction, &mut DiscoveryBudget::default())
}

fn open_record_with_budget(
    transaction: &Path,
    budget: &mut DiscoveryBudget,
) -> io::Result<(RecoveryRecord, RecoveryLock)> {
    let metadata = fs::symlink_metadata(transaction)?;
    if !metadata.is_dir() || metadata.is_symlink() {
        return Err(invalid("recovery path must be a directory, not a symlink"));
    }
    let transaction = transaction.canonicalize()?;
    let lock = open_regular(&transaction.join("lock"), true)?;
    acquire(&lock)?;
    let lock = RecoveryLock(lock);
    let content = budget.read(&transaction.join(MANIFEST), MAX_MANIFEST_BYTES)?;
    let manifest: Manifest = serde_json::from_slice(&content).map_err(invalid_json)?;
    if manifest.version != 1 {
        return Err(invalid("unsupported recovery manifest version"));
    }
    let name = manifest.destination_name.decode()?;
    let mut components = name.components();
    if !matches!(components.next(), Some(Component::Normal(part)) if part == name.as_os_str())
        || components.next().is_some()
    {
        return Err(invalid("journal destination must be a single filename"));
    }
    let destination = transaction
        .parent()
        .ok_or_else(|| invalid("journal has no parent"))?
        .join(name);
    let record = RecoveryRecord {
        purpose: manifest.purpose,
        move_source: manifest
            .source
            .as_ref()
            .map(NativePath::decode)
            .transpose()?,
        published: manifest.published,
        original_available: path_exists(&transaction.join("original"))?,
        payload_available: path_exists(&transaction.join("payload"))?,
        destination_occupied: path_exists(&destination)?,
        transaction,
        destination,
    };
    Ok((record, lock))
}

fn invalid_json(error: serde_json::Error) -> io::Error {
    invalid(format!("invalid recovery manifest: {error}"))
}

/// Inspect an inactive journal without changing its data. Active journals fail.
pub fn inspect_recovery(transaction: &Path) -> io::Result<RecoveryRecord> {
    open_record(transaction).map(|(record, _lock)| record)
}

/// A path that could not be inspected during recovery discovery.
#[derive(Debug)]
pub struct RecoveryIssue {
    /// Journal path, or the parent when a directory entry could not be read.
    pub path: PathBuf,
    /// The original inspection or enumeration error.
    pub error: io::Error,
}

/// Valid journals and inspection failures from a nonrecursive discovery pass.
/// Absence is established only when both record collections and `issues` are empty.
#[derive(Debug, Default)]
pub struct RecoveryDiscovery {
    /// Inactive extraction journals and historical publication status.
    pub archive_records: Vec<crate::ArchiveRecoveryRecord>,
    /// Inactive, valid records ordered by transaction path.
    pub records: Vec<RecoveryRecord>,
    /// Read-only bulk-rename intent observations; outcomes remain unverified.
    pub rename_intents: Vec<crate::RenameIntentRecord>,
    /// Active, malformed, inaccessible, or unreadable entries, ordered by path.
    pub issues: Vec<RecoveryIssue>,
}

impl RecoveryDiscovery {
    /// Stream a versioned, inspection-only JSON report, preserving native paths.
    /// `parent` identifies the supplied discovery scope. A complete status means
    /// no discovery issues were observed, not that filesystem recovery is safe
    /// without revalidation. This does not flush the caller's writer.
    pub fn write_json(&self, parent: &Path, writer: impl Write) -> Result<(), serde_json::Error> {
        crate::recovery_report::write(self, parent, writer)
    }
}

/// Discover all accessible journals without discarding valid records when a
/// neighbor fails inspection. Opening the parent can fail outright; subsequent
/// enumeration and inspection failures are retained in `issues`. This is a
/// filesystem observation, not a snapshot; records may change after inspection.
/// A pass examines at most 10,000 directory entries across its scope and reads
/// at most 64 MiB of control data (plus one overflow-detection byte). Reaching a
/// limit leaves an explicit issue; the uninspected remainder is not counted.
pub fn discover_recovery_report(parent: &Path) -> io::Result<RecoveryDiscovery> {
    discover_report(parent, true)
}

fn discover_report(parent: &Path, include_renames: bool) -> io::Result<RecoveryDiscovery> {
    discover_with_budget(parent, include_renames, &mut DiscoveryBudget::default())
}

fn discover_with_budget(
    parent: &Path,
    include_renames: bool,
    budget: &mut DiscoveryBudget,
) -> io::Result<RecoveryDiscovery> {
    let mut report = RecoveryDiscovery::default();
    let mut paths = Vec::new();
    for entry in fs::read_dir(parent)? {
        if !budget.entry() {
            report.issues.push(RecoveryIssue {
                path: parent.into(),
                error: invalid("recovery discovery entry limit reached; inspection incomplete"),
            });
            break;
        }
        match entry {
            Ok(entry)
                if entry.file_name().to_string_lossy().starts_with(PREFIX)
                    || (include_renames
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(crate::rename_review::PREFIX))
                    || (include_renames
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(crate::archive_journal::PREFIX)) =>
            {
                paths.push(entry.path());
            }
            Ok(_) => {}
            Err(error) => report.issues.push(RecoveryIssue {
                path: parent.into(),
                error,
            }),
        }
    }
    paths.sort();
    for path in paths {
        if budget.exhausted() {
            report.issues.push(RecoveryIssue {
                path: parent.into(),
                error: invalid("recovery discovery byte limit reached; inspection incomplete"),
            });
            break;
        }
        if path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with(crate::archive_journal::PREFIX)
        }) {
            crate::archive_journal::discover(&path, &mut report, budget);
            continue;
        }
        if path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with(crate::rename_review::PREFIX)
        }) {
            crate::rename_review::discover(&path, &mut report, budget);
            continue;
        }
        match open_record_with_budget(&path, budget) {
            Ok((record, _lock)) => report.records.push(record),
            Err(error) => report.issues.push(RecoveryIssue { path, error }),
        }
    }
    report.issues.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

/// Discover overwrite/copy-undo journals in path order, requiring every matching
/// entry to be valid and inactive. This legacy API excludes bulk-rename intents.
/// For partial results and rename intents use `discover_recovery_report`.
pub fn discover_recovery(parent: &Path) -> io::Result<Vec<RecoveryRecord>> {
    let report = discover_report(parent, false)?;
    if let Some(issue) = report.issues.into_iter().next() {
        return Err(io::Error::new(
            issue.error.kind(),
            format!("{}: {}", issue.path.display(), issue.error),
        ));
    }
    Ok(report.records)
}

/// Restore a saved original only when its destination is unoccupied. The lock
/// is held through the exclusive rename. Payload and manifest remain for review.
/// This never deletes an existing destination or writes to a recorded move source.
/// A post-move sync failure returns an `io::Error` containing
/// [`RecoveryDurabilityError`]: the destination was restored, but persistence
/// could not be confirmed. Inspect it rather than retrying the move blindly.
pub fn restore_original(transaction: &Path) -> io::Result<PathBuf> {
    restore_original_with_sync(transaction, sync_directory)
}

fn restore_original_with_sync(
    transaction: &Path,
    sync: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let (record, _lock) = open_record(transaction)?;
    if record.published {
        return Err(invalid(
            "publication was committed; the saved original may be partially cleaned up. Inspect it and recover manually",
        ));
    }
    if !record.original_available {
        return Err(invalid("journal has no saved original"));
    }
    rename_noreplace(&record.transaction.join("original"), &record.destination)
        .map_err(io::Error::other)?;
    sync_restored(&record.transaction, &record.destination, sync)?;
    Ok(record.destination)
}

/// Move an unpublished overwrite payload to an explicitly supplied unoccupied path.
/// The recorded move-source path is never used as an implicit write target.
/// Post-move sync failures contain [`RecoveryDurabilityError`], as for
/// [`restore_original`].
pub fn restore_payload(transaction: &Path, destination: &Path) -> io::Result<()> {
    restore_payload_with_sync(transaction, destination, sync_directory)
}

fn restore_payload_with_sync(
    transaction: &Path,
    destination: &Path,
    sync: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let (record, _lock) = open_record(transaction)?;
    if record.purpose != RecoveryPurpose::Overwrite || record.published {
        return Err(invalid(
            "payload restoration requires an unpublished overwrite journal; inspect unexpected payloads manually",
        ));
    }
    if !record.payload_available {
        return Err(invalid("journal has no staged payload"));
    }
    // Do not allow restoring into the journal itself, which would confuse its
    // control files and could later make cleanup remove recovered user data.
    let target = crate::copy::canonicalize_with_missing_suffix(destination)
        .ok_or_else(|| invalid("cannot resolve payload recovery destination"))?;
    if target.starts_with(&record.transaction) {
        return Err(invalid("payload recovery target is inside its journal"));
    }
    crate::move_op::move_to(
        &record.transaction.join("payload"),
        &target,
        &tokio_util::sync::CancellationToken::new(),
    )
    .map_err(io::Error::other)?;
    sync_restored(&record.transaction, &target, sync)
}

/// Recovery moved the data successfully, but directory persistence is uncertain.
/// Available by downcasting the inner error of a recovery `io::Error`.
#[derive(Debug)]
pub struct RecoveryDurabilityError {
    /// Destination installed before synchronization failed.
    pub destination: PathBuf,
    /// Every failed directory synchronization, in attempt order.
    pub failures: Vec<(PathBuf, io::Error)>,
}

impl std::fmt::Display for RecoveryDurabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "recovery restored {}; directory synchronization failed; inspect the restored destination before retrying",
            self.destination.display()
        )
    }
}

impl std::error::Error for RecoveryDurabilityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failures.first().map(|(_, error)| error as _)
    }
}

fn sync_restored(
    transaction: &Path,
    destination: &Path,
    mut sync: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = sync(transaction) {
        failures.push((transaction.to_owned(), error));
    }
    // Always attempt the destination parent, even if syncing the journal failed.
    match destination.parent() {
        Some(parent) => {
            if let Err(error) = sync(parent) {
                failures.push((parent.to_owned(), error));
            }
        }
        None => failures.push((
            destination.to_owned(),
            invalid("recovery target has no parent"),
        )),
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::new(
            failures[0].1.kind(),
            RecoveryDurabilityError {
                destination: destination.to_owned(),
                failures,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_io_failures_preserve_the_original_error() {
        let original = io::Error::from_raw_os_error(13);
        let kind = original.kind();
        let message = original.to_string();
        let error = lock_error(fs::TryLockError::Error(original));
        assert_eq!(error.raw_os_error(), Some(13));
        assert_eq!(error.kind(), kind);
        assert_eq!(error.to_string(), message);
    }

    #[test]
    fn journal_sync_attempts_both_directories_and_reports_each_failure() {
        for failures in [[false, false], [true, false], [false, true], [true, true]] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("destination");
            fs::write(&destination, b"original").unwrap();
            let journal = OverwriteJournal::begin(&destination, None).unwrap();
            let mut attempted = Vec::new();
            let result = journal.sync_with(|path| {
                let index = attempted.len();
                attempted.push(path.to_owned());
                if failures[index] {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("injected directory {index} failure"),
                    ))
                } else {
                    Ok(())
                }
            });
            assert_eq!(
                attempted,
                [
                    journal.path().to_owned(),
                    journal.path().parent().unwrap().to_owned()
                ],
                "failure case {failures:?}"
            );
            if failures.iter().any(|failed| *failed) {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                for (index, failed) in failures.iter().enumerate() {
                    if *failed {
                        assert!(
                            error
                                .to_string()
                                .contains(&format!("directory {index} failure"))
                        );
                        assert!(
                            error
                                .to_string()
                                .contains(&attempted[index].display().to_string())
                        );
                    }
                }
            } else {
                result.unwrap();
            }
            assert_eq!(fs::read(destination).unwrap(), b"original");
        }
    }

    #[test]
    fn publication_rollback_reports_sync_failure_and_retains_recovery_record() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        fs::write(&destination, b"original").unwrap();
        let mut journal = OverwriteJournal::begin(&destination, None).unwrap();
        let transaction = journal.path().to_owned();
        let mut attempts = 0;
        // No payload: installation fails and the saved original is moved back.
        let error = journal
            .publish_with_sync(|journal| {
                attempts += 1;
                if attempts == 2 {
                    Err(io::Error::other("injected rollback sync failure"))
                } else {
                    journal.sync()
                }
            })
            .unwrap_err();
        assert_eq!(attempts, 2);
        assert!(error.contains("publication failed"), "{error}");
        assert!(error.contains("original rollback: Ok"), "{error}");
        assert!(error.contains("injected rollback sync failure"), "{error}");
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        drop(journal);
        let record = inspect_recovery(&transaction).unwrap();
        assert!(!record.published);
        assert!(record.destination_occupied);
        assert!(!record.original_available);
        assert!(!record.payload_available);
    }

    #[test]
    fn recovery_sync_failures_report_installed_data_and_attempt_both_directories() {
        for payload in [false, true] {
            for failed_attempts in [[true, false], [false, true], [true, true]] {
                let root = tempfile::tempdir().unwrap();
                let destination = root.path().canonicalize().unwrap().join("destination");
                let journal = OverwriteJournal::begin(&destination, None).unwrap();
                let transaction = journal.path().to_owned();
                let source = if payload {
                    journal.payload()
                } else {
                    journal.original()
                };
                fs::write(&source, b"retained data").unwrap();
                drop(journal);
                let mut attempts = Vec::new();
                let sync = |path: &Path| {
                    let fail = failed_attempts[attempts.len()];
                    attempts.push(path.to_owned());
                    if fail {
                        Err(io::Error::from_raw_os_error(5))
                    } else {
                        Ok(())
                    }
                };
                let error = if payload {
                    restore_payload_with_sync(&transaction, &destination, sync).unwrap_err()
                } else {
                    restore_original_with_sync(&transaction, sync).unwrap_err()
                };
                let detail = error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<RecoveryDurabilityError>()
                    .unwrap();
                assert_eq!(detail.destination, destination);
                assert_eq!(
                    attempts,
                    [
                        transaction.clone(),
                        destination.parent().unwrap().to_owned()
                    ]
                );
                assert_eq!(
                    detail.failures.len(),
                    failed_attempts.iter().filter(|&&failed| failed).count()
                );
                for (path, error) in &detail.failures {
                    assert!(attempts.contains(path));
                    assert_eq!(error.raw_os_error(), Some(5));
                }
                assert!(error.to_string().contains("recovery restored"));
                assert_eq!(fs::read(&destination).unwrap(), b"retained data");
                assert!(!source.exists());
                assert!(transaction.join(MANIFEST).is_file());
                let record = inspect_recovery(&transaction).unwrap();
                assert!(record.destination_occupied);
                assert!(!record.original_available && !record.payload_available);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn control_file_replacement_between_probe_and_open_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        for write in [false, true] {
            let path = root.path().join(format!("control-{write}"));
            let retained = root.path().join(format!("retained-{write}"));
            fs::write(&path, b"original control").unwrap();
            let before = fs::symlink_metadata(&path).unwrap();
            // Keep the original inode alive, so this does not rely on timing
            // or assumptions about inode reuse by the filesystem.
            fs::rename(&path, &retained).unwrap();
            fs::write(&path, b"replacement control").unwrap();
            let error = open_regular_checked(&path, write, &before).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("replaced while opening"));
            assert_eq!(fs::read(&path).unwrap(), b"replacement control");
            assert_eq!(fs::read(&retained).unwrap(), b"original control");
            // A fresh observation accepts the new regular file.
            assert!(open_regular(&path, write).is_ok());
        }
    }
    use std::process::Command;

    #[test]
    fn discovery_entry_limit_counts_unrelated_entries_and_reports_partial() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..5 {
            fs::write(root.path().join(format!("unrelated-{index}")), b"keep").unwrap();
        }
        let mut budget = DiscoveryBudget {
            entries: 2,
            bytes: 100,
        };
        let report = discover_with_budget(root.path(), true, &mut budget).unwrap();
        assert!(report.records.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert!(report.issues[0].error.to_string().contains("entry limit"));
        let mut json = Vec::new();
        report.write_json(root.path(), &mut json).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value["status"]["state"], "partial");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 5);
    }

    #[test]
    fn discovery_shares_byte_budget_between_journals_and_rename_intents() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let journal = OverwriteJournal::begin(&root.join("destination"), None).unwrap();
        let manifest = fs::read(journal.path().join(MANIFEST)).unwrap();
        drop(journal);
        let staging = root.join(".gravityfile-rename-budget");
        fs::create_dir(&staging).unwrap();
        let intent = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "purpose": "bulk_rename_inspection_only",
            "source": crate::native_path::NativePath(&root.join("source")),
            "staged": crate::native_path::NativePath(&staging.join("item-0")),
            "destination": crate::native_path::NativePath(&root.join("target")),
        }))
        .unwrap();
        let path = staging.join("item-0.json");
        fs::write(&path, &intent).unwrap();
        let mut budget = DiscoveryBudget {
            entries: 100,
            bytes: manifest.len() as u64 + 5,
        };
        let report = discover_with_budget(&root, true, &mut budget).unwrap();
        assert_eq!(report.records.len(), 1);
        assert!(report.rename_intents.is_empty());
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.error.to_string().contains("byte limit"))
        );
        assert_eq!(budget.bytes, 0);
        assert_eq!(fs::read(&path).unwrap(), intent);
        assert_eq!(
            discover_recovery_report(&root)
                .unwrap()
                .rename_intents
                .len(),
            1
        );
    }

    #[test]
    fn discovery_nested_entry_limit_keeps_inspected_intents() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let staging = root.join(".gravityfile-rename-budget");
        fs::create_dir(&staging).unwrap();
        for index in 0..3 {
            let stem = format!("item-{index}");
            let intent = serde_json::json!({
                "schema_version": 1, "purpose": "bulk_rename_inspection_only",
                "source": crate::native_path::NativePath(&root.join(format!("source-{index}"))),
                "staged": crate::native_path::NativePath(&staging.join(&stem)),
                "destination": crate::native_path::NativePath(&root.join(format!("target-{index}"))),
            });
            fs::write(
                staging.join(format!("{stem}.json")),
                serde_json::to_vec(&intent).unwrap(),
            )
            .unwrap();
        }
        // One parent entry and one nested intent; later entries are not inspected.
        let mut budget = DiscoveryBudget {
            entries: 2,
            ..DiscoveryBudget::default()
        };
        let report = discover_with_budget(&root, true, &mut budget).unwrap();
        assert_eq!(report.rename_intents.len(), 1);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.error.to_string().contains("entry limit"))
        );
        assert_eq!(fs::read_dir(staging).unwrap().count(), 3);
    }

    #[test]
    fn payload_restore_rejects_committed_and_copy_undo_journals_without_mutation() {
        for purpose in [RecoveryPurpose::Overwrite, RecoveryPurpose::CopyUndo] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("destination");
            let restore_to = root.path().join("restore-to");
            fs::write(&destination, b"original").unwrap();
            let transaction = if purpose == RecoveryPurpose::Overwrite {
                let mut journal = OverwriteJournal::begin(&destination, None).unwrap();
                fs::write(journal.payload(), b"committed replacement").unwrap();
                journal.publish().unwrap();
                let path = journal.path().to_path_buf();
                drop(journal);
                path
            } else {
                retain_copy_undo(&destination).unwrap();
                discover_recovery(root.path()).unwrap()[0]
                    .transaction
                    .clone()
            };
            // This file is not a legitimate unpublished payload in either state.
            fs::write(transaction.join("payload"), b"later content").unwrap();
            let before = fs::read(transaction.join(MANIFEST)).unwrap();
            let error = restore_payload(&transaction, &restore_to).unwrap_err();
            assert!(error.to_string().contains("unpublished overwrite journal"));
            assert!(!restore_to.exists());
            assert_eq!(
                fs::read(transaction.join("payload")).unwrap(),
                b"later content"
            );
            assert_eq!(fs::read(transaction.join("original")).unwrap(), b"original");
            assert_eq!(fs::read(transaction.join(MANIFEST)).unwrap(), before);
            if purpose == RecoveryPurpose::Overwrite {
                assert_eq!(fs::read(&destination).unwrap(), b"committed replacement");
            } else {
                // Correct copy-undo restoration remains available.
                restore_original(&transaction).unwrap();
                assert_eq!(fs::read(&destination).unwrap(), b"original");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_rejects_replacement_types_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("directory");
        fs::create_dir(&directory).unwrap();
        sync_directory(&directory).unwrap();
        let file = root.path().join("file");
        fs::write(&file, b"unchanged").unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&directory, &link).unwrap();
        let fifo = root.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated path without interior NULs.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let file_for_worker = file.clone();
        let work = std::thread::spawn(move || {
            for path in [file_for_worker, link, fifo] {
                tx.send(sync_directory(&path).is_err()).unwrap();
            }
        });
        for _ in 0..3 {
            assert!(
                rx.recv_timeout(std::time::Duration::from_secs(5))
                    .expect("directory sync blocked on a non-directory")
            );
        }
        work.join().unwrap();
        assert_eq!(fs::read(file).unwrap(), b"unchanged");
    }

    /// Spawned only by the crash-boundary test. exit() deliberately bypasses Drop.
    #[test]
    fn crash_writer() {
        let Some(root) = std::env::var_os("GRAVITYFILE_TEST_CRASH_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let phase = std::env::var("GRAVITYFILE_TEST_CRASH_PHASE").unwrap();
        if phase.starts_with("copy-undo-") {
            let journal = OverwriteJournal::begin_for(
                &root.join("destination"),
                None,
                RecoveryPurpose::CopyUndo,
            )
            .unwrap();
            if phase == "copy-undo-retained" {
                rename_noreplace(&journal.destination, &journal.original()).unwrap();
                journal.sync().unwrap();
            }
            std::process::exit(77);
        }
        let journal = OverwriteJournal::begin(&root.join("destination"), None).unwrap();
        fs::write(journal.payload(), b"replacement").unwrap();
        if std::env::var("GRAVITYFILE_TEST_CRASH_PHASE").unwrap() != "payload" {
            rename_noreplace(&journal.destination, &journal.original()).unwrap();
            journal.sync().unwrap();
        }
        if std::env::var("GRAVITYFILE_TEST_CRASH_PHASE").unwrap() == "published" {
            rename_noreplace(&journal.payload(), &journal.destination).unwrap();
            journal.sync().unwrap();
        }
        std::process::exit(77);
    }

    #[test]
    fn copy_undo_process_exit_preserves_data_and_recovery_intent() {
        for phase in ["copy-undo-prepared", "copy-undo-retained"] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("destination");
            fs::write(&destination, b"later user edits").unwrap();
            let result = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "recovery::tests::crash_writer", "--nocapture"])
                .env("GRAVITYFILE_TEST_CRASH_ROOT", root.path())
                .env("GRAVITYFILE_TEST_CRASH_PHASE", phase)
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(77));
            let records = discover_recovery(root.path()).unwrap();
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record.purpose, RecoveryPurpose::CopyUndo);
            assert!(!record.published);
            if phase == "copy-undo-retained" {
                assert!(record.original_available);
                assert!(!record.destination_occupied);
                restore_original(&record.transaction).unwrap();
            } else {
                assert!(!record.original_available);
                assert!(record.destination_occupied);
                assert!(restore_original(&record.transaction).is_err());
            }
            assert_eq!(fs::read(destination).unwrap(), b"later user edits");
        }
    }

    #[test]
    fn legacy_recovery_manifest_defaults_to_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let journal = OverwriteJournal::begin(&root.path().join("destination"), None).unwrap();
        let transaction = journal.path().to_owned();
        drop(journal);
        let path = transaction.join(MANIFEST);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest.as_object_mut().unwrap().remove("purpose");
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            inspect_recovery(&transaction).unwrap().purpose,
            RecoveryPurpose::Overwrite
        );
    }

    #[test]
    fn recovery_after_process_exit_at_each_publication_boundary() {
        for phase in ["payload", "backup", "published"] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("destination");
            fs::write(&destination, b"original").unwrap();
            let result = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "recovery::tests::crash_writer", "--nocapture"])
                .env("GRAVITYFILE_TEST_CRASH_ROOT", root.path())
                .env("GRAVITYFILE_TEST_CRASH_PHASE", phase)
                .output()
                .unwrap();
            assert_eq!(
                result.status.code(),
                Some(77),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let records = discover_recovery(root.path()).unwrap();
            assert_eq!(records.len(), 1);
            let record = &records[0];
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&record.transaction)
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
            if phase == "backup" {
                assert!(!destination.exists());
                restore_original(&record.transaction).unwrap();
                assert_eq!(fs::read(&destination).unwrap(), b"original");
                assert_eq!(
                    fs::read(record.transaction.join("payload")).unwrap(),
                    b"replacement"
                );
            } else {
                assert!(restore_original(&record.transaction).is_err());
                assert_eq!(
                    fs::read(&destination).unwrap(),
                    if phase == "payload" {
                        b"original".as_slice()
                    } else {
                        b"replacement".as_slice()
                    }
                );
            }
        }
    }

    #[test]
    fn live_journal_cannot_be_recovered() {
        let root = tempfile::tempdir().unwrap();
        let journal = OverwriteJournal::begin(&root.path().join("destination"), None).unwrap();
        assert!(inspect_recovery(journal.path()).is_err());
        let path = journal.path().to_path_buf();
        drop(journal);
        assert!(inspect_recovery(&path).is_ok());
    }

    #[test]
    fn staged_move_payload_can_be_recovered_only_to_an_explicit_free_path() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"source data").unwrap();
        let journal =
            OverwriteJournal::begin(&root.path().join("destination"), Some(&source)).unwrap();
        rename_noreplace(&source, &journal.payload()).unwrap();
        let path = journal.path().to_path_buf();
        drop(journal);
        assert!(restore_payload(&path, &path.join("misplaced")).is_err());
        fs::write(&source, b"new source").unwrap();
        assert!(restore_payload(&path, &source).is_err());
        let explicit = root.path().join("recovered");
        restore_payload(&path, &explicit).unwrap();
        assert_eq!(fs::read(explicit).unwrap(), b"source data");
        assert_eq!(fs::read(source).unwrap(), b"new source");
        assert!(!inspect_recovery(&path).unwrap().payload_available);
    }

    #[cfg(unix)]
    #[test]
    fn duplicated_descriptor_cannot_extend_completed_lock_ownership() {
        let root = tempfile::tempdir().unwrap();
        let journal = OverwriteJournal::begin(&root.path().join("destination"), None).unwrap();
        let path = journal.path().to_path_buf();
        let duplicate = journal.lock.0.try_clone().unwrap();
        assert!(inspect_recovery(&path).is_err());
        drop(journal);
        assert!(inspect_recovery(&path).is_ok());
        drop(duplicate);
    }

    #[test]
    fn recovery_never_overwrites_a_concurrently_created_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        fs::write(&destination, b"original").unwrap();
        let journal = OverwriteJournal::begin(&destination, None).unwrap();
        rename_noreplace(&destination, &journal.original()).unwrap();
        let path = journal.path().to_path_buf();
        drop(journal);
        fs::write(&destination, b"concurrent writer").unwrap();
        assert!(restore_original(&path).is_err());
        assert_eq!(fs::read(destination).unwrap(), b"concurrent writer");
        assert_eq!(fs::read(path.join("original")).unwrap(), b"original");
    }

    #[test]
    fn committed_backups_are_not_presented_as_complete_originals() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        fs::write(&destination, b"original").unwrap();
        let mut journal = OverwriteJournal::begin(&destination, None).unwrap();
        fs::write(journal.payload(), b"replacement").unwrap();
        journal.publish().unwrap();
        let path = journal.path().to_path_buf();
        drop(journal);
        // Simulate a later removal of the published destination. Even now a
        // backup from committed cleanup cannot be assumed complete.
        fs::remove_file(&destination).unwrap();
        assert!(restore_original(&path).is_err());
        assert!(!destination.exists());
        assert_eq!(fs::read(path.join("original")).unwrap(), b"original");
    }

    #[test]
    fn successful_publication_cleans_up_and_failure_preserves_old_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        fs::write(&destination, b"original").unwrap();
        let mut journal = OverwriteJournal::begin(&destination, None).unwrap();
        // A missing payload forces publication failure after saving the original.
        assert!(journal.publish().is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        fs::write(journal.payload(), b"replacement").unwrap();
        journal.publish().unwrap();
        journal.finish().unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"replacement");
        assert!(discover_recovery(root.path()).unwrap().is_empty());
    }

    #[test]
    fn malformed_or_traversing_manifests_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = OverwriteJournal::begin(&root.path().join("destination"), None).unwrap();
        journal.manifest.destination_name = NativePath::encode(Path::new("../escape"));
        write_manifest(journal.path(), &journal.manifest).unwrap();
        let path = journal.path().to_path_buf();
        drop(journal);
        assert!(inspect_recovery(&path).is_err());
        fs::write(path.join(MANIFEST), b"not json").unwrap();
        assert!(inspect_recovery(&path).is_err());
        fs::write(
            path.join(MANIFEST),
            vec![0; MAX_MANIFEST_BYTES as usize + 1],
        )
        .unwrap();
        assert!(inspect_recovery(&path).is_err());
    }

    #[test]
    fn discovery_keeps_valid_records_beside_active_and_malformed_journals() {
        let root = tempfile::tempdir().unwrap();
        let valid = OverwriteJournal::begin(&root.path().join("valid"), None).unwrap();
        let valid_path = valid.path().to_owned();
        fs::write(valid.original(), b"saved data").unwrap();
        drop(valid);
        let active = OverwriteJournal::begin(&root.path().join("active"), None).unwrap();
        let malformed = root.path().join(".gravityfile-recovery-malformed");
        fs::create_dir(&malformed).unwrap();
        fs::write(malformed.join("lock"), b"").unwrap();
        fs::write(malformed.join(MANIFEST), b"not json").unwrap();
        fs::write(root.path().join("unrelated"), b"keep").unwrap();

        let report = discover_recovery_report(root.path()).unwrap();
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].transaction, valid_path);
        assert_eq!(report.issues.len(), 2);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.path.canonicalize().unwrap() == active.path())
        );
        let active_issue = report
            .issues
            .iter()
            .find(|issue| issue.path.canonicalize().unwrap() == active.path())
            .unwrap();
        assert_eq!(active_issue.error.kind(), io::ErrorKind::WouldBlock);
        assert!(report.issues.iter().any(|issue| issue.path == malformed));
        assert!(
            report
                .issues
                .windows(2)
                .all(|pair| pair[0].path <= pair[1].path)
        );
        assert!(discover_recovery(root.path()).is_err());
        assert_eq!(
            fs::read(valid_path.join("original")).unwrap(),
            b"saved data"
        );
        assert_eq!(fs::read(malformed.join(MANIFEST)).unwrap(), b"not json");
        drop(active);
        let report = discover_recovery_report(root.path()).unwrap();
        assert_eq!(report.records.len(), 2);
        assert_eq!(report.issues.len(), 1);
        assert!(
            report
                .records
                .windows(2)
                .all(|pair| pair[0].transaction <= pair[1].transaction)
        );
    }

    #[test]
    fn recovery_discovery_distinguishes_empty_and_unreadable_parent() {
        let root = tempfile::tempdir().unwrap();
        let report = discover_recovery_report(root.path()).unwrap();
        assert!(report.records.is_empty() && report.issues.is_empty());
        assert!(discover_recovery_report(&root.path().join("missing")).is_err());
        let file = root.path().join("file");
        fs::write(&file, b"keep").unwrap();
        assert!(discover_recovery_report(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn native_filename_roundtrip_and_symlink_control_file_rejection() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join(OsString::from_vec(vec![b'f', 0xff]));
        let journal = OverwriteJournal::begin(&destination, None).unwrap();
        let path = journal.path().to_path_buf();
        drop(journal);
        assert_eq!(
            inspect_recovery(&path).unwrap().destination,
            root.path()
                .canonicalize()
                .unwrap()
                .join(destination.file_name().unwrap())
        );
        let manifest = path.join(MANIFEST);
        fs::rename(&manifest, root.path().join("external")).unwrap();
        std::os::unix::fs::symlink(root.path().join("external"), &manifest).unwrap();
        assert!(inspect_recovery(&path).is_err());
    }
}
