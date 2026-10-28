//! Read chosen files and create local image previews for staged attachments.
//! The hidden input stays beside the composer's attach button because iOS
//! rejects programmatic clicks outside the gesture that requested them.

use dioxus::prelude::*;

use super::dom;

/// One chosen file: its name, length and the browser's handle to it. The
/// bytes are read a chunk at a time while the upload runs, so a large video is
/// never held whole in the tab's memory.
#[derive(Debug, Clone, PartialEq)]
pub struct ChosenFile {
    /// The name the worker will store it under.
    pub name: String,
    /// Its byte length.
    pub size_bytes: u64,
    /// The browser's handle, read through `dom::read_file_range`.
    pub file: web_sys::File,
    /// A local image URL owned by staging and released on removal or Send,
    /// through [`crate::components::terminal_chrome::dom::revoke_preview`].
    pub preview_url: Option<String>,
    /// Upload identity shared with the transfer ledger.
    pub upload_id: Option<String>,
    /// The status shown by the shared staged attachment component.
    pub upload_state: StagedUploadState,
    /// Worker path returned by the upload, held until Send.
    pub uploaded_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StagedUploadState {
    #[default]
    Staged,
    Queued,
    Hashing,
    Uploading {
        bytes_sent: u64,
    },
    Done,
    Failed {
        reason: String,
        retryable: bool,
    },
}

impl StagedUploadState {
    #[must_use]
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::Queued | Self::Hashing | Self::Uploading { .. })
    }

    #[must_use]
    pub fn blocks_send(&self) -> bool {
        self.is_in_flight() || matches!(self, Self::Failed { .. })
    }
}

/// Apply one upload event to the staged-file lifecycle.
pub fn transition_staged_upload(state: &mut StagedUploadState, next: StagedUploadState) -> bool {
    let allowed = matches!(
        (&*state, &next),
        (StagedUploadState::Staged, StagedUploadState::Queued)
            | (StagedUploadState::Queued, StagedUploadState::Hashing)
            | (
                StagedUploadState::Queued,
                StagedUploadState::Uploading { .. }
            )
            | (StagedUploadState::Queued, StagedUploadState::Failed { .. })
            | (
                StagedUploadState::Hashing,
                StagedUploadState::Uploading { .. }
            )
            | (StagedUploadState::Hashing, StagedUploadState::Done)
            | (StagedUploadState::Hashing, StagedUploadState::Failed { .. })
            | (
                StagedUploadState::Uploading { .. },
                StagedUploadState::Uploading { .. }
            )
            | (StagedUploadState::Uploading { .. }, StagedUploadState::Done)
            | (
                StagedUploadState::Uploading { .. },
                StagedUploadState::Failed { .. }
            )
            | (
                StagedUploadState::Failed {
                    retryable: true,
                    ..
                },
                StagedUploadState::Queued
            )
    );
    if allowed {
        *state = next;
    }
    allowed
}

/// The `multiple` file input the composer's attach button clicks.
///
/// Hidden with `display: none` rather than `visibility: hidden` so it is out
/// of the focus order and out of the accessibility tree entirely; the button
/// that owns the gesture is the control a reader announces.
#[component]
pub fn AttachmentInput(
    #[props(default)] accept: Option<String>,
    on_chosen: EventHandler<Vec<ChosenFile>>,
    onmounted: Option<EventHandler<MountedEvent>>,
) -> Element {
    let read = move |event: FormEvent| {
        spawn_read_chosen(dom::files_of_event(&event), on_chosen);
    };
    let mounted = move |event: MountedEvent| {
        if let Some(handler) = onmounted {
            handler.call(event);
        }
    };
    rsx! {
        input {
            r#type: "file",
            multiple: true,
            accept: accept,
            style: "display: none;",
            "data-testid": "chat-file-input",
            onmounted: mounted,
            onchange: read,
        }
    }
}

/// Turn a picker result or a drop into `ChosenFile`s, in the order the user
/// picked them.
///
/// The desktop contract attaches several files in one gesture and the worker
/// stores them in that order, so the order is the payload and not an accident.
pub(super) fn spawn_read_chosen(
    files: Vec<web_sys::File>,
    on_chosen: EventHandler<Vec<ChosenFile>>,
) {
    // Off the render path, because minting a preview is asynchronous.
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let chosen = read_chosen(files).await;
        if !chosen.is_empty() {
            on_chosen.call(chosen);
        }
    });
    // A host with no file picker has nothing to read.
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (files, on_chosen);
}

/// Read `files` into `ChosenFile`s, previews minted, in the order given. The
/// one reader behind the picker, a drop, and a paste.
#[cfg(target_arch = "wasm32")]
pub(super) async fn read_chosen(files: Vec<web_sys::File>) -> Vec<ChosenFile> {
    let mut chosen = Vec::with_capacity(files.len());
    for file in files {
        chosen.push(ChosenFile {
            name: dom::file_name(&file),
            size_bytes: dom::file_size(&file),
            preview_url: dom::preview_url(&file).await,
            file,
            upload_id: None,
            upload_state: StagedUploadState::Staged,
            uploaded_path: None,
        });
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::{StagedUploadState, transition_staged_upload};

    #[test]
    fn staged_upload_progress_and_send_gate_follow_lifecycle() {
        let mut state = StagedUploadState::Staged;
        assert!(!state.blocks_send());
        assert!(transition_staged_upload(
            &mut state,
            StagedUploadState::Queued
        ));
        assert!(state.blocks_send());
        assert!(transition_staged_upload(
            &mut state,
            StagedUploadState::Hashing
        ));
        assert!(transition_staged_upload(
            &mut state,
            StagedUploadState::Uploading { bytes_sent: 12 }
        ));
        assert!(state.blocks_send());
        assert!(transition_staged_upload(
            &mut state,
            StagedUploadState::Done
        ));
        assert!(!state.blocks_send());
        assert!(!transition_staged_upload(
            &mut state,
            StagedUploadState::Uploading { bytes_sent: 13 }
        ));
    }

    #[test]
    fn failed_upload_blocks_send_until_removed_or_retried() {
        let mut state = StagedUploadState::Uploading { bytes_sent: 4 };
        assert!(transition_staged_upload(
            &mut state,
            StagedUploadState::Failed {
                reason: "carrier refused".to_owned(),
                retryable: true,
            }
        ));
        assert!(state.blocks_send());
        assert!(transition_staged_upload(
            &mut state,
            StagedUploadState::Queued
        ));
        assert!(state.blocks_send());
    }
}
