//! Rename operation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::copy::rename_noreplace;
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::rename_journal::RenamePhase;
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Result sent through the channel during rename operations.
#[derive(Debug)]
pub enum RenameResult {
    /// Progress update.
    Progress(OperationProgress),
    /// The operation completed.
    Complete(OperationComplete),
}

/// Start an async rename operation.
///
/// Renames a single file or directory.
pub fn start_rename(source: PathBuf, new_name: String) -> mpsc::Receiver<RenameResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);

    tokio::spawn(async move {
        rename_impl(source, new_name, tx).await;
    });

    rx
}

/// Rename a set of paths as one transaction, including swaps and cycles.
///
/// Every source is first moved into a private sibling staging directory, then all
/// staged paths are installed at their targets with no-replace semantics. Any
/// failure attempts to roll installed and staged paths back to their original
/// names without replacement. Rollback failures report retained paths. This is
/// not crash-atomic. Synced intent and progress records preserve native paths
/// and historical phases for inspection; they never permit automatic replay.
/// Parents must already exist. Duplicate entries and paths beneath another
/// source in the batch are rejected before staging, including no-op entries.
pub fn rename_paths_transactional(renames: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    rename_paths_with_cancellation(renames, &CancellationToken::new())
}

/// Rename a batch with cooperative cancellation and uncancelled rollback.
/// Cancellation before the final installation attempts to restore every source;
/// rollback failures retain paths and are included in the returned error.
/// A completed final installation commits the batch even if cancellation arrives
/// immediately afterwards. Blocking filesystem calls cannot be interrupted.
pub fn rename_paths_with_cancellation(
    renames: &[(PathBuf, PathBuf)],
    token: &CancellationToken,
) -> Result<(), String> {
    rename_paths_impl(renames, token, rename_noreplace)
}

fn rename_paths_impl(
    renames: &[(PathBuf, PathBuf)],
    token: &CancellationToken,
    rename: impl FnMut(&Path, &Path) -> Result<(), crate::copy::RenameError>,
) -> Result<(), String> {
    rename_paths_observed(renames, token, rename, |_, _| {})
}

fn rename_paths_observed(
    renames: &[(PathBuf, PathBuf)],
    token: &CancellationToken,
    mut rename: impl FnMut(&Path, &Path) -> Result<(), crate::copy::RenameError>,
    observer: fn(&Path, RenamePhase),
) -> Result<(), String> {
    let check_cancelled = || {
        if token.is_cancelled() {
            Err(crate::copy::CANCELLED_MESSAGE.to_string())
        } else {
            Ok(())
        }
    };
    check_cancelled()?;
    // Resolve parent aliases without following the leaf itself: renaming a
    // symlink must continue to operate on that link, not its referent.
    let renames: Vec<_> = renames
        .iter()
        .map(|(source, target)| {
            check_cancelled()?;
            Ok((rename_path(source)?, rename_path(target)?))
        })
        .collect::<Result<_, String>>()?;
    let sources: HashSet<&Path> = renames.iter().map(|(source, _)| source.as_path()).collect();
    let targets: HashSet<&Path> = renames.iter().map(|(_, target)| target.as_path()).collect();
    if sources.len() != renames.len() {
        return Err("Bulk rename contains the same source more than once".to_string());
    }
    if targets.len() != renames.len() {
        return Err("Bulk rename contains duplicate destinations".to_string());
    }

    // A source directory must not be staged out from beneath another source
    // or destination. Check ancestors against the set, avoiding pairwise scans.
    for (source, target) in &renames {
        check_cancelled()?;
        for path in [source, target] {
            if let Some(ancestor) = path
                .ancestors()
                .skip(1)
                .find(|ancestor| sources.contains(ancestor))
            {
                return Err(format!(
                    "Bulk rename path '{}' is beneath source '{}'",
                    path.display(),
                    ancestor.display()
                ));
            }
        }
    }

    for (source, target) in &renames {
        check_cancelled()?;
        std::fs::symlink_metadata(source)
            .map_err(|error| format!("Cannot access '{}': {error}", source.display()))?;
        match std::fs::symlink_metadata(target) {
            Ok(_) if !sources.contains(target.as_path()) => {
                return Err(format!("'{}' already exists", target.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("Cannot inspect '{}': {error}", target.display()));
            }
        }
    }

    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::with_capacity(renames.len());
    let mut staging = RenameStaging::new(observer);
    let mut planned = Vec::new();
    if renames.len() > crate::rename_batch::MAX_ITEMS {
        return Err("Bulk rename exceeds recoverable item limit".into());
    }
    for (source, target) in renames {
        if source == target {
            continue;
        }
        check_cancelled()?;
        let temporary = staging
            .payload(&source, planned.len())
            .map_err(|e| e.to_string())?;
        if let Err(error) = staging.record(&source, &temporary, &target) {
            return Err(format!(
                "Failed to prepare rename intent before mutation: {error}"
            ));
        }
        planned.push((source, temporary, target));
    }
    staging.batch_files = match crate::rename_batch::prepare(&planned, &staging.identities) {
        Ok(files) => files,
        Err(error) => {
            staging.cleanup_safe = false;
            return Err(format!(
                "Failed to prepare batch before mutation: {error}; inspect retained journals"
            ));
        }
    };
    for (source, temporary, target) in planned {
        if let Err(error) = check_cancelled() {
            return Err(format_rename_failure(error, staging.rollback(&staged, 0)));
        }
        staging.cleanup_safe = false;
        if let Err(error) = staging.verify(&temporary, &source) {
            return Err(format_rename_failure(
                error.to_string(),
                staging.rollback(&staged, 0),
            ));
        }
        if let Err(error) = rename(&source, &temporary) {
            let rollback = staging.rollback(&staged, 0);
            return Err(format_rename_failure(
                format!("Failed to stage '{}': {error}", source.display()),
                rollback,
            ));
        }
        staged.push((source, temporary, target));
        let (source, temporary, _) = staged.last().expect("just staged");
        if let Err(error) = staging.after_move(source, temporary, temporary, RenamePhase::Staged) {
            return Err(format_rename_failure(
                format!("Staged item but failed to record progress: {error}"),
                staging.rollback(&staged, 0),
            ));
        }
    }

    for (installed, (_, temporary, target)) in staged.iter().enumerate() {
        if let Err(error) = check_cancelled() {
            return Err(format_rename_failure(
                error,
                staging.rollback(&staged, installed),
            ));
        }
        if let Err(error) = staging.phase(temporary, RenamePhase::InstallPending) {
            return Err(format_rename_failure(
                format!("Failed to record installation intent: {error}"),
                staging.rollback(&staged, installed),
            ));
        }
        if let Err(error) = staging.verify(temporary, temporary) {
            return Err(format_rename_failure(
                error.to_string(),
                staging.rollback(&staged, installed),
            ));
        }
        if let Err(error) = rename(temporary, target) {
            let rollback = staging.rollback(&staged, installed);
            return Err(format_rename_failure(
                format!("Failed to install '{}': {error}", target.display()),
                rollback,
            ));
        }
        if let Err(error) = staging.after_move(temporary, target, temporary, RenamePhase::Installed)
        {
            // The final installation is the existing commit boundary. Never
            // undo the completed batch because recording its acknowledgement failed.
            if installed + 1 == staged.len() {
                return Err(format!(
                    "Renames installed but progress recording failed: {error}; inspect retained .gravityfile-rename-* records before retrying"
                ));
            }
            return Err(format_rename_failure(
                format!("Installed item but failed to record progress: {error}"),
                staging.rollback(&staged, installed + 1),
            ));
        }
    }

    staging.settle(&staged).map_err(|error| {
        format!("Renames installed but directory sync failed: {error}; inspect retained .gravityfile-rename-* intent records")
    })?;
    if let Some(path) = crate::rename_batch::commit(&staging.batch_files).map_err(|error| format!("Renames installed but batch commit recording failed: {error}; inspect retained journals before retrying"))? {
        staging.batch_files.push(path);
    }
    staging.cleanup_safe = !staging.journal_failed;
    Ok(())
}

/// Reserve one private directory per source parent. Never use TempDir's recursive
/// destructor after a payload may have been moved: failures and unwinding must
/// preserve data. Empty-only cleanup also preserves unexpected concurrent entries.
struct RenameStaging {
    directories: HashMap<PathBuf, PathBuf>,
    intent_files: Vec<PathBuf>,
    locks: Vec<crate::recovery::RecoveryLock>,
    cleanup_safe: bool,
    phases: HashMap<PathBuf, (usize, RenamePhase)>,
    event_files: Vec<PathBuf>,
    journal_failed: bool,
    observer: fn(&Path, RenamePhase),
    identities: HashMap<PathBuf, crate::ObjectIdentity>,
    batch_files: Vec<PathBuf>,
    parents: HashMap<PathBuf, crate::ObjectIdentity>,
}

impl Default for RenameStaging {
    fn default() -> Self {
        Self::new(|_, _| {})
    }
}

impl RenameStaging {
    fn new(observer: fn(&Path, RenamePhase)) -> Self {
        Self {
            directories: HashMap::new(),
            intent_files: Vec::new(),
            locks: Vec::new(),
            cleanup_safe: true,
            phases: HashMap::new(),
            event_files: Vec::new(),
            journal_failed: false,
            observer,
            identities: HashMap::new(),
            batch_files: Vec::new(),
            parents: HashMap::new(),
        }
    }

    fn record(&mut self, source: &Path, staged: &Path, destination: &Path) -> std::io::Result<()> {
        use crate::native_path::NativePath;
        use std::io::Write;
        #[derive(serde::Serialize)]
        struct Intent<'a> {
            schema_version: u32,
            journal_version: u32,
            lock_required: bool,
            purpose: &'static str,
            outcome: &'static str,
            source: NativePath<'a>,
            staged: NativePath<'a>,
            destination: NativePath<'a>,
            object_identity: &'a crate::ObjectIdentity,
        }
        let identity = crate::ObjectIdentity::capture(source)?;
        for path in [source, staged, destination] {
            let parent = path.parent().expect("normalized rename parent");
            if !self.parents.contains_key(parent) {
                self.parents
                    .insert(parent.into(), crate::ObjectIdentity::capture(parent)?);
            }
        }
        let path = staged.with_extension("json");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        self.intent_files.push(path);
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(
            &mut writer,
            &Intent {
                schema_version: 1,
                journal_version: 1,
                lock_required: true,
                purpose: "bulk_rename_inspection_only",
                outcome: "unverified; inspect source, staged, and destination paths",
                source: NativePath(source),
                staged: NativePath(staged),
                destination: NativePath(destination),
                object_identity: &identity,
            },
        )
        .map_err(std::io::Error::other)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        let directory = staged.parent().expect("reserved payload has a parent");
        crate::recovery::sync_directory(directory)?;
        crate::recovery::sync_directory(directory.parent().expect("staging has a parent"))?;
        self.phases
            .insert(staged.into(), (0, RenamePhase::StagePending));
        self.identities.insert(staged.into(), identity);
        (self.observer)(staged, RenamePhase::StagePending);
        Ok(())
    }

    fn verify(&self, payload: &Path, path: &Path) -> std::io::Result<()> {
        self.parents
            .get(path.parent().expect("rename parent"))
            .ok_or_else(|| std::io::Error::other("missing rename parent identity"))?
            .verify(path.parent().unwrap())?;
        self.identities
            .get(payload)
            .ok_or_else(|| std::io::Error::other("missing rename object identity"))?
            .verify(path)
    }

    fn phase(&mut self, payload: &Path, phase: RenamePhase) -> std::io::Result<()> {
        self.cleanup_safe = false;
        if self.journal_failed {
            return Err(std::io::Error::other(
                "rename progress journal unavailable; retained data requires inspection",
            ));
        }
        let result = (|| {
            let &(sequence, previous) = self
                .phases
                .get(payload)
                .ok_or_else(|| std::io::Error::other("missing rename progress intent"))?;
            if !phase.follows(previous) {
                return Err(std::io::Error::other("invalid rename progress transition"));
            }
            // Track even a partially written event for successful cleanup only.
            self.event_files
                .push(crate::rename_journal::event_path(payload, sequence));
            crate::rename_journal::write(payload, sequence, phase)?;
            self.phases.insert(payload.into(), (sequence + 1, phase));
            Ok(())
        })();
        if result.is_err() {
            self.journal_failed = true;
        }
        result?;
        (self.observer)(payload, phase);
        Ok(())
    }

    fn after_move(
        &mut self,
        from: &Path,
        to: &Path,
        payload: &Path,
        phase: RenamePhase,
    ) -> std::io::Result<()> {
        self.after_move_with_sync(from, to, payload, phase, crate::recovery::sync_directory)
    }

    fn after_move_with_sync(
        &mut self,
        from: &Path,
        to: &Path,
        payload: &Path,
        phase: RenamePhase,
        mut sync: impl FnMut(&Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.cleanup_safe = false;
        self.verify(payload, to)?;
        // Both directories must be synced before recording a completed move.
        let first = sync(from.parent().expect("move source parent"));
        let second = sync(to.parent().expect("move destination parent"));
        first.and(second)?;
        self.phase(payload, phase)
    }

    fn rollback(
        &mut self,
        staged: &[(PathBuf, PathBuf, PathBuf)],
        installed: usize,
    ) -> Vec<String> {
        let mut errors =
            rollback_staged_renames_with(staged, installed, |from, to, payload, before, after| {
                self.verify(payload, from)
                    .map_err(|error| error.to_string())?;
                self.phase(payload, before)
                    .map_err(|error| error.to_string())?;
                rename_noreplace(from, to).map_err(|error| error.to_string())?;
                self.after_move(from, to, payload, after)
                    .map_err(|error| error.to_string())
            });
        if errors.is_empty() {
            if let Err(error) = self.settle(staged) {
                errors.push(format!(
                    "rollback directory sync failed: {error}; rename intent records retained"
                ));
            } else {
                self.cleanup_safe = !self.journal_failed;
            }
        } else {
            self.cleanup_safe = false;
        }
        errors
    }

    fn settle(&mut self, staged: &[(PathBuf, PathBuf, PathBuf)]) -> std::io::Result<()> {
        self.cleanup_safe = false;
        let mut directories: HashSet<&Path> =
            self.directories.values().map(PathBuf::as_path).collect();
        directories.extend(self.directories.keys().map(PathBuf::as_path));
        for (source, _, target) in staged {
            directories.extend(source.parent());
            directories.extend(target.parent());
        }
        for directory in directories {
            crate::recovery::sync_directory(directory).map_err(|error| {
                std::io::Error::new(error.kind(), format!("{}: {error}", directory.display()))
            })?;
        }
        Ok(())
    }

    fn payload(&mut self, source: &Path, index: usize) -> std::io::Result<PathBuf> {
        let parent = source.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no parent")
        })?;
        if !self.directories.contains_key(parent) {
            let directory = crate::staging::private_directory(parent, ".gravityfile-rename-")?;
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let lock =
                crate::recovery::RecoveryLock::new(options.open(directory.path().join("lock"))?)?;
            self.locks.push(lock);
            self.directories
                .insert(parent.to_path_buf(), directory.keep());
        }
        Ok(self.directories[parent].join(format!("item-{index}")))
    }
}

impl Drop for RenameStaging {
    fn drop(&mut self) {
        if self.cleanup_safe {
            let mut removed_all = true;
            for intent in self
                .event_files
                .iter()
                .chain(&self.intent_files)
                .chain(&self.batch_files)
            {
                if let Err(error) = std::fs::remove_file(intent) {
                    removed_all = false;
                    tracing::warn!(path = %intent.display(), %error, "Bulk rename intent retained");
                }
            }
            // Intent cleanup is coordinated. Release handles before unlinking
            // lock files, including on platforms that deny unlinking open files.
            if removed_all {
                self.locks.clear();
                for directory in self.directories.values() {
                    if let Err(error) = std::fs::remove_file(directory.join("lock")) {
                        tracing::warn!(path = %directory.display(), %error, "Bulk rename lock file retained");
                    }
                }
            }
        }
        for directory in self.directories.values() {
            if let Err(error) = std::fs::remove_dir(directory) {
                tracing::warn!(path = %directory.display(), %error, "Bulk rename staging retained; inspect remaining data");
            }
        }
    }
}

fn rename_path(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("Bulk rename path '{}' has no filename", path.display()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("Cannot resolve parent of '{}': {error}", path.display()))?;
    Ok(parent.join(name))
}

#[cfg(test)]
fn rollback_staged_renames(
    staged: &[(PathBuf, PathBuf, PathBuf)],
    installed: usize,
) -> Vec<String> {
    rollback_staged_renames_with(staged, installed, |from, to, _, _, _| {
        rename_noreplace(from, to).map_err(|error| error.to_string())
    })
}

fn rollback_staged_renames_with(
    staged: &[(PathBuf, PathBuf, PathBuf)],
    installed: usize,
    mut rename: impl FnMut(&Path, &Path, &Path, RenamePhase, RenamePhase) -> Result<(), String>,
) -> Vec<String> {
    let mut errors = Vec::new();

    // Only restore an installed entry after successfully restaging it. If a
    // staging path was occupied concurrently, it is not our payload and must
    // never be moved into the original source location.
    let mut restorable = vec![true; staged.len()];
    for (index, (_, temporary, target)) in staged[..installed].iter().enumerate().rev() {
        if let Err(error) = rename(
            target,
            temporary,
            temporary,
            RenamePhase::RestagePending,
            RenamePhase::Restaged,
        ) {
            restorable[index] = false;
            errors.push(format!(
                "failed to restage '{}' from '{}': {error}",
                temporary.display(),
                target.display()
            ));
        }
    }

    for (index, (source, temporary, _)) in staged.iter().enumerate().rev() {
        if !restorable[index] {
            continue;
        }
        // Missing/inaccessible staged payloads are rollback failures too. An
        // existence probe must not silently turn them into successful recovery.
        if let Err(error) = rename(
            temporary,
            source,
            temporary,
            RenamePhase::RestorePending,
            RenamePhase::Restored,
        ) {
            errors.push(format!(
                "failed to restore '{}' from '{}': {error}",
                source.display(),
                temporary.display()
            ));
        }
    }
    errors
}

fn format_rename_failure(message: String, rollback_errors: Vec<String>) -> String {
    if rollback_errors.is_empty() {
        message
    } else {
        format!("{message}; rollback errors: {}", rollback_errors.join("; "))
    }
}

/// Internal implementation of rename operation.
async fn rename_impl(source: PathBuf, new_name: String, tx: mpsc::Sender<RenameResult>) {
    match tx.try_reserve() {
        Ok(permit) => {
            let mut progress = OperationProgress::new(OperationType::Rename, 1, 0);
            progress.set_current_file(Some(source.clone()));
            permit.send(RenameResult::Progress(progress));
        }
        Err(mpsc::error::TrySendError::Closed(_)) => return,
        Err(mpsc::error::TrySendError::Full(_)) => {}
    }
    let work_source = source.clone();
    let receiver = tx.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(usize, bool), String> {
        if receiver.is_closed() {
            return Ok((0, true));
        }
        validate_filename(&new_name)?;
        work_source
            .symlink_metadata()
            .map_err(|error| format!("Cannot inspect source: {error}"))?;
        let parent = work_source.parent().unwrap_or(std::path::Path::new(""));
        let destination = parent.join(&new_name);
        if destination == work_source {
            return Ok((0, false));
        }
        match destination.symlink_metadata() {
            Ok(_) => return Err(format!("'{new_name}' already exists")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot inspect destination: {error}")),
        }
        if receiver.is_closed() {
            return Ok((0, true));
        }
        rename_noreplace(&work_source, &destination)
            .map_err(|error| format!("Rename failed: {error}"))?;
        Ok((1, false))
    })
    .await;
    let (succeeded, failed, cancelled, errors) = match result {
        Ok(Ok((count, cancelled))) => (count, 0, cancelled, Vec::new()),
        Ok(Err(error)) => (0, 1, false, vec![OperationError::new(source, error)]),
        Err(error) => (
            0,
            1,
            false,
            vec![OperationError::new(source, format!("Task failed: {error}"))],
        ),
    };
    let _ = tx
        .send(RenameResult::Complete(OperationComplete {
            operation_type: OperationType::Rename,
            succeeded,
            failed,
            cancelled,
            skipped: 0,
            bytes_processed: 0,
            errors,
        }))
        .await;
}

/// Validate a new UTF-8 filename against application and target-specific rules.
pub fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty".into());
    }

    // Conservative application byte budget, not a universal filesystem limit.
    // Actual component limits depend on the filesystem and encoding; this policy
    // also avoids accepting names that exceed common Unix byte limits.
    if name.len() > 255 {
        return Err("Name is too long (max 255 bytes)".into());
    }

    // Check for invalid characters
    let invalid_chars = ['/', '\0'];
    for c in invalid_chars {
        if name.contains(c) {
            return Err(format!("Name cannot contain '{}'", c));
        }
    }

    // Reject control characters (newline, tab, DEL, etc.): they break many
    // tools and enable terminal display spoofing.
    if name.chars().any(|c| c.is_control()) {
        return Err("Name cannot contain control characters".into());
    }

    #[cfg(target_os = "windows")]
    validate_windows_name(name)?;

    // Check for leading/trailing spaces or dots (problematic on Windows)
    if name.starts_with(' ') || name.ends_with(' ') {
        return Err("Name cannot start or end with spaces".into());
    }

    if name.ends_with('.') {
        return Err("Name cannot end with a dot".into());
    }

    // Check for . and .. which are reserved
    if name == "." || name == ".." {
        return Err("'.' and '..' are reserved names".into());
    }

    Ok(())
}

/// Win32 component rules, kept host-testable without imposing them on Unix.
/// <https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file>
#[cfg(any(target_os = "windows", test))]
fn validate_windows_name(name: &str) -> Result<(), String> {
    let windows_invalid = ['\\', ':', '*', '?', '"', '<', '>', '|'];
    for c in windows_invalid {
        if name.contains(c) {
            return Err(format!("Name cannot contain '{}'", c));
        }
    }

    // Check for reserved names
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³", "LPT1", "LPT2", "LPT3", "LPT4",
        "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let upper_name = name.to_uppercase();
    let base_name = upper_name.split('.').next().unwrap_or("");
    if reserved.contains(&base_name) {
        return Err("Reserved filename".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_refuses_replaced_staged_or_installed_objects_without_moving_replacements() {
        for fail_call in [1, 3] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path().canonicalize().unwrap();
            let first = root.join("first");
            let second = root.join("second");
            let saved = root.join("saved");
            std::fs::write(&first, b"first").unwrap();
            std::fs::write(&second, b"second").unwrap();
            let mut calls = 0;
            let mut replaced = None;
            let error = rename_paths_impl(
                &[
                    (first.clone(), second.clone()),
                    (second.clone(), first.clone()),
                ],
                &CancellationToken::new(),
                |from, to| {
                    rename_noreplace(from, to)?;
                    calls += 1;
                    if calls == fail_call {
                        std::fs::rename(to, &saved).unwrap();
                        std::fs::write(to, b"unrelated replacement").unwrap();
                        replaced = Some(to.to_path_buf());
                    }
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(error.contains("identity changed"), "{error}");
            assert_eq!(std::fs::read(saved).unwrap(), b"first");
            assert_eq!(
                std::fs::read(replaced.unwrap()).unwrap(),
                b"unrelated replacement"
            );
            assert!(!first.exists());
            let report = crate::discover_recovery_report(&root).unwrap();
            assert!(report.issues.is_empty());
            assert_eq!(report.rename_intents.len(), 2);
        }
    }

    #[test]
    fn completed_phase_requires_both_directory_syncs_and_keeps_pending_on_failure() {
        for failing in [0, 1] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let source = root.join("source");
            std::fs::write(&source, b"payload").unwrap();
            let mut staging = RenameStaging::default();
            let payload = staging.payload(&source, 0).unwrap();
            staging
                .record(&source, &payload, &root.join("target"))
                .unwrap();
            rename_noreplace(&source, &payload).unwrap();
            let mut attempts = Vec::new();
            let error = staging
                .after_move_with_sync(&source, &payload, &payload, RenamePhase::Staged, |path| {
                    let fail = attempts.len() == failing;
                    attempts.push(path.to_path_buf());
                    if fail {
                        Err(std::io::Error::other("injected sync failure"))
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();
            assert!(error.to_string().contains("injected sync failure"));
            assert_eq!(attempts, [root.clone(), payload.parent().unwrap().into()]);
            assert!(!crate::rename_journal::event_path(&payload, 0).exists());
            drop(staging);
            let report = crate::discover_recovery_report(&root).unwrap();
            assert!(report.issues.is_empty());
            assert_eq!(
                report.rename_intents[0].last_recorded_phase,
                Some(RenamePhase::StagePending)
            );
            assert_eq!(std::fs::read(payload).unwrap(), b"payload");
        }
    }

    #[test]
    fn failed_install_intent_prevents_forward_mutation_and_preserves_staged_payloads() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let first = root.join("first");
        let second = root.join("second");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let mut calls = 0;
        let mut collision = None;
        let error = rename_paths_impl(
            &[
                (first.clone(), second.clone()),
                (second.clone(), first.clone()),
            ],
            &CancellationToken::new(),
            |from, to| {
                calls += 1;
                rename_noreplace(from, to)?;
                if calls == 2 {
                    let path =
                        crate::rename_journal::event_path(&to.parent().unwrap().join("item-0"), 1);
                    std::fs::write(&path, b"competing event").unwrap();
                    collision = Some(path);
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(calls, 2);
        assert!(
            error.contains("Failed to record installation intent"),
            "{error}"
        );
        assert!(!first.exists() && !second.exists());
        assert_eq!(
            std::fs::read(collision.unwrap()).unwrap(),
            b"competing event"
        );
        let report = crate::discover_recovery_report(&root).unwrap();
        assert_eq!(report.rename_intents.len(), 2);
        let mut bytes: Vec<_> = report
            .rename_intents
            .iter()
            .map(|item| std::fs::read(&item.staged).unwrap())
            .collect();
        bytes.sort();
        assert_eq!(bytes, [b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn rename_progress_crash_worker() {
        let Some(root) = std::env::var_os("GRAVITYFILE_RENAME_PROGRESS_ROOT") else {
            return;
        };
        fn observe(_: &Path, phase: RenamePhase) {
            let wanted: RenamePhase =
                serde_json::from_str(&std::env::var("GRAVITYFILE_RENAME_PROGRESS_PHASE").unwrap())
                    .unwrap();
            if phase == wanted {
                std::process::exit(79);
            }
        }
        let root = PathBuf::from(root);
        let multi = std::env::var_os("GRAVITYFILE_RENAME_MULTI_PARENT").is_some();
        let first = if multi {
            root.join("left/first")
        } else {
            root.join("first")
        };
        let second = if multi {
            root.join("right/second")
        } else {
            root.join("second")
        };
        let rollback = std::env::var_os("GRAVITYFILE_RENAME_PROGRESS_ROLLBACK").is_some();
        let mut calls = 0;
        let _ = rename_paths_observed(
            &[(first.clone(), second.clone()), (second, first)],
            &CancellationToken::new(),
            |from, to| {
                calls += 1;
                if rollback && calls == 4 {
                    return Err(crate::copy::RenameError::Io(std::io::Error::other(
                        "injected installation failure",
                    )));
                }
                rename_noreplace(from, to)
            },
            observe,
        );
        panic!("progress crash phase not reached");
    }

    #[test]
    fn process_exit_at_every_recorded_phase_preserves_data_and_inspectable_progress() {
        use RenamePhase::*;
        for (phase, rollback) in [
            (StagePending, false),
            (Staged, false),
            (InstallPending, false),
            (Installed, false),
            (RestagePending, true),
            (Restaged, true),
            (RestorePending, true),
            (Restored, true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            std::fs::write(root.join("first"), b"first payload").unwrap();
            std::fs::write(root.join("second"), b"second payload").unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "rename::tests::rename_progress_crash_worker",
                    "--nocapture",
                ])
                .env("GRAVITYFILE_RENAME_PROGRESS_ROOT", &root)
                .env(
                    "GRAVITYFILE_RENAME_PROGRESS_PHASE",
                    serde_json::to_string(&phase).unwrap(),
                );
            if rollback {
                command.env("GRAVITYFILE_RENAME_PROGRESS_ROLLBACK", "1");
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(79),
                "{phase:?}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let report = crate::discover_recovery_report(&root).unwrap();
            assert!(report.issues.is_empty(), "{phase:?}: {:?}", report.issues);
            assert!(
                report
                    .rename_intents
                    .iter()
                    .any(|item| item.last_recorded_phase == Some(phase)),
                "{phase:?}"
            );
            let mut paths = HashSet::from([root.join("first"), root.join("second")]);
            for item in &report.rename_intents {
                paths.insert(item.staged.clone());
                assert!(item.locked_during_inspection);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    for event in std::fs::read_dir(&item.transaction)
                        .unwrap()
                        .map(Result::unwrap)
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "event"))
                    {
                        assert_eq!(
                            event.metadata().unwrap().permissions().mode() & 0o777,
                            0o600
                        );
                    }
                }
            }
            let mut bytes: Vec<_> = paths
                .iter()
                .filter(|path| path.exists())
                .map(|path| std::fs::read(path).unwrap())
                .collect();
            bytes.sort();
            assert_eq!(
                bytes,
                [b"first payload".to_vec(), b"second payload".to_vec()],
                "{phase:?}"
            );
            let mut json = Vec::new();
            report.write_json(&root, &mut json).unwrap();
            let document: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert!(document["rename_intents"].as_array().unwrap().iter()
                .any(|item| item["last_recorded_phase"] == serde_json::to_value(phase).unwrap()));
            assert!(
                document["rename_intents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|item| item["outcome"].as_str().unwrap().starts_with("unverified"))
            );
        }
    }

    #[test]
    fn failed_final_progress_record_keeps_completed_swap_and_conflicting_event() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let first = root.join("first");
        let second = root.join("second");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let mut calls = 0;
        let mut collision = None;
        let error = rename_paths_impl(
            &[
                (first.clone(), second.clone()),
                (second.clone(), first.clone()),
            ],
            &CancellationToken::new(),
            |from, to| {
                rename_noreplace(from, to)?;
                calls += 1;
                if calls == 4 {
                    let path = crate::rename_journal::event_path(from, 2);
                    std::fs::write(&path, b"competing event").unwrap();
                    collision = Some(path);
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            error.contains("Renames installed but progress recording failed"),
            "{error}"
        );
        assert_eq!(std::fs::read(first).unwrap(), b"second");
        assert_eq!(std::fs::read(second).unwrap(), b"first");
        assert_eq!(
            std::fs::read(collision.unwrap()).unwrap(),
            b"competing event"
        );
        let report = crate::discover_recovery_report(&root).unwrap();
        assert_eq!(report.rename_intents.len(), 2);
        assert_eq!(report.issues.len(), 1);
        assert!(
            report
                .rename_intents
                .iter()
                .any(|item| item.last_recorded_phase.is_none())
        );
    }

    #[test]
    fn rename_crash_worker() {
        let Some(root) = std::env::var_os("GRAVITYFILE_RENAME_CRASH_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let phase: usize = std::env::var("GRAVITYFILE_RENAME_CRASH_PHASE")
            .unwrap()
            .parse()
            .unwrap();
        let first = root.join("first-é");
        let second = root.join("second");
        let mut calls = 0;
        let _ = rename_paths_impl(
            &[(first.clone(), second.clone()), (second, first)],
            &CancellationToken::new(),
            |from, to| {
                if phase == 0 {
                    std::process::exit(77);
                }
                rename_noreplace(from, to)?;
                calls += 1;
                if calls == phase {
                    std::process::exit(77);
                }
                Ok(())
            },
        );
        panic!("crash phase not reached");
    }

    #[test]
    fn process_exit_preserves_rename_intent_and_all_payloads() {
        fn native_path(value: &serde_json::Value) -> PathBuf {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                let bytes: Vec<u8> = serde_json::from_value(value["bytes"].clone()).unwrap();
                std::ffi::OsString::from_vec(bytes).into()
            }
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;
                let wide: Vec<u16> = serde_json::from_value(value["wide"].clone()).unwrap();
                std::ffi::OsString::from_wide(&wide).into()
            }
        }
        for phase in 0..=4 {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let first = root.join("first-é");
            let second = root.join("second");
            std::fs::write(&first, b"first").unwrap();
            std::fs::write(&second, b"second").unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "rename::tests::rename_crash_worker",
                    "--nocapture",
                ])
                .env("GRAVITYFILE_RENAME_CRASH_ROOT", &root)
                .env("GRAVITYFILE_RENAME_CRASH_PHASE", phase.to_string())
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(77),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let staging: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".gravityfile-rename-")
                })
                .collect();
            assert_eq!(staging.len(), 1);
            let mut paths = HashSet::from([first.clone(), second.clone()]);
            let mut intents = 0;
            for entry in std::fs::read_dir(staging[0].path())
                .unwrap()
                .map(Result::unwrap)
            {
                if entry.path().extension().is_some_and(|ext| ext == "json") {
                    let intent: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
                    assert_eq!(intent["schema_version"], 1);
                    assert_eq!(intent["purpose"], "bulk_rename_inspection_only");
                    assert!(
                        intent["outcome"]
                            .as_str()
                            .unwrap()
                            .starts_with("unverified")
                    );
                    let source = native_path(&intent["source"]);
                    let target = native_path(&intent["destination"]);
                    assert!(source == first || source == second);
                    assert_eq!(
                        target,
                        if source == first {
                            second.clone()
                        } else {
                            first.clone()
                        }
                    );
                    let staged = native_path(&intent["staged"]);
                    assert_eq!(staged.parent().unwrap(), staging[0].path());
                    paths.insert(staged);
                    intents += 1;
                }
            }
            assert_eq!(intents, 2);
            let discovered = crate::discover_recovery_report(&root).unwrap();
            assert!(discovered.issues.is_empty(), "{:?}", discovered.issues);
            assert_eq!(discovered.rename_intents.len(), intents);
            assert!(
                discovered
                    .rename_intents
                    .iter()
                    .all(|intent| intent.locked_during_inspection)
            );
            assert!(discovered.records.is_empty());
            let mut json = Vec::new();
            discovered.write_json(&root, &mut json).unwrap();
            let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(json["rename_intents"].as_array().unwrap().len(), intents);
            assert_eq!(json["status"]["state"], "complete");
            let mut contents: Vec<_> = paths
                .iter()
                .filter(|path| path.exists())
                .map(|path| std::fs::read(path).unwrap())
                .collect();
            contents.sort();
            assert_eq!(contents, vec![b"first".to_vec(), b"second".to_vec()]);
        }
    }

    #[test]
    fn windows_device_names_include_superscript_digits_and_extensions() {
        for base in [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM9", "LPT1", "LPT9", "COM¹", "COM²", "COM³",
            "LPT¹", "LPT²", "LPT³",
        ] {
            for suffix in ["", ".txt", ".tar.gz"] {
                for name in [
                    format!("{base}{suffix}"),
                    format!("{}{suffix}", base.to_lowercase()),
                ] {
                    assert!(validate_windows_name(&name).is_err(), "accepted {name}");
                }
            }
        }
        for name in [
            "COM0",
            "COM10",
            "LPT10",
            "COM¹0",
            "xCON",
            ".CON",
            "NULL",
            "report.txt",
        ] {
            assert!(validate_windows_name(name).is_ok(), "rejected {name}");
        }
        for character in ['\\', ':', '*', '?', '"', '<', '>', '|'] {
            assert!(validate_windows_name(&format!("a{character}b")).is_err());
        }
        #[cfg(not(target_os = "windows"))]
        assert!(
            validate_filename("COM¹.txt").is_ok(),
            "Windows device rules must not leak onto Unix"
        );
    }

    #[tokio::test]
    async fn closed_rename_receiver_preserves_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::write(&source, b"keep").unwrap();
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        rename_impl(source.clone(), "destination".into(), tx).await;
        assert_eq!(std::fs::read(source).unwrap(), b"keep");
        assert!(!temp.path().join("destination").exists());
    }

    #[tokio::test]
    async fn full_progress_queue_does_not_stall_rename() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::write(&source, b"keep").unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(RenameResult::Progress(OperationProgress::new(
            OperationType::Rename,
            1,
            0,
        )))
        .unwrap();
        let work = tokio::spawn(rename_impl(source.clone(), "destination".into(), tx));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !destination.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("progress queue stalled rename");
        rx.recv().await.unwrap();
        let RenameResult::Complete(result) = rx.recv().await.unwrap() else {
            panic!("completion expected")
        };
        assert!(result.is_success());
        assert_eq!(result.succeeded, 1);
        assert_eq!(std::fs::read(destination).unwrap(), b"keep");
        assert!(!source.exists());
        work.await.unwrap();
    }

    #[tokio::test]
    async fn same_name_rename_validates_source_without_claiming_a_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        for present in [false, true] {
            if present {
                std::fs::write(&source, b"keep").unwrap();
            }
            let mut rx = start_rename(source.clone(), "source".into());
            loop {
                let result = rx.recv().await.expect("rename completion");
                if let RenameResult::Complete(result) = result {
                    assert_eq!(result.is_success(), present);
                    assert_eq!(result.succeeded, 0);
                    assert_eq!(result.failed, usize::from(!present));
                    if !present {
                        assert_eq!(result.errors[0].path, source);
                    }
                    break;
                }
            }
        }
        assert_eq!(std::fs::read(source).unwrap(), b"keep");
    }

    #[test]
    fn test_validate_filename_valid() {
        assert!(validate_filename("test.txt").is_ok());
        assert!(validate_filename("my-file").is_ok());
        assert!(validate_filename(".hidden").is_ok());
        assert!(validate_filename("file with spaces").is_ok());
    }

    #[test]
    fn test_validate_filename_invalid() {
        assert!(validate_filename("").is_err());
        assert!(validate_filename("test/file").is_err());
        assert!(validate_filename(".").is_err());
        assert!(validate_filename("..").is_err());
        assert!(validate_filename("file ").is_err());
        assert!(validate_filename(" file").is_err());
        assert!(validate_filename("file.").is_err());
    }

    #[test]
    fn test_validate_filename_rejects_control_chars() {
        assert!(validate_filename("a\nb").is_err());
        assert!(validate_filename("a\tb").is_err());
        assert!(validate_filename("a\u{7f}b").is_err());
        assert!(validate_filename("a\u{1b}[31m").is_err());
    }

    #[test]
    fn cancellation_rolls_back_each_partial_phase_but_not_a_committed_swap() {
        // Two staging renames followed by two installations. Cancellation
        // after the final installation is past the commit boundary.
        for cancel_after in 0..=4 {
            let directory = tempfile::tempdir().unwrap();
            let first = directory.path().join("first");
            let second = directory.path().join("second");
            std::fs::write(&first, b"first").unwrap();
            std::fs::write(&second, b"second").unwrap();
            let token = CancellationToken::new();
            if cancel_after == 0 {
                token.cancel();
            }
            let mut calls = 0;
            let result = rename_paths_impl(
                &[
                    (first.clone(), second.clone()),
                    (second.clone(), first.clone()),
                ],
                &token,
                |source, target| {
                    rename_noreplace(source, target)?;
                    calls += 1;
                    if calls == cancel_after {
                        token.cancel();
                    }
                    Ok(())
                },
            );
            if cancel_after < 4 {
                assert!(result.unwrap_err().contains(crate::copy::CANCELLED_MESSAGE));
                assert_eq!(std::fs::read(&first).unwrap(), b"first");
                assert_eq!(std::fs::read(&second).unwrap(), b"second");
            } else {
                result.unwrap();
                assert_eq!(std::fs::read(&first).unwrap(), b"second");
                assert_eq!(std::fs::read(&second).unwrap(), b"first");
            }
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        }
    }

    #[test]
    fn cancellation_reports_blocked_rollback_without_clobbering() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        std::fs::write(&source, b"payload").unwrap();
        let token = CancellationToken::new();
        let mut staged_path = None;
        let error = rename_paths_impl(&[(source.clone(), target.clone())], &token, |from, to| {
            rename_noreplace(from, to)?;
            staged_path = Some(to.to_path_buf());
            std::fs::write(from, b"concurrent writer").unwrap();
            token.cancel();
            Ok(())
        })
        .unwrap_err();
        let staged_path = staged_path.unwrap();
        assert!(error.contains("rollback errors"));
        assert!(error.contains(&staged_path.display().to_string()));
        assert_eq!(std::fs::read(&source).unwrap(), b"concurrent writer");
        assert_eq!(std::fs::read(staged_path).unwrap(), b"payload");
        assert!(!target.exists());
    }

    #[test]
    fn private_staging_is_shared_per_parent_and_removed_after_commit() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let mut reserved = Vec::new();
        let mut calls = 0;
        rename_paths_impl(
            &[
                (first.clone(), second.clone()),
                (second.clone(), first.clone()),
            ],
            &CancellationToken::new(),
            |source, target| {
                if calls < 2 {
                    let parent = target.parent().unwrap();
                    assert!(
                        parent
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(".gravityfile-rename-")
                    );
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        assert_eq!(
                            std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
                            0o700
                        );
                    }
                    reserved.push(parent.to_path_buf());
                }
                calls += 1;
                rename_noreplace(source, target)
            },
        )
        .unwrap();
        assert_eq!(reserved.len(), 2);
        assert_eq!(reserved[0], reserved[1]);
        assert!(!reserved[0].exists());
        assert_eq!(std::fs::read(first).unwrap(), b"second");
        assert_eq!(std::fs::read(second).unwrap(), b"first");
    }

    #[test]
    fn recovery_inspection_excludes_a_live_rename_batch() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        std::fs::write(&source, b"payload").unwrap();
        let mut calls = 0;
        rename_paths_impl(
            &[(source.clone(), target.clone())],
            &CancellationToken::new(),
            |from, to| {
                let report = crate::discover_recovery_report(directory.path()).unwrap();
                assert!(report.rename_intents.is_empty());
                assert_eq!(report.issues.len(), 1);
                assert_eq!(
                    report.issues[0].error.kind(),
                    std::io::ErrorKind::WouldBlock
                );
                assert!(
                    report.issues[0]
                        .error
                        .to_string()
                        .contains("record is active")
                );
                calls += 1;
                rename_noreplace(from, to)
            },
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(std::fs::read(target).unwrap(), b"payload");
        let report = crate::discover_recovery_report(directory.path()).unwrap();
        assert!(report.rename_intents.is_empty());
        assert!(report.issues.is_empty());
    }

    #[test]
    fn new_intent_cannot_be_inspected_without_its_required_lock() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let source = root.join("source");
        std::fs::write(&source, b"source").unwrap();
        let destination = root.join("destination");
        let mut staging = RenameStaging::default();
        let payload = staging.payload(&source, 0).unwrap();
        staging.record(&source, &payload, &destination).unwrap();
        staging.cleanup_safe = false;
        drop(staging);
        let report = crate::discover_recovery_report(&root).unwrap();
        assert_eq!(report.rename_intents.len(), 1);
        assert!(report.rename_intents[0].locked_during_inspection);
        std::fs::remove_file(payload.parent().unwrap().join("lock")).unwrap();
        let report = crate::discover_recovery_report(&root).unwrap();
        assert!(report.rename_intents.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert!(
            report.issues[0]
                .error
                .to_string()
                .contains("missing transaction lock")
        );
        assert!(payload.with_extension("json").exists());
    }

    #[test]
    fn staging_cleanup_preserves_payload_when_unwinding() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        std::fs::write(&source, b"must survive").unwrap();
        let mut retained = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = rename_paths_impl(
                &[(source.clone(), target.clone())],
                &CancellationToken::new(),
                |from, to| {
                    rename_noreplace(from, to)?;
                    retained = Some(to.to_path_buf());
                    panic!("injected failure after staging");
                },
            );
        }));
        assert!(result.is_err());
        assert!(!source.exists());
        assert!(!target.exists());
        let retained = retained.unwrap();
        assert_eq!(std::fs::read(&retained).unwrap(), b"must survive");
        rename_noreplace(&retained, &source).unwrap();
        assert_eq!(std::fs::read(source).unwrap(), b"must survive");
    }

    #[test]
    fn transactional_rename_validates_no_op_entries_before_filtering() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        std::fs::write(&source, b"original").unwrap();
        let error = rename_paths_transactional(&[
            (source.clone(), source.clone()),
            (source.clone(), target.clone()),
        ])
        .unwrap_err();
        assert!(error.contains("same source"));
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
        assert!(!target.exists());
        let missing = directory.path().join("missing");
        assert!(rename_paths_transactional(&[(missing.clone(), missing)]).is_err());
    }

    #[test]
    fn transactional_rename_rejects_nested_sources_and_destinations_before_staging() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent");
        let child = parent.join("child");
        let other = directory.path().join("other");
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&child, b"child").unwrap();
        std::fs::write(&other, b"other").unwrap();
        for pair in [
            (child.clone(), directory.path().join("renamed-child")),
            (other.clone(), parent.join("incoming")),
        ] {
            let error = rename_paths_transactional(&[
                (parent.clone(), directory.path().join("renamed-parent")),
                pair,
            ])
            .unwrap_err();
            assert!(error.contains("beneath source"));
            assert_eq!(std::fs::read(&child).unwrap(), b"child");
            assert_eq!(std::fs::read(&other).unwrap(), b"other");
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        }
    }

    #[cfg(unix)]
    #[test]
    fn transactional_rename_resolves_parent_aliases_but_preserves_leaf_symlinks() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent");
        let alias = directory.path().join("alias");
        std::fs::create_dir(&parent).unwrap();
        symlink(&parent, &alias).unwrap();
        let source = parent.join("source");
        std::fs::write(&source, b"original").unwrap();
        assert!(
            rename_paths_transactional(&[
                (source.clone(), parent.join("one")),
                (alias.join("source"), parent.join("two")),
            ])
            .unwrap_err()
            .contains("same source")
        );
        let link = parent.join("link");
        symlink(&source, &link).unwrap();
        let renamed_link = alias.join("renamed-link");
        rename_paths_transactional(&[(link.clone(), renamed_link.clone())]).unwrap();
        assert_eq!(std::fs::read_link(renamed_link).unwrap(), source);
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
        assert!(std::fs::symlink_metadata(link).is_err());
    }

    #[test]
    fn rollback_does_not_restore_an_occupied_staging_path() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staged = directory.path().join("staged");
        let target = directory.path().join("target");
        std::fs::write(&target, b"installed payload").unwrap();
        std::fs::write(&staged, b"concurrent writer").unwrap();
        let errors =
            rollback_staged_renames(&[(source.clone(), staged.clone(), target.clone())], 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("failed to restage"));
        assert!(!source.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"installed payload");
        assert_eq!(std::fs::read(&staged).unwrap(), b"concurrent writer");
    }

    #[test]
    fn rollback_reports_missing_payload_and_continues_other_restores() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let staged = directory.path().join("staged");
        let missing = directory.path().join("missing");
        std::fs::write(&staged, b"recoverable").unwrap();
        let errors = rollback_staged_renames(
            &[
                (
                    source.clone(),
                    staged.clone(),
                    directory.path().join("target"),
                ),
                (
                    directory.path().join("other-source"),
                    missing.clone(),
                    directory.path().join("other-target"),
                ),
            ],
            0,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(&missing.display().to_string()));
        assert_eq!(std::fs::read(source).unwrap(), b"recoverable");
        assert!(!staged.exists());
    }

    #[test]
    fn rollback_restores_a_partially_installed_cycle() {
        let directory = tempfile::tempdir().unwrap();
        let sources: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|name| directory.path().join(name))
            .collect();
        let staged: Vec<_> = (0..3)
            .map(|index| {
                (
                    sources[index].clone(),
                    directory.path().join(format!("staged-{index}")),
                    sources[(index + 1) % 3].clone(),
                )
            })
            .collect();
        for (index, (_, temporary, _)) in staged.iter().enumerate() {
            std::fs::write(temporary, [index as u8]).unwrap();
        }
        for (_, temporary, target) in &staged[..2] {
            rename_noreplace(temporary, target).unwrap();
        }
        assert!(rollback_staged_renames(&staged, 2).is_empty());
        for (index, (source, temporary, _)) in staged.iter().enumerate() {
            assert_eq!(std::fs::read(source).unwrap(), [index as u8]);
            assert!(!temporary.exists());
        }
    }

    #[test]
    fn transactional_rename_supports_swaps() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, "second").unwrap();

        rename_paths_transactional(&[
            (first.clone(), second.clone()),
            (second.clone(), first.clone()),
        ])
        .unwrap();

        assert_eq!(std::fs::read_to_string(first).unwrap(), "second");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "first");
    }

    #[test]
    fn transactional_rename_rejects_external_conflicts_without_changes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let conflict = temp.path().join("conflict.txt");
        std::fs::write(&source, "source").unwrap();
        std::fs::write(&conflict, "existing").unwrap();

        let result = rename_paths_transactional(&[(source.clone(), conflict.clone())]);

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "source");
        assert_eq!(std::fs::read_to_string(conflict).unwrap(), "existing");
    }
}
