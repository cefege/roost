//! Performing one `Effect` the core returned, in the order it returned them.
//!
//! Called only by `Pump::dispatch`. Depends on `pump::socket` for the Sync
//! socket, `platform::connect` for Connect calls, and the core's storage for
//! the two persisted ledgers. The v2 counterparts are the call sites the store
//! actions made directly (`sync-domain-state.ts:83-101` for a command,
//! `sync-frame.ts:55-69` for the watermark, `agentSeenLedger.ts` for the merge).

use roost_client_core::client::agents::{AGENT_SEEN_STORAGE_KEY, AgentSeenLedger};
use roost_client_core::sync::SYNC_WATERMARK_KEY;
use roost_client_core::{ClientEvent, Effect};

use super::{Pump, carriers, socket};

/// Perform one effect.
pub(super) fn perform(pump: &Pump, effect: Effect) {
    match effect {
        Effect::DialSync { generation, dial } => socket::open(pump, generation, dial),
        Effect::CloseSyncLink { generation, reason } => socket::close(pump, generation, &reason),
        Effect::SendSync(command) => socket::send(pump, &command),
        Effect::Rpc(call) => {
            let pump = pump.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let result = pump.inner.rpc.call_core(&call).await;
                pump.dispatch(ClientEvent::RpcResultReceived(result));
            });
        }
        Effect::PersistWatermark { event_id } => {
            let core = pump.inner.core.borrow();
            core.storage()
                .set(SYNC_WATERMARK_KEY, &event_id.to_string());
        }
        Effect::PersistAgentSeen { encoded } => persist_agent_seen(pump, &encoded),
        Effect::SendDirect { token, command } => carriers::send(pump, &token, &command),
        Effect::RequestDirectGrant {
            session_ids,
            worker_fp,
        } => {
            // A request that is only logged leaves the core's grant lifecycle in
            // `Requested` forever, which is the state whose whole point is that
            // nothing waits on it: the coordinator never mints, no carrier ever
            // authenticates, and the session stays on Sync with no fault anywhere
            // to say why. The grant is therefore minted here and the answer is
            // reported back either way — a mint that returns is a REFUSAL, not a
            // pending state (`client::carriers::grant::GrantInput::Refused`).
            carriers::request_grant(pump, session_ids, &worker_fp);
        }
        Effect::CloseDirectCarriers { worker_fp } => close_worker_carriers(pump, &worker_fp),
        Effect::MintTerminalViewId {
            session_id,
            attempt_id,
            logical_view_id,
            target,
        } => mint_terminal_view_id(pump, session_id, attempt_id, logical_view_id, target),
        Effect::Carrier(action) => perform_carrier(pump, *action),
    }
}

/// Mint one view id with the document's own entropy, and hand it straight back.
///
/// SYNCHRONOUS, and dispatched rather than queued on purpose: the core asked for
/// an id to publish a view with, and the only thing between the ask and the
/// answer is one `randomUUID`. `Pump::dispatch` queues a reentrant event until the
/// effects in flight finish, so handing it back here is ordered behind the
/// effect that asked rather than racing it.
fn mint_terminal_view_id(
    pump: &Pump,
    session_id: String,
    attempt_id: u64,
    logical_view_id: String,
    target: roost_client_core::ViewIdTarget,
) {
    let wire_view_id = crate::platform::terminal_view_id::mint_view_id();
    match &wire_view_id {
        Some(id) => tracing::info!(
            target: "terminal",
            %session_id,
            %logical_view_id,
            target = target.as_str(),
            wire_view_id = %id,
            "minted a terminal view id"
        ),
        None => tracing::warn!(
            target: "terminal",
            %session_id,
            %logical_view_id,
            target = target.as_str(),
            "this document has no secure-context crypto, so it mints no view id"
        ),
    }
    pump.dispatch(ClientEvent::TerminalViewIdMinted {
        session_id,
        attempt_id,
        logical_view_id,
        target,
        wire_view_id,
    });
}

/// Perform one peer-lifecycle action, or say that this build cannot.
#[cfg(target_arch = "wasm32")]
fn perform_carrier(pump: &Pump, action: roost_client_core::client::carriers::CarrierEffect) {
    super::peer_lane::perform(pump, action);
}

/// A build with no browser holds no peer connection and no timer, so the
/// effect is still emitted and still REPORTED — never ignored. The
/// silent-drop failure is the one this whole layer refuses to make, and a
/// document that cannot peer would otherwise read as one that chose not to.
#[cfg(not(target_arch = "wasm32"))]
fn perform_carrier(_pump: &Pump, action: roost_client_core::client::carriers::CarrierEffect) {
    tracing::warn!(
        target: "carriers",
        action = ?action,
        "this build has no browser WebRTC stack; the peer action was not performed"
    );
}

/// A worker was deleted: every socket it held goes, and each one is logged by
/// the generation it was presenting.
///
/// A handle is closed rather than dropped so the worker's door sees a close
/// frame while it is still listening; a dropped handle closes it too, but nothing
/// here would say which connection the operator's delete just took down.
///
/// A build with no browser holds no connections, so the arm exists and does
/// nothing: the effect is still emitted, and a host that ignored it would be the
/// silent-drop failure this whole layer refuses to make.
#[cfg(target_arch = "wasm32")]
fn close_worker_carriers(pump: &Pump, worker_fp: &str) {
    let closed = pump.inner.carriers.borrow_mut().retire_worker(worker_fp);
    if closed.is_empty() {
        return;
    }
    tracing::info!(
        target: "carriers",
        worker_fp,
        closed = closed.len(),
        "worker retired; its direct carriers are closed"
    );
    for handle in closed {
        handle.close(1000, "the worker was retired");
    }
}

/// A build with no browser holds no connections to close.
#[cfg(not(target_arch = "wasm32"))]
fn close_worker_carriers(_pump: &Pump, _worker_fp: &str) {}

/// Merge this tab's acknowledgement ledger with whatever another tab on the
/// profile wrote, then write the union: a blind write would discard the other
/// tab's acknowledgements.
fn persist_agent_seen(pump: &Pump, encoded: &str) {
    let core = pump.inner.core.borrow();
    let storage = core.storage();
    let mut merged = AgentSeenLedger::decode(storage.get(AGENT_SEEN_STORAGE_KEY).as_deref());
    let ours = AgentSeenLedger::decode(Some(encoded));
    merged.merge(&ours.tokens());
    storage.set(AGENT_SEEN_STORAGE_KEY, &merged.encode());
}
