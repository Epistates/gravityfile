//! The Miller preview pane: choosing what to preview and loading it off
//! the UI thread.

use std::path::PathBuf;

use tokio::sync::oneshot;

use super::state::LayoutMode;
use super::{App, PreviewLoad, PreviewRequest};
use crate::preview::{ImageContext, PreviewContent, PreviewLoader};

impl App {
    /// Scroll the file preview by `delta` rows.
    pub(super) fn scroll_preview(&mut self, delta: isize) {
        // The preview pane's rows, less its header line.
        let visible = self.preview_area.get().height.saturating_sub(1);
        self.preview_state.scroll_by(delta, usize::from(visible));
    }

    /// Update the file preview for the currently selected item in Miller mode.
    pub(super) fn update_preview(&mut self) {
        // Only update preview in Miller mode for non-directory items
        if self.layout_mode != LayoutMode::Miller {
            self.clear_preview();
            return;
        }

        let Some((view_node, _)) = self.get_view_root_node() else {
            self.clear_preview();
            return;
        };

        let Some(selected_child) = view_node.children.get(self.nav.miller_state.selected) else {
            self.clear_preview();
            return;
        };

        // Only show file preview for non-directories
        if selected_child.is_dir() {
            // For directories, we show child list directly via miller columns
            self.clear_preview();
            return;
        }

        // Get the full path to the selected file
        let file_path = self.nav.view_root.join(selected_child.file_name());

        self.schedule_preview(file_path, false);
    }

    pub(super) fn schedule_preview(&mut self, path: PathBuf, force: bool) {
        if !self.preview_state.begin_update(&path, force) {
            return;
        }

        let mode = self.preview_state.mode;
        // Decoding and protocol encoding run in the background task with the
        // rest of the preview work, never inside `terminal.draw`.
        let image_context = self.image_picker.as_ref().map(|picker| ImageContext {
            picker: picker.clone(),
            area: self.preview_area.get().as_size(),
        });
        let request = PreviewRequest {
            path,
            mode,
            image_context,
        };
        if self.preview_rx.is_some() {
            self.pending_preview = Some(request);
        } else {
            self.start_preview(request);
        }
    }

    pub(super) fn start_preview(&mut self, request: PreviewRequest) {
        let PreviewRequest {
            path,
            mode,
            image_context,
        } = request;
        let (tx, rx) = oneshot::channel();
        self.preview_rx = Some(rx);
        tokio::task::spawn_blocking(move || {
            let result = PreviewLoader::load_with_mode(&path, mode, image_context.as_ref());
            let _ = tx.send(PreviewLoad { path, mode, result });
        });
    }

    pub(super) fn finish_preview(&mut self, preview: Option<PreviewLoad>) {
        self.preview_rx = None;
        if let Some(request) = self.pending_preview.take() {
            // A newer request supersedes this result even when path/mode match
            // (forced reload, or leaving and returning to the same selection).
            self.start_preview(request);
        } else if let Some(preview) = preview {
            self.preview_state
                .complete_update(&preview.path, preview.mode, preview.result);
        } else if self.preview_state.loading {
            self.preview_state.loading = false;
            self.preview_state.content =
                PreviewContent::Error("Preview worker ended without a result".into());
        }
    }

    pub(super) fn clear_preview(&mut self) {
        // Keep observing the active load so switching views cannot spawn a
        // second worker while the first is still reading or decoding.
        self.pending_preview = None;
        self.preview_state.clear();
    }
}
