#![doc = include_str!("../README.md")]

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
