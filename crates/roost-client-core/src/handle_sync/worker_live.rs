//! The live worker-metrics subscription: the worker set this client samples, and
//! the command that tells the coordinator about it on the open Sync socket.
//!
//! The coordinator's subscription lives on the socket that named it and dies with
//! that socket, so the whole set is re-sent on every socket the client opens. A
//! set change is sent as the whole set too, so the command is idempotent.

use roost_protocol::wire::brand::WorkerFp;

use crate::effect::{Effect, SyncCommand};
use crate::store::Store;

/// Replace the subscribed worker set, and send the whole set to the open socket.
///
/// With no open socket the set is kept and goes out on the next socket's open
/// (`resend_worker_live_subscription`).
pub fn handle_worker_live_subscribe(
    store: &mut Store,
    worker_fps: &[WorkerFp],
    out: &mut Vec<Effect>,
) {
    store.worker_live.replace_subscription(worker_fps);
    store.note_change();
    tracing::info!(
        target: "sync",
        workers = store.worker_live.subscribed().len(),
        "worker live subscription set"
    );
    if store.sync.socket_id().is_some() {
        out.push(Effect::SendSync(SyncCommand::WorkerLiveSubscription {
            worker_fps: store.worker_live.subscribed().to_vec(),
        }));
    }
}

/// Re-send the subscribed set on a freshly opened Sync socket. An empty set has
/// nothing to re-send: the new socket starts with no subscription.
pub fn resend_worker_live_subscription(store: &Store, out: &mut Vec<Effect>) {
    let subscribed = store.worker_live.subscribed();
    if subscribed.is_empty() {
        return;
    }
    tracing::info!(
        target: "sync",
        workers = subscribed.len(),
        "worker live subscription re-sent on sync link open"
    );
    out.push(Effect::SendSync(SyncCommand::WorkerLiveSubscription {
        worker_fps: subscribed.to_vec(),
    }));
}
