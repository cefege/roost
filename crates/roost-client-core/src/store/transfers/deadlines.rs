//! The two transfer deadlines: a running card that stopped advancing is
//! called stalled, and a settled card whose dismissal has come leaves the
//! stack. Called once per pass by `handle_sweep`; the deadlines themselves are
//! armed by the parent module's state machine.

use super::{TRANSFER_STALL_AFTER_MS, TransferState};
use crate::store::Store;

/// Run both deadlines. Returns whether any card changed, so a caller can skip
/// a repaint it has already accounted for.
pub fn sweep_transfers(store: &mut Store, now_ms: u64) -> bool {
    let stalled: Vec<String> = store
        .transfers
        .transfers
        .values()
        .filter(|transfer| {
            transfer.state == TransferState::Running
                && transfer
                    .last_progress_ms
                    .is_some_and(|last| now_ms.saturating_sub(last) >= TRANSFER_STALL_AFTER_MS)
        })
        .map(|transfer| transfer.id.clone())
        .collect();
    let due: Vec<String> = store
        .transfers
        .transfers
        .values()
        .filter(|transfer| {
            transfer
                .dismiss_at_ms
                .is_some_and(|deadline| now_ms >= deadline)
        })
        .map(|transfer| transfer.id.clone())
        .collect();
    if stalled.is_empty() && due.is_empty() {
        return false;
    }
    for id in &stalled {
        if let Some(transfer) = store.transfers.transfers.get_mut(id) {
            transfer.state = TransferState::Stalled;
        }
    }
    for id in &due {
        store.transfers.drop_card(id);
    }
    store.note_change();
    tracing::debug!(
        target: "store",
        stalled = stalled.len(),
        removed = due.len(),
        "transfer deadlines"
    );
    true
}
