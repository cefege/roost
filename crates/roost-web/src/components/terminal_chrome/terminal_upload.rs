//! Files into a terminal pane: picker, drop, and paste share one upload path.
//! Upload progress and completion update the pane's staged set; only the
//! composer's Send action types completed worker paths into its draft.
//! The upload queue and its carrier plan belong to `super::upload`.

use dioxus::prelude::{EventHandler, Signal, WritableExt};
use std::{cell::Cell, rc::Rc};

use super::attachment_picker::ChosenFile;
use super::short_paths::short_path_preference;
use super::upload::{InsertionSink, UploadContext, enqueue_attachments};
use super::upload_host::DirectIdentity;
use crate::pump::Pump;

fn upload_context(pump: &Pump, session_id: &str, worker_fp: &str) -> UploadContext {
    let tab_id = pump.core().borrow().store().tab_id.clone();
    let device_fingerprint = pump
        .rpc()
        .device_key()
        .map(|key| key.fingerprint().to_owned())
        .unwrap_or_default();
    UploadContext {
        session_id: session_id.to_owned(),
        worker_fp: Some(worker_fp.to_owned()),
        short_path: short_path_preference(),
        identity: DirectIdentity {
            tab_id,
            device_fingerprint,
        },
    }
}

/// Start every selected file immediately, leaving its path for the composer.
pub fn start_staged_uploads(
    pump: &Pump,
    session_id: &str,
    worker_fp: &str,
    mut staged_files: Signal<Vec<ChosenFile>>,
    mut chosen: Vec<ChosenFile>,
) {
    let mut queued = Vec::with_capacity(chosen.len());
    for file in &mut chosen {
        let Some(upload_id) = super::upload_id::mint_upload_id() else {
            file.upload_state = super::attachment_picker::StagedUploadState::Failed {
                reason: "This page cannot create a secure upload ID.".to_owned(),
                retryable: true,
            };
            continue;
        };
        file.upload_id = Some(upload_id);
        file.upload_state = super::attachment_picker::StagedUploadState::Staged;
        queued.push(file.clone());
    }
    staged_files.with_mut(|staged| staged.extend(chosen));
    if queued.is_empty() {
        return;
    }
    queue_staged_uploads(pump, session_id, worker_fp, staged_files, queued);
}

fn queue_staged_uploads(
    pump: &Pump,
    session_id: &str,
    worker_fp: &str,
    staged_files: Signal<Vec<ChosenFile>>,
    queued: Vec<ChosenFile>,
) {
    if queued.is_empty() {
        return;
    }
    let context = upload_context(pump, session_id, worker_fp);
    let path_signal = Cell::new(staged_files);
    let sink: InsertionSink = Rc::new(move |upload_id, path| {
        let mut path_signal = path_signal.get();
        let Ok(mut files) = path_signal.try_write() else {
            return;
        };
        if let Some(file) = files
            .iter_mut()
            .find(|file| file.upload_id.as_deref() == Some(upload_id))
        {
            file.uploaded_path = Some(path.to_owned());
        }
    });
    let status_signal = Cell::new(staged_files);
    let observer = Rc::new(move |upload_id: &str, update| {
        let mut status_signal = status_signal.get();
        let Ok(mut files) = status_signal.try_write() else {
            return;
        };
        if let Some(file) = files
            .iter_mut()
            .find(|file| file.upload_id.as_deref() == Some(upload_id))
        {
            let next = match update {
                super::upload::UploadUpdate::Queued => {
                    super::attachment_picker::StagedUploadState::Queued
                }
                super::upload::UploadUpdate::Hashing => {
                    super::attachment_picker::StagedUploadState::Hashing
                }
                super::upload::UploadUpdate::Uploading { bytes_sent } => {
                    super::attachment_picker::StagedUploadState::Uploading { bytes_sent }
                }
                super::upload::UploadUpdate::Done => {
                    super::attachment_picker::StagedUploadState::Done
                }
                super::upload::UploadUpdate::Failed { reason, retryable } => {
                    super::attachment_picker::StagedUploadState::Failed { reason, retryable }
                }
            };
            super::attachment_picker::transition_staged_upload(&mut file.upload_state, next);
        }
    });
    enqueue_attachments(pump, &context, queued, sink, Some(observer));
}

/// Retry one definitively failed staged upload without moving uncertain bytes.
pub fn retry_staged_upload(
    pump: &Pump,
    session_id: &str,
    worker_fp: &str,
    mut staged_files: Signal<Vec<ChosenFile>>,
    index: usize,
) {
    let retry_file = {
        let mut files = staged_files.write();
        let Some(file) = files.get_mut(index) else {
            return;
        };
        if !matches!(
            &file.upload_state,
            super::attachment_picker::StagedUploadState::Failed {
                retryable: true,
                ..
            }
        ) {
            return;
        }
        let Some(upload_id) = super::upload_id::mint_upload_id() else {
            file.upload_state = super::attachment_picker::StagedUploadState::Failed {
                reason: "This page cannot create a secure upload ID.".to_owned(),
                retryable: true,
            };
            return;
        };
        file.upload_id = Some(upload_id);
        file.uploaded_path = None;
        file.upload_state = super::attachment_picker::StagedUploadState::Staged;
        file.clone()
    };
    queue_staged_uploads(pump, session_id, worker_fp, staged_files, vec![retry_file]);
}

/// Build the composer's retry action for this pane's staged files.
pub fn retry_staged_upload_handler(
    pump: Pump,
    session_id: String,
    worker_fp: String,
    staged_files: Signal<Vec<ChosenFile>>,
) -> EventHandler<usize> {
    EventHandler::new(move |index| {
        retry_staged_upload(&pump, &session_id, &worker_fp, staged_files, index);
    })
}
/// Append an entry-point's files to the pane's staged set and start them.
pub fn stage_attachments(
    pump: Pump,
    staged_files: Signal<Vec<ChosenFile>>,
    session_id: String,
    worker_fp: String,
) -> EventHandler<Vec<ChosenFile>> {
    EventHandler::new(move |chosen: Vec<ChosenFile>| {
        tracing::info!(
            target: "attachments",
            session = %session_id,
            files = chosen.len(),
            "terminal attachments staged"
        );
        start_staged_uploads(&pump, &session_id, &worker_fp, staged_files, chosen);
    })
}

/// Read pasted files and start uploads as soon as their previews are ready.
#[cfg(target_arch = "wasm32")]
pub fn stage_pasted_files(
    pump: Pump,
    session_id: String,
    worker_fp: String,
    staged_files: Signal<Vec<ChosenFile>>,
    files: Vec<web_sys::File>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let chosen = super::attachment_picker::read_chosen(files).await;
        start_staged_uploads(&pump, &session_id, &worker_fp, staged_files, chosen);
    });
}
