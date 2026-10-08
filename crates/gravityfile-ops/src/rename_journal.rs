//! Bounded immutable progress records. Historical evidence, never a replay plan.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Last recorded bulk-rename phase, not the current location or identity of data.
/// Pending phases deliberately allow an ambiguous result after process death.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RenamePhase {
    /// Intent was synced before moving the source into staging.
    StagePending,
    /// The staging rename and its containing directories were synced.
    Staged,
    /// Installation was recorded before moving the staged item.
    InstallPending,
    /// The destination rename and its containing directories were synced.
    Installed,
    /// Rollback was recorded before moving an installed item back to staging.
    RestagePending,
    /// Rollback moved an installed item back to staging and synced directories.
    Restaged,
    /// Restoration was recorded before moving a staged item back to its source.
    RestorePending,
    /// Restoration moved the item to its source and synced directories.
    Restored,
}

impl RenamePhase {
    pub(crate) fn follows(self, previous: Self) -> bool {
        use RenamePhase::*;
        matches!(
            (previous, self),
            (StagePending, Staged | RestorePending)
                | (Staged, InstallPending | RestorePending)
                | (InstallPending, Installed | RestagePending | RestorePending)
                | (Installed, RestagePending)
                | (RestagePending, Restaged)
                | (Restaged, RestorePending)
                | (RestorePending, Restored)
        )
    }
}

pub(crate) const MAX_EVENTS: usize = 7;
pub(crate) const MAX_EVENT_BYTES: u64 = 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    pub(crate) schema_version: u32,
    pub(crate) sequence: usize,
    pub(crate) phase: RenamePhase,
}

pub(crate) fn event_path(payload: &Path, sequence: usize) -> PathBuf {
    payload.with_extension(format!("{sequence:02}.event"))
}

pub(crate) fn write(payload: &Path, sequence: usize, phase: RenamePhase) -> io::Result<PathBuf> {
    if sequence >= MAX_EVENTS {
        return Err(io::Error::other("rename progress event limit reached"));
    }
    let path = event_path(payload, sequence);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    let bytes = serde_json::to_vec(&Event {
        schema_version: 1,
        sequence,
        phase,
    })
    .map_err(io::Error::other)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    crate::recovery::sync_directory(payload.parent().expect("staged payload parent"))?;
    Ok(path)
}

pub(crate) fn inspect(
    payload: &Path,
    paths: &mut [PathBuf],
    budget: &mut crate::recovery::DiscoveryBudget,
) -> io::Result<RenamePhase> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid rename progress chain");
    if paths.len() > MAX_EVENTS {
        return Err(invalid());
    }
    paths.sort();
    let mut previous = RenamePhase::StagePending;
    for (sequence, path) in paths.iter().enumerate() {
        if *path != event_path(payload, sequence) {
            return Err(invalid());
        }
        let bytes = budget.read(path, MAX_EVENT_BYTES)?;
        let event: Event = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if event.schema_version != 1 || event.sequence != sequence || !event.phase.follows(previous)
        {
            return Err(invalid());
        }
        previous = event.phase;
    }
    Ok(previous)
}
