//! File operations engine for gravityfile.
//!
//! This crate provides async file operations (copy, move, rename, create, delete)
//! with progress reporting via channels, following the same pattern as the
//! existing deletion implementation.

pub mod archive;
mod archive_journal;
mod conflict;
mod copy;
#[cfg(test)]
mod copy_bench;
mod create;
mod executor;
mod move_op;
mod native_path;
mod object_identity;
mod operation;
mod progress;
mod recovery;
mod recovery_report;
mod rename;
mod rename_batch;
mod rename_journal;
mod rename_review;
mod staging;
mod undo;
mod undo_review;

pub use archive::{
    ArchiveError, ArchiveFormat, ArchiveResult, create_archive, create_archive_cancellable,
    extract_archive, extract_archive_cancellable,
};
pub use archive_journal::{ArchiveRecoveryRecord, retain_archive_extraction};
pub use conflict::{Conflict, ConflictKind, ConflictRequest, ConflictResolution};
pub use copy::{CopyOptions, CopyResult, start_copy};
pub use create::{CreateResult, start_create_directory, start_create_file};
pub use executor::{
    OperationExecutor, OperationResult, execute_undo, execute_undo_with_cancellation,
};
pub use move_op::{MoveComplete, MoveOptions, MoveResult, start_move};
pub use object_identity::{ObjectIdentity, ObjectKind};
pub use operation::{FileOperation, OperationError};
pub use progress::{OperationComplete, OperationProgress, OperationType};
pub use recovery::{
    RecoveryDiscovery, RecoveryDurabilityError, RecoveryIssue, RecoveryPurpose, RecoveryRecord,
    discover_recovery, discover_recovery_report, inspect_recovery, restore_original,
    restore_payload,
};
pub use rename::{
    RenameResult, rename_paths_transactional, rename_paths_with_cancellation, start_rename,
    validate_filename,
};
pub use rename_batch::{RenameRecovery, restore_rename_batch};
pub use rename_journal::RenamePhase;
pub use rename_review::RenameIntentRecord;
pub use tokio_util::sync::CancellationToken;
pub use undo::{UndoAttempt, UndoEntry, UndoLog, UndoableOperation};

/// Default channel buffer size for operation progress updates.
pub const OPERATION_CHANNEL_SIZE: usize = 100;
