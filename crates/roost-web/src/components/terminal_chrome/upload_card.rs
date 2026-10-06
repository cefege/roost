//! The transfer card one upload writes through, and the settled outcome the
//! card is showing.
//!
//! Every transition is here so the driver above it never touches the store's
//! transfer vocabulary, and so the four outcomes a card can end in — accepted,
//! deduplicated, rejected, ambiguous — are decided in one place. AMBIGUOUS is
//! the one that matters: the bytes may be on the worker's disk, so it is its
//! own terminal state, and it is the state a user must NOT be tempted to retry.
//! Ports the card half of `enqueueAttachmentTo` in
//! `apps/web/src/lib/attachments.ts` and the outcomes of
//! `apps/web/src/store/transfers.ts`.

use roost_client_core::client::attachments::transfer::AttachmentTransferResult;
use roost_client_core::client::attachments::transfer::ledger::{
    begin_upload_card, record_upload_progress, settle_upload_card,
};
use roost_client_core::store::Store;
use roost_client_core::store::transfers::{
    TransferRoute, TransferState, add_transfer, mark_transfer_state, set_transfer_route,
};

use super::upload_id::now_ms;
use super::upload_plan::UploadOutcome;

/// The one preview URL an upload may mint, released when its card leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPreview(pub Option<String>);

impl UploadPreview {
    /// The URL to hand the card, when the browser could mint one.
    pub fn url(&self) -> Option<String> {
        self.0.clone()
    }
}

/// Put a card on the stack, in the state that means "chosen, not started".
pub fn begin_card(
    store: &mut Store,
    upload_id: &str,
    file_name: &str,
    total_bytes: u64,
    preview: &UploadPreview,
) {
    add_transfer(
        store,
        roost_client_core::store::transfers::NewTransfer {
            id: upload_id.to_owned(),
            name: file_name.to_owned(),
            direction: roost_client_core::store::transfers::TransferDirection::Up,
            bytes_total: total_bytes,
            state: TransferState::Queued,
            preview_url: preview.url(),
            now_ms: now_ms(),
        },
    );
}

/// The card is hashing the content for its dedup probe.
pub fn mark_hashing(store: &mut Store, upload_id: &str) {
    mark(store, upload_id, TransferState::Hashing);
}

/// The probe missed; bytes are moving.
pub fn mark_running(store: &mut Store, upload_id: &str) {
    mark(store, upload_id, TransferState::Running);
}

/// The probe hit: the worker already held these bytes and nothing was sent.
pub fn mark_deduplicated(store: &mut Store, upload_id: &str) {
    mark(store, upload_id, TransferState::Dedup);
}

/// One acknowledged byte count. Returns whether the card changed.
pub fn record_progress(store: &mut Store, upload_id: &str, bytes_done: u64) -> bool {
    record_upload_progress(store, upload_id, bytes_done, now_ms())
}

/// A carrier opened and bytes are about to take this route.
pub fn mark_route(store: &mut Store, upload_id: &str, route: TransferRoute) {
    set_transfer_route(store, upload_id, route);
}

/// Settle the card from the outcome, and report the path a caller should type
/// into the PTY — `None` for a rejected or ambiguous upload, because neither
/// has a path the worker is known to hold.
pub fn settle_card(store: &mut Store, upload_id: &str, outcome: &UploadOutcome) -> Option<String> {
    match outcome {
        UploadOutcome::Accepted(result) => {
            settle_upload_card(store, upload_id, Ok(result), now_ms());
            Some(result.abs_path.clone())
        }
        UploadOutcome::Deduplicated { abs_path } => {
            let held = AttachmentTransferResult {
                abs_path: abs_path.clone(),
            };
            settle_upload_card(store, upload_id, Ok(&held), now_ms());
            Some(abs_path.clone())
        }
        UploadOutcome::Rejected { reason } => {
            settle_upload_card(store, upload_id, Err(reason), now_ms());
            None
        }
        // AMBIGUOUS, and deliberately not a failure: the write may have landed.
        // A card that said "failed" invites the user to try again, and a
        // retried ambiguous write is a doubled upload.
        UploadOutcome::Ambiguous { reason } => {
            mark_transfer_state(
                store,
                upload_id,
                TransferState::Ambiguous,
                Some(reason.clone()),
                now_ms(),
            );
            None
        }
    }
}

/// A refusal that happened before any carrier was chosen, so the card is put on
/// the stack only to say why it left.
pub fn refuse_card(store: &mut Store, upload_id: &str, file_name: &str, reason: &str) {
    begin_upload_card(store, upload_id, file_name, 0, now_ms());
    mark_transfer_state(
        store,
        upload_id,
        TransferState::Failed,
        Some(reason.to_owned()),
        now_ms(),
    );
}

fn mark(store: &mut Store, upload_id: &str, state: TransferState) {
    mark_transfer_state(store, upload_id, state, None, now_ms());
}
