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

use super::{Pump, socket};

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
            core.storage().set(SYNC_WATERMARK_KEY, &event_id.to_string());
        }
        Effect::PersistAgentSeen { encoded } => persist_agent_seen(pump, &encoded),
        Effect::SendDirect { token, command } => {
            // A direct route is elected only after a carrier reports ready, and
            // this build's carriers are the loopback and peer slices' to open;
            // until one exists the core never elects a route to send on.
            tracing::warn!(
                target: "pump",
                worker_fp = token.worker_fp.as_deref().unwrap_or(""),
                command = ?std::mem::discriminant(&command),
                "no direct carrier is open for this token; command dropped"
            );
        }
        Effect::RequestDirectGrant {
            session_id,
            worker_fp,
        } => {
            tracing::info!(
                target: "pump",
                %session_id,
                %worker_fp,
                "direct terminal grant requested; the Sync route stays elected"
            );
        }
    }
}

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
