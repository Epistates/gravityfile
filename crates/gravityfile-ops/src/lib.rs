//! File operations engine for gravityfile.
//!
//! This crate provides async file operations (copy, move, rename, create, delete)
//! with progress reporting via channels, following the same pattern as the
//! existing deletion implementation.

pub mod archive;
mod conflict;
mod copy;
mod create;
mod executor;
mod move_op;
mod operation;
mod progress;
mod rename;
mod undo;

pub use archive::{ArchiveError, ArchiveFormat, ArchiveResult, create_archive, extract_archive};
pub use conflict::{Conflict, ConflictKind, ConflictResolution};
pub use copy::{CopyOptions, CopyResult, start_copy};
pub use create::{CreateResult, start_create_directory, start_create_file};
pub use executor::{OperationExecutor, OperationResult, execute_undo};
pub use move_op::{MoveComplete, MoveOptions, MoveResult, start_move};
pub use operation::{FileOperation, OperationError};
pub use progress::{OperationComplete, OperationProgress, OperationType};
pub use rename::{RenameResult, start_rename};
pub use tokio_util::sync::CancellationToken;
pub use undo::{UndoEntry, UndoLog, UndoableOperation};

/// Default channel buffer size for operation progress updates.
pub const OPERATION_CHANNEL_SIZE: usize = 100;
