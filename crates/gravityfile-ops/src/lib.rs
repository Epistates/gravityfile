//! File operations for gravityfile: copy, move, rename, create, delete,
//! undo and archives.
//!
//! Long-running operations run on blocking threads and report through a
//! channel: best-effort [`OperationProgress`] snapshots, interactive
//! [`ConflictRequest`]s, and one reliable completion message. Dropping the
//! receiver cancels the operation.
//!
//! # Safety model
//!
//! - New content is assembled in a private staging directory beside its
//!   destination and published with a single no-clobber rename, so a failed
//!   or cancelled operation never leaves a partial item at a visible path and
//!   never silently replaces anything.
//! - Overwrites the user approves move the replaced item to the system Trash
//!   (or delete it permanently, if configured) only after the replacement is
//!   complete.
//! - Copies skip entries they cannot read and report each one instead of
//!   discarding everything that did copy; cross-filesystem moves keep the
//!   source unless every entry arrived.

pub mod archive;
mod batch;
mod conflict;
mod create;
mod delete;
mod error;
mod fs;
mod progress;
mod rename;
mod transfer;
mod undo;

pub use archive::{
    ArchiveError, ArchiveFormat, ArchiveResult, ExtractLimits, create_archive,
    create_archive_cancellable, extract_archive, extract_archive_cancellable, extract_archive_with,
};
pub use batch::{TransferComplete, TransferOptions, TransferResult, start_copy, start_move};
pub use conflict::{Conflict, ConflictKind, ConflictRequest, ConflictResolution, auto_rename_path};
pub use create::{CreateResult, start_create_directory, start_create_file};
pub use delete::{DeleteComplete, DeleteMode, DeleteResult, start_delete};
pub use error::OperationError;
pub use fs::STAGING_PREFIX;
pub use progress::{OperationComplete, OperationProgress, OperationType};
pub use rename::{RenameResult, rename_paths, start_rename, validate_filename};
pub use tokio_util::sync::CancellationToken;
pub use transfer::ReplacePolicy;
pub use undo::{UndoComplete, UndoEntry, UndoLog, UndoResult, UndoableOperation, execute_undo};

/// Channel capacity for operation messages.
pub const OPERATION_CHANNEL_SIZE: usize = 100;
