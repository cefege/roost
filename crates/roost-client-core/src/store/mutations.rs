//! The named store writes, and the only ones a component may make.
//!
//! `ARCHITECTURE.md:42` — components never mutate the store directly; they call
//! one of these. Each write mirrors a Connect call that has already returned, so
//! the UI reflects the change before the Sync delta that would confirm it lands.
//! That is the same optimistic pattern as `optimistic_spawn`, and it carries the
//! same hazard: a write whose answer arrives late must not undo a newer one. The
//! difference is that these records are keyed by the coordinator's own id, so
//! there is no second write to race — the Sync delta is keyed the same way and
//! the later arrival wins by construction.
//!
//! The Solid footgun this file exists to avoid is
//! `feedback_solid_setstore_record_replace` (`docs/FAILURE-INDEX.md:16`): a
//! setter function handed a whole Record subtree silently no-ops. A `BTreeMap`
//! cannot express that mistake, which is the main reason these records are maps
//! here.
//!
//! Ported from `apps/web/src/store/mutations.ts`, plus the worker registry the
//! bootstrap list lands in — `RpcResult::WorkersList` had nowhere to put its rows.
//! The dev-dep-free shape of the pair request is v2's `root.ts:20-36`.

use std::collections::BTreeMap;

use roost_protocol::wire::{McpRelay, Worker};

use crate::store::Store;

/// A pending tap-to-pair request, as the browser learned it from the
/// coordinator's inline response.
///
/// Declared here because `root.ts` declares it, not because this module needs
/// every field: the pairing ceremony and the notifier both read it, and a second
/// declaration of the same row would be a second answer to "what is a pair
/// request".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRequest {
    /// The coordinator's one-shot id for this request. The map key.
    pub ephemeral_id: String,
    /// What the pairing surface shows as the request's subject.
    pub label: String,
    /// When the browser first heard about it, in milliseconds.
    pub created_at_ms: i64,
    /// The requesting user agent, verbatim.
    pub user_agent: String,
    /// The browser the request claims.
    pub client_browser: String,
    /// The operating system the request claims.
    pub client_os: String,
    /// The device class the request claims.
    pub client_device_type: String,
    /// The address the request arrived from, as the edge saw it.
    pub source_ip: String,
    /// The country the edge resolved, when it could.
    pub country_code: String,
    /// The region the edge resolved, when it could.
    pub region: String,
    /// The city the edge resolved, when it could.
    pub city: String,
    /// The edge identity provider that vouched for the request, when one did.
    pub edge_identity_provider: String,
    /// The edge identity itself, when there was one.
    pub edge_identity: String,
    /// Whether the edge verified that identity.
    pub edge_identity_verified: bool,
    /// When the request stops being answerable, in milliseconds.
    pub expires_at_ms: i64,
}

impl PairRequest {
    /// Whether the request can still be answered at `now_ms`.
    pub fn is_live_at(&self, now_ms: i64) -> bool {
        now_ms < self.expires_at_ms
    }
}

/// Replace the worker registry with an authoritative list.
///
/// The bootstrap list, and every re-hydration a reconnect's fresh domain
/// generation triggers. A REPLACE and not a merge, because a worker absent from
/// the list is one the coordinator can no longer reach, and leaving its row behind
/// is how a sidebar offers a machine that will not answer. Reachability is not
/// this store's to derive: the routable fingerprint set arrives on its own bus, and
/// a row here says a machine is KNOWN, not that it is reachable.
pub fn replace_workers(store: &mut Store, workers: BTreeMap<String, Worker>) -> bool {
    if store.workers == workers {
        return false;
    }
    store.workers = workers;
    store.note_change();
    tracing::debug!(target: "store", count = store.workers.len(), "worker registry");
    true
}

/// Forget one worker, when the coordinator retires it.
pub fn delete_worker(store: &mut Store, worker_fp: &str) -> bool {
    if store.workers.remove(worker_fp).is_none() {
        return false;
    }
    store.note_change();
    tracing::info!(target: "store", worker_fp, "worker retired from the registry");
    true
}

/// Forget one pair request.
///
/// Per-key, always. A whole-map write here would drop the requests a user is still
/// looking at along with the one they dismissed.
pub fn delete_pair_request(store: &mut Store, ephemeral_id: &str) -> bool {
    if store.pair_requests.remove(ephemeral_id).is_none() {
        return false;
    }
    store.note_change();
    tracing::debug!(target: "store", ephemeral_id, "pair request dismissed");
    true
}

/// Replace the relay set with an authoritative one.
pub fn replace_mcp_relays(store: &mut Store, relays: BTreeMap<String, McpRelay>) -> bool {
    if store.mcp_relays == relays {
        return false;
    }
    store.mcp_relays = relays;
    store.note_change();
    true
}

/// Add or replace one relay, keyed by its own id.
pub fn upsert_mcp_relay(store: &mut Store, relay: McpRelay) -> bool {
    let id = relay.id.to_string();
    if store.mcp_relays.get(&id) == Some(&relay) {
        return false;
    }
    store.mcp_relays.insert(id, relay);
    store.note_change();
    true
}

/// Remove one relay.
pub fn delete_mcp_relay(store: &mut Store, id: &str) -> bool {
    if store.mcp_relays.remove(id).is_none() {
        return false;
    }
    store.note_change();
    tracing::debug!(target: "store", relay = %id, "mcp relay removed");
    true
}
