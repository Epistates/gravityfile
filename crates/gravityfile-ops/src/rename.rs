//! Single and bulk rename.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::fs::{CANCELLED, rename_noreplace, same_object};
use crate::progress::{OperationComplete, OperationProgress, OperationType};
use crate::{OPERATION_CHANNEL_SIZE, OperationError};

/// Message sent while a rename runs.
#[derive(Debug)]
pub enum RenameResult {
    Progress(OperationProgress),
    Complete(OperationComplete),
}

/// Rename `source` to `new_name` within its directory.
pub fn start_rename(source: PathBuf, new_name: String) -> mpsc::Receiver<RenameResult> {
    let (tx, rx) = mpsc::channel(OPERATION_CHANNEL_SIZE);
    tokio::spawn(async move {
        let mut progress = OperationProgress::new(OperationType::Rename, 1, 0);
        progress.set_current_file(Some(source.clone()));
        let _ = tx.try_send(RenameResult::Progress(progress));
        let token = CancellationToken::new();
        let work_source = source.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<bool, String> {
            validate_filename(&new_name)?;
            let target = crate::fs::parent_or_current(&work_source).join(&new_name);
            if target == work_source {
                fs::symlink_metadata(&work_source).map_err(|error| error.to_string())?;
                return Ok(false);
            }
            rename_paths(&[(work_source, target)], &token)?;
            Ok(true)
        })
        .await
        .unwrap_or_else(|error| Err(format!("worker failed: {error}")));
        let (succeeded, failed, errors) = match result {
            Ok(changed) => (usize::from(changed), 0, Vec::new()),
            Err(error) => (0, 1, vec![OperationError::new(source, error)]),
        };
        let _ = tx
            .send(RenameResult::Complete(OperationComplete {
                operation_type: OperationType::Rename,
                cancelled: false,
                succeeded,
                failed,
                skipped: 0,
                bytes_processed: 0,
                errors,
            }))
            .await;
    });
    rx
}

/// Rename a set of paths as one unit, including swaps (`a→b`, `b→a`), cycles
/// and case-only changes on case-insensitive filesystems.
///
/// Every source is first moved to a private temporary name in its own
/// directory, then each temporary is published at its target, both without
/// replacing anything. A failure or cancellation before the last publication
/// moves everything back. Targets must be in existing directories; a target
/// that exists and is not itself being renamed is rejected up front.
pub fn rename_paths(
    renames: &[(PathBuf, PathBuf)],
    token: &CancellationToken,
) -> Result<(), String> {
    let renames = validate_batch(renames)?;
    if renames.is_empty() {
        return Ok(());
    }
    let nonce = std::process::id()
        ^ (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0));

    // Phase 1: sources to temporary names.
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::with_capacity(renames.len());
    for (index, (source, target)) in renames.into_iter().enumerate() {
        let temporary = crate::fs::parent_or_current(&source)
            .join(format!(".gravityfile-rename-{nonce:08x}-{index}"));
        let result = if token.is_cancelled() {
            Err(CANCELLED.to_string())
        } else {
            rename_noreplace(&source, &temporary)
                .map_err(|error| format!("cannot rename '{}': {error}", source.display()))
        };
        if let Err(error) = result {
            return Err(with_rollback(error, rollback(&staged, 0)));
        }
        staged.push((source, temporary, target));
    }

    // Phase 2: temporaries to targets. Cancellation is honored until the
    // final publication, which commits the batch.
    for index in 0..staged.len() {
        let (_, temporary, target) = &staged[index];
        let result = if token.is_cancelled() {
            Err(CANCELLED.to_string())
        } else {
            rename_noreplace(temporary, target)
                .map_err(|error| format!("cannot rename to '{}': {error}", target.display()))
        };
        if let Err(error) = result {
            return Err(with_rollback(error, rollback(&staged, index)));
        }
    }
    Ok(())
}

/// Undo `published` phase-2 renames, then every phase-1 rename.
fn rollback(staged: &[(PathBuf, PathBuf, PathBuf)], published: usize) -> Vec<String> {
    let mut errors = Vec::new();
    let mut restorable = vec![true; staged.len()];
    for (index, (_, temporary, target)) in staged[..published].iter().enumerate().rev() {
        if let Err(error) = rename_noreplace(target, temporary) {
            restorable[index] = false;
            errors.push(format!("'{}' left in place: {error}", target.display()));
        }
    }
    for (index, (source, temporary, _)) in staged.iter().enumerate().rev() {
        if restorable[index]
            && let Err(error) = rename_noreplace(temporary, source)
        {
            errors.push(format!(
                "'{}' left at '{}': {error}",
                source.display(),
                temporary.display()
            ));
        }
    }
    errors
}

fn with_rollback(error: String, rollback_errors: Vec<String>) -> String {
    if rollback_errors.is_empty() {
        error
    } else {
        format!(
            "{error}; rollback incomplete: {}",
            rollback_errors.join("; ")
        )
    }
}

/// Resolve parents (not leaves: renaming a symlink renames the link), drop
/// no-op entries and reject duplicates, nesting and external conflicts.
fn validate_batch(renames: &[(PathBuf, PathBuf)]) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let resolve = |path: &Path| -> Result<PathBuf, String> {
        let name = path
            .file_name()
            .ok_or_else(|| format!("'{}' has no file name", path.display()))?;
        let parent = dunce::canonicalize(crate::fs::parent_or_current(path)).map_err(|error| {
            format!(
                "cannot resolve the directory of '{}': {error}",
                path.display()
            )
        })?;
        Ok(parent.join(name))
    };
    let mut resolved = Vec::with_capacity(renames.len());
    for (source, target) in renames {
        let (source, target) = (resolve(source)?, resolve(target)?);
        fs::symlink_metadata(&source)
            .map_err(|error| format!("cannot access '{}': {error}", source.display()))?;
        if source != target {
            resolved.push((source, target));
        }
    }
    let sources: HashSet<&Path> = resolved
        .iter()
        .map(|(source, _)| source.as_path())
        .collect();
    let targets: HashSet<&Path> = resolved
        .iter()
        .map(|(_, target)| target.as_path())
        .collect();
    if sources.len() != resolved.len() {
        return Err("the same item is renamed more than once".into());
    }
    if targets.len() != resolved.len() {
        return Err("two items would receive the same name".into());
    }
    for (source, target) in &resolved {
        for path in [source, target] {
            if let Some(ancestor) = path.ancestors().skip(1).find(|a| sources.contains(a)) {
                return Err(format!(
                    "'{}' is inside '{}', which is also being renamed",
                    path.display(),
                    ancestor.display()
                ));
            }
        }
        // A case-only rename finds its own source at the target on
        // case-insensitive filesystems; that is not a conflict.
        if fs::symlink_metadata(target).is_ok()
            && !sources.contains(target.as_path())
            && !same_object(source, target)
        {
            return Err(format!("'{}' already exists", target.display()));
        }
    }
    Ok(resolved)
}

/// Validate a new file name against portable rules.
pub fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty".into());
    }
    if name.len() > 255 {
        return Err("Name is too long (max 255 bytes)".into());
    }
    for c in ['/', '\0'] {
        if name.contains(c) {
            return Err(format!("Name cannot contain '{c}'"));
        }
    }
    if name.chars().any(char::is_control) {
        return Err("Name cannot contain control characters".into());
    }
    #[cfg(windows)]
    validate_windows_name(name)?;
    if name.starts_with(' ') || name.ends_with(' ') {
        return Err("Name cannot start or end with spaces".into());
    }
    if name.ends_with('.') {
        return Err("Name cannot end with a dot".into());
    }
    if name == "." || name == ".." {
        return Err("'.' and '..' are reserved names".into());
    }
    Ok(())
}

/// Win32 naming rules, host-testable on every platform.
#[cfg(any(windows, test))]
fn validate_windows_name(name: &str) -> Result<(), String> {
    for c in ['\\', ':', '*', '?', '"', '<', '>', '|'] {
        if name.contains(c) {
            return Err(format!("Name cannot contain '{c}'"));
        }
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
        "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
    ];
    let upper = name.to_uppercase();
    if RESERVED.contains(&upper.split('.').next().unwrap_or("")) {
        return Err("Reserved filename".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn swaps_and_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let [a, b, c] = ["a", "b", "c"].map(|name| dir.path().join(name));
        for path in [&a, &b, &c] {
            fs::write(path, path.file_name().unwrap().to_str().unwrap()).unwrap();
        }
        rename_paths(
            &[
                (a.clone(), b.clone()),
                (b.clone(), c.clone()),
                (c.clone(), a.clone()),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            (read(&a), read(&b), read(&c)),
            ("c".into(), "a".into(), "b".into())
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn external_conflicts_change_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let [a, b, taken] = ["a", "b", "taken"].map(|name| dir.path().join(name));
        for path in [&a, &b, &taken] {
            fs::write(path, "x").unwrap();
        }
        let error = rename_paths(
            &[
                (a.clone(), dir.path().join("a2")),
                (b.clone(), taken.clone()),
            ],
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert!(a.exists() && b.exists() && !dir.path().join("a2").exists());
    }

    #[test]
    fn duplicate_and_nested_entries_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("p");
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("child"), "x").unwrap();
        let token = CancellationToken::new();
        assert!(
            rename_paths(
                &[
                    (parent.clone(), dir.path().join("q")),
                    (parent.join("child"), parent.join("c2"))
                ],
                &token
            )
            .unwrap_err()
            .contains("inside")
        );
        let file = parent.join("child");
        assert!(
            rename_paths(
                &[(file.clone(), parent.join("x")), (file, parent.join("y"))],
                &token
            )
            .unwrap_err()
            .contains("more than once")
        );
    }

    #[test]
    fn cancellation_restores_every_source() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        fs::write(&a, "a").unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(rename_paths(&[(a.clone(), dir.path().join("b"))], &token).is_err());
        assert_eq!(read(&a), "a");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn case_only_rename() {
        let dir = tempfile::tempdir().unwrap();
        let upper = dir.path().join("Readme.md");
        fs::write(&upper, "x").unwrap();
        rename_paths(
            &[(upper, dir.path().join("README.md"))],
            &CancellationToken::new(),
        )
        .unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("README.md")]);
    }

    #[cfg(unix)]
    #[test]
    fn renames_symlinks_not_their_targets() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("target"), "t").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("target", &link).unwrap();
        rename_paths(
            &[(link, dir.path().join("renamed"))],
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(
            fs::symlink_metadata(dir.path().join("renamed"))
                .unwrap()
                .is_symlink()
        );
        assert!(dir.path().join("target").is_file());
    }

    #[tokio::test]
    async fn start_rename_reports_conflicts_and_no_ops() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        fs::write(&a, "a").unwrap();
        fs::write(dir.path().join("b"), "b").unwrap();
        let complete = |mut rx: mpsc::Receiver<RenameResult>| async move {
            loop {
                if let Some(RenameResult::Complete(complete)) = rx.recv().await {
                    return complete;
                }
            }
        };
        let result = complete(start_rename(a.clone(), "b".into())).await;
        assert_eq!(result.failed, 1);
        let result = complete(start_rename(a.clone(), "a".into())).await;
        assert_eq!((result.succeeded, result.failed), (0, 0));
        let result = complete(start_rename(a, "c".into())).await;
        assert_eq!(result.succeeded, 1);
        assert!(dir.path().join("c").exists());
    }

    #[test]
    fn filename_validation() {
        for valid in ["file.txt", "my file", ".hidden", "日本語"] {
            assert!(validate_filename(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "", ".", "..", "a/b", "a\0b", " lead", "trail ", "dot.", "tab\t",
        ] {
            assert!(validate_filename(invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_filename(&"x".repeat(256)).is_err());
        for reserved in ["CON", "con.txt", "COM¹", "LPT9.log", "a:b"] {
            assert!(validate_windows_name(reserved).is_err(), "{reserved}");
        }
    }
}
