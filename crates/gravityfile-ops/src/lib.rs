//! File operations engine for gravityfile.
//!
//! This crate provides async file operations (copy, move, rename, create, delete)
//! with progress reporting via channels, following the same pattern as the
//! existing deletion implementation.

mod conflict;
mod copy;
mod create;
mod executor;
mod move_op;
mod operation;
mod progress;
mod rename;
mod undo;

pub use conflict::{Conflict, ConflictKind, ConflictResolution};
pub use copy::{start_copy, CopyOptions, CopyResult};
pub use create::{start_create_directory, start_create_file, CreateResult};
pub use executor::{execute_undo, OperationExecutor, OperationResult};
pub use move_op::{start_move, MoveOptions, MoveResult};
pub use operation::{FileOperation, OperationError};
pub use progress::{OperationComplete, OperationProgress, OperationType};
pub use rename::{start_rename, RenameResult};
pub use undo::{UndoEntry, UndoLog, UndoableOperation};

/// Default channel buffer size for operation progress updates.
pub const OPERATION_CHANNEL_SIZE: usize = 100;
