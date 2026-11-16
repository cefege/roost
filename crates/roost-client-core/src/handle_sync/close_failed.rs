//! Closing a Sync link a frame proved broken, so the dial loop redials onto a
//! clean baseline.
//!
//! Called by the `SyncFrameRefused` arm in `handle_event` and by the folds that
//! meet a frame the core would otherwise leave unapplied.

use crate::effect::Effect;
use crate::store::Store;

/// Stop accepting on `generation` and ask the host to close its socket.
///
/// Only the CURRENT, accepting link is closed: a refusal that arrives for a
/// generation already replaced names a socket that is gone, and closing the
/// live one for it would tear down a healthy link. `request_redial` is the
/// same step taken first — `link.accepting = false` — so nothing after
/// this frame on the same socket is applied or acknowledged.
pub(crate) fn close_failed_sync_link(
    store: &mut Store,
    generation: u64,
    reason: String,
    out: &mut Vec<Effect>,
) {
    if !store.sync.request_redial(generation) {
        tracing::debug!(
            target: "sync",
            generation,
            reason = %reason,
            "sync frame refused on a link that is no longer current"
        );
        return;
    }
    store.note_change();
    tracing::warn!(target: "sync", generation, reason = %reason, "sync link closed: frame refused");
    out.push(Effect::CloseSyncLink { generation, reason });
}
