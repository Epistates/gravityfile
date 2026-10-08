//! Archive task admission, destination handling and completion policy.
//!
//! One task owns the archive channel at a time. Publication warnings retain
//! selection and refresh the tree; refresh waits for any active mutation.

use std::path::PathBuf;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::App;

pub(super) struct ArchiveCancellation(pub(super) CancellationToken);

impl Drop for ArchiveCancellation {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(super) enum ArchiveCompletion {
    Extract {
        archive_path: PathBuf,
        destination: PathBuf,
        result: gravityfile_ops::ArchiveResult<Vec<PathBuf>>,
    },
    Compress {
        archive_path: PathBuf,
        sources: Vec<PathBuf>,
        result: gravityfile_ops::ArchiveResult<()>,
    },
}

impl App {
    pub(super) fn finish_archive_task(&mut self, completion: Option<ArchiveCompletion>) {
        self.archive_rx = None;
        self.archive_cancel = None;
        if let Some(completion) = completion {
            self.handle_archive_completion(completion);
        } else {
            if self.scan_rx.is_none() {
                self.operation_message = None;
            }
            self.error = Some(
                "Archive worker ended without a result; inspect the destination before retrying"
                    .to_string(),
            );
            // Loss of the reply says nothing about publication. Rescan without
            // replaying work or interrupting a mutation already in progress.
            self.request_archive_refresh();
        }
    }

    pub(super) fn cancel_archive_operation(&mut self) -> bool {
        let Some(cancel) = self.archive_cancel.as_ref() else {
            return false;
        };
        cancel.0.cancel();
        self.error = Some("Cancelling archive operation; waiting for completion".to_string());
        self.needs_redraw = true;
        true
    }

    pub(super) fn request_archive_refresh(&mut self) {
        self.pending_archive_refresh = true;
        if self.scan_rx.is_none() {
            self.start_scan();
        }
    }

    pub(super) fn handle_archive_completion(&mut self, completion: ArchiveCompletion) {
        if self.scan_rx.is_none() {
            self.operation_message = None;
        }
        match completion {
            ArchiveCompletion::Extract {
                archive_path,
                destination,
                result,
            } => match result {
                Ok(extracted_files) => {
                    self.error = Some(format!(
                        "Extracted {} files from {} to {}",
                        extracted_files.len(),
                        archive_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        destination.display()
                    ));
                    self.request_archive_refresh();
                }
                Err(gravityfile_ops::ArchiveError::Cancelled) => {
                    self.error =
                        Some("Archive extraction cancelled before publication".to_string());
                }
                Err(error @ gravityfile_ops::ArchiveError::ExtractionRollback { .. }) => {
                    self.error = Some(Self::sanitize_archive_error(&error));
                    self.request_archive_refresh();
                }
                Err(error) => {
                    self.error = Some(format!(
                        "Failed to extract: {}",
                        Self::sanitize_archive_error(&error)
                    ));
                }
            },
            ArchiveCompletion::Compress {
                archive_path,
                sources,
                result,
            } => match result {
                Ok(()) => {
                    self.error = Some(format!(
                        "Created archive {} with {} items",
                        archive_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        sources.len()
                    ));
                    // Remove only this operation's sources through the hash set.
                    // Scanning every mark against the source Vec is quadratic.
                    for source in &sources {
                        self.marked.remove(source);
                    }
                    self.request_archive_refresh();
                }
                Err(gravityfile_ops::ArchiveError::Cancelled) => {
                    self.error = Some("Archive creation cancelled before publication".to_string());
                }
                Err(error) => {
                    self.error = Some(format!(
                        "Failed to create archive: {}",
                        Self::sanitize_archive_error(&error)
                    ));
                }
            },
        }
    }

    /// Sanitize archive error messages to prevent path disclosure.
    fn sanitize_archive_error(err: &gravityfile_ops::ArchiveError) -> String {
        use gravityfile_ops::ArchiveError;
        match err {
            ArchiveError::ExtractionRollback { left_behind, .. } => format!(
                "Archive extraction failed; {} extracted entries could not be withdrawn, inspect the destination",
                left_behind.len()
            ),
            ArchiveError::Cancelled => "Archive operation cancelled".to_string(),
            ArchiveError::Io(e) => {
                // Extract just the error kind, not the full message which may contain paths
                let kind = e.kind();
                match kind {
                    std::io::ErrorKind::NotFound => "File not found".to_string(),
                    std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
                    std::io::ErrorKind::AlreadyExists => "File already exists".to_string(),
                    std::io::ErrorKind::InvalidData => "Invalid or corrupted archive".to_string(),
                    _ => format!("I/O error: {}", kind),
                }
            }
            ArchiveError::Zip(_) => "Invalid or corrupted ZIP archive".to_string(),
            ArchiveError::UnsupportedFormat(fmt) => format!("Unsupported format: {}", fmt),
            ArchiveError::NotFound(_) => "Archive file not found".to_string(),
            ArchiveError::DestinationExists(_) => "Destination already exists".to_string(),
            ArchiveError::DecompressionBomb(_) => {
                "Archive rejected: potential decompression bomb detected".to_string()
            }
        }
    }

    /// Extract an archive file to a destination directory.
    pub(super) fn extract_archive(&mut self, destination: Option<String>) {
        if self.archive_rx.is_some() {
            self.error = Some("Another archive operation is already running".to_string());
            return;
        }

        // Get the archive path from current selection
        let Some(archive_path) = self.get_selected_path() else {
            self.error = Some("No file selected for extraction".to_string());
            return;
        };

        // Check if it's actually an archive
        if gravityfile_ops::ArchiveFormat::from_path(&archive_path).is_none() {
            self.error = Some("Selected file is not a supported archive format".to_string());
            return;
        }

        // Determine destination directory
        let dest_dir = if let Some(dest) = destination {
            // Validate user-provided path
            let dest_path = match Self::validate_user_path(&dest) {
                Ok(p) => p,
                Err(e) => {
                    self.error = Some(format!("Invalid destination: {}", e));
                    return;
                }
            };

            if dest_path.is_absolute() {
                dest_path
            } else {
                // Relative to view root
                self.view_root.join(&dest_path)
            }
        } else {
            // Extract to current directory by default
            self.view_root.clone()
        };

        let (tx, rx) = oneshot::channel();
        self.archive_rx = Some(rx);
        self.operation_message = Some((true, "Extracting archive…".to_string()));
        let cancel = CancellationToken::new();
        self.archive_cancel = Some(ArchiveCancellation(cancel.clone()));
        tokio::task::spawn_blocking(move || {
            let result =
                gravityfile_ops::extract_archive_cancellable(&archive_path, &dest_dir, &cancel);
            let _ = tx.send(ArchiveCompletion::Extract {
                archive_path,
                destination: dest_dir,
                result,
            });
        });
    }

    /// Compress marked files into an archive.
    pub(super) fn compress_files(&mut self, archive_name: &str) {
        if self.archive_rx.is_some() {
            self.error = Some("Another archive operation is already running".to_string());
            return;
        }

        // Get files to compress from marked items, or current selection
        let files: Vec<PathBuf> = if self.marked.is_empty() {
            if let Some(path) = self.get_selected_path() {
                vec![path]
            } else {
                self.error = Some("No files selected for compression".to_string());
                return;
            }
        } else {
            self.marked.iter().cloned().collect()
        };

        if files.is_empty() {
            self.error = Some("No files to compress".to_string());
            return;
        }

        // Validate archive name
        let validated_name = match Self::validate_user_path(archive_name) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(format!("Invalid archive name: {}", e));
                return;
            }
        };

        // Determine archive path - relative to view root
        let archive_path = self.view_root.join(&validated_name);

        // Determine format from name
        let format = match gravityfile_ops::ArchiveFormat::from_path(&archive_path) {
            Some(f) => f,
            None => {
                // Default to zip if no recognized extension
                gravityfile_ops::ArchiveFormat::Zip
            }
        };

        // Adjust path if needed (add extension if missing)
        let archive_path = if gravityfile_ops::ArchiveFormat::from_path(&archive_path).is_none() {
            let mut native_path = archive_path.into_os_string();
            native_path.push(format.extension());
            PathBuf::from(native_path)
        } else {
            archive_path
        };

        let (tx, rx) = oneshot::channel();
        self.archive_rx = Some(rx);
        self.operation_message = Some((true, "Creating archive…".to_string()));
        let cancel = CancellationToken::new();
        self.archive_cancel = Some(ArchiveCancellation(cancel.clone()));
        tokio::task::spawn_blocking(move || {
            let result =
                gravityfile_ops::create_archive_cancellable(&files, &archive_path, format, &cancel);
            let _ = tx.send(ArchiveCompletion::Compress {
                archive_path,
                sources: files,
                result,
            });
        });
    }
}
