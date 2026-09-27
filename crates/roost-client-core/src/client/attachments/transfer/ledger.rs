//! The three card transitions an upload makes, in the order it makes them.
//! Depends on `store::transfers` for every decision: a progress decrease is a
//! restart inside `running`, a late tick for a settled card is ignored, and a
//! settled card dismisses itself. This file only forwards.

use crate::client::attachments::transfer::AttachmentTransferResult;
use crate::store::Store;
use crate::store::transfers::{
    NewTransfer, TransferDirection, TransferState, add_transfer, mark_transfer_state,
    set_transfer_progress,
};

/// Put an upload card on the stack, in the state that means "bytes are
/// moving".
///
/// One card per upload id, and the id is the one the grant named, so a card
/// and a grant are the same upload rather than two things that agree.
pub fn begin_upload_card(store: &mut Store, id: &str, name: &str, total_bytes: u64, now_ms: u64) {
    add_transfer(
        store,
        NewTransfer {
            id: id.to_owned(),
            name: name.to_owned(),
            direction: TransferDirection::Up,
            bytes_total: total_bytes,
            state: TransferState::Running,
            preview_url: None,
            now_ms,
        },
    );
}

/// Report one acknowledged byte count. Returns whether the card changed.
///
/// A decrease is a restart inside `running`, not a failure: that rule belongs
/// to the card state machine, and this only forwards the count.
pub fn record_upload_progress(store: &mut Store, id: &str, bytes_done: u64, now_ms: u64) -> bool {
    set_transfer_progress(store, id, bytes_done, None, now_ms)
}

/// Settle the card: `Done` for a committed upload, `Failed` with the reason
/// otherwise. Returns whether the card changed.
///
/// The path is deliberately not written onto the card. A card is a progress
/// record and a failure the user can read; the path belongs to the insertion
/// step, which is the only consumer of it.
pub fn settle_upload_card(
    store: &mut Store,
    id: &str,
    outcome: Result<&AttachmentTransferResult, &str>,
    now_ms: u64,
) -> bool {
    match outcome {
        Ok(_) => mark_transfer_state(store, id, TransferState::Done, None, now_ms),
        Err(reason) => mark_transfer_state(
            store,
            id,
            TransferState::Failed,
            Some(reason.to_owned()),
            now_ms,
        ),
    }
}
