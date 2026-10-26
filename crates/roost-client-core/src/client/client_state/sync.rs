//! Drives account-scoped state sync from the sweep and from the answers to its
//! calls. `ClientStateSync` lives on `Store`: the debounce observations, the
//! in-flight batch, the pending pull, and the settled flag. Every timing
//! decision uses the monotonic `now_ms` the sweep passes in, never the epoch
//! clock that stamps writes.

use std::collections::BTreeMap;

use roost_protocol::client_state::{MAX_BATCH_ENTRIES, validate_value};

use super::entry::ClientStateEntry;
use super::merge::{merge_list_answer, merge_remote_entries};
use super::store::ClientStateStore;
use crate::effect::{Effect, RpcCall, RpcResult};
use crate::store::Store;

/// Quiet time a dirty key waits, so a burst of keystrokes flushes once.
const DEBOUNCE_MS: u64 = 1_000;
/// Longest the landing page waits for a first pull before rendering anyway.
const SETTLE_DEADLINE_MS: u64 = 5_000;

#[derive(Debug, Default)]
pub struct ClientStateSync {
    started_ms: u64,
    settled: bool,
    list_call_id: Option<u64>,
    observed: BTreeMap<String, Observation>,
}

#[derive(Debug, Clone, Copy)]
struct Observation {
    /// The dirty write sequence last seen for this key.
    seq: u64,
    /// The sweep time at which `seq` was first seen.
    changed_ms: u64,
    in_flight: Option<InFlight>,
    /// A batch failed for this key; it waits for the next pull, not a timer.
    parked: bool,
}

#[derive(Debug, Clone, Copy)]
struct InFlight {
    call_id: u64,
    /// The write sequence the batch carried. A newer write must survive the answer.
    sent_seq: u64,
}

impl Observation {
    fn is_due(&self, now_ms: u64) -> bool {
        self.in_flight.is_none()
            && !self.parked
            && now_ms.saturating_sub(self.changed_ms) >= DEBOUNCE_MS
    }
}

impl ClientStateSync {
    /// Start the settle deadline. Called once, when the core is constructed.
    pub(crate) fn begin(&mut self, now_ms: u64) {
        self.started_ms = now_ms;
    }

    /// True once the first pull has merged, or the deadline has passed.
    pub fn is_settled(&self) -> bool {
        self.settled
    }
}

/// Ask the coordinator for every synced key. Called once per link generation, so
/// a reconnect re-pulls, and it clears parked keys so they retry.
pub(crate) fn request_list(store: &mut Store, out: &mut Vec<Effect>) {
    for observation in store.client_state.observed.values_mut() {
        observation.parked = false;
    }
    let call_id = store.next_call_id();
    store.client_state.list_call_id = Some(call_id);
    tracing::info!(target: "client_state", call_id, "client state pull requested");
    out.push(Effect::Rpc(RpcCall::ClientStateList { call_id }));
}

/// Settle on deadline, note new dirty writes, and flush the keys whose quiet
/// period has passed. Called from the sweep, the only time entry point.
pub(crate) fn sweep(
    store: &mut Store,
    storage: &ClientStateStore,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    settle_on_deadline(store, now_ms);
    observe_dirty_keys(&mut store.client_state.observed, storage, now_ms);
    flush_due_keys(store, storage, now_ms, out);
}

fn settle_on_deadline(store: &mut Store, now_ms: u64) {
    let sync = &mut store.client_state;
    if sync.settled || now_ms.saturating_sub(sync.started_ms) < SETTLE_DEADLINE_MS {
        return;
    }
    sync.settled = true;
    tracing::info!(target: "client_state", reason = "timeout", "client state settled");
    store.note_change();
}

fn observe_dirty_keys(
    observed: &mut BTreeMap<String, Observation>,
    storage: &ClientStateStore,
    now_ms: u64,
) {
    let dirty: BTreeMap<String, u64> = storage.dirty_snapshot().into_iter().collect();
    observed.retain(|key, _| dirty.contains_key(key));
    for (key, seq) in dirty {
        let observation = observed.entry(key).or_insert(Observation {
            seq,
            changed_ms: now_ms,
            in_flight: None,
            parked: false,
        });
        if observation.seq != seq {
            observation.seq = seq;
            observation.changed_ms = now_ms;
            observation.parked = false;
        }
    }
}

fn flush_due_keys(
    store: &mut Store,
    storage: &ClientStateStore,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let batch = collect_batch(&mut store.client_state.observed, storage, now_ms);
    if batch.is_empty() {
        return;
    }
    let call_id = store.next_call_id();
    let mut entries = Vec::with_capacity(batch.len());
    for (entry, sent_seq) in batch {
        if let Some(observation) = store.client_state.observed.get_mut(&entry.key) {
            observation.in_flight = Some(InFlight { call_id, sent_seq });
        }
        entries.push(entry);
    }
    tracing::info!(
        target: "client_state",
        call_id,
        count = entries.len(),
        "client state set flushed"
    );
    out.push(Effect::Rpc(RpcCall::ClientStateSet { call_id, entries }));
}

/// The due keys, in key order, up to one batch. A key that cannot be sent is
/// released now: its dirty mark is cleared and it stays local.
fn collect_batch(
    observed: &mut BTreeMap<String, Observation>,
    storage: &ClientStateStore,
    now_ms: u64,
) -> Vec<(ClientStateEntry, u64)> {
    let due: Vec<String> = observed
        .iter()
        .filter(|(_, observation)| observation.is_due(now_ms))
        .map(|(key, _)| key.clone())
        .collect();
    let mut batch = Vec::new();
    for key in due {
        if batch.len() == MAX_BATCH_ENTRIES {
            break;
        }
        let Some(observation) = observed.get(&key).copied() else {
            continue;
        };
        let sendable =
            storage
                .flush_entry(&key)
                .filter(|entry| match validate_value("value", &entry.value) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(
                            target: "client_state",
                            reason = %error,
                            "client state value over the cap; kept local"
                        );
                        false
                    }
                });
        match sendable {
            Some(entry) => batch.push((entry, observation.seq)),
            None => {
                storage.clear_dirty_if_unchanged(&key, observation.seq);
                observed.remove(&key);
            }
        }
    }
    batch
}

/// Answer a Set: clear the keys the batch carried that were not written again,
/// then merge the coordinator's winners.
fn settle_set_answer(
    store: &mut Store,
    storage: &ClientStateStore,
    call_id: u64,
    entries: &[ClientStateEntry],
) {
    store.client_state.observed.retain(|key, observation| {
        let Some(flight) = observation.in_flight else {
            return true;
        };
        if flight.call_id != call_id {
            return true;
        }
        observation.in_flight = None;
        !storage.clear_dirty_if_unchanged(key, flight.sent_seq)
    });
    let counts = merge_remote_entries(storage, entries);
    tracing::info!(
        target: "client_state",
        call_id,
        applied = counts.applied,
        queued = counts.queued,
        "client state set answered"
    );
    rehydrate_after_merge(store, storage, counts.applied);
}

/// Release a failed batch's keys to wait for the next pull. They stay dirty, so
/// no write is lost, and they do not retry on a timer.
fn park_failed_set(store: &mut Store, call_id: u64) -> usize {
    let mut parked = 0;
    for observation in store.client_state.observed.values_mut() {
        if observation
            .in_flight
            .is_some_and(|flight| flight.call_id == call_id)
        {
            observation.in_flight = None;
            observation.parked = true;
            parked += 1;
        }
    }
    parked
}

fn owns_set_call(store: &Store, call_id: u64) -> bool {
    store.client_state.observed.values().any(|observation| {
        observation
            .in_flight
            .is_some_and(|flight| flight.call_id == call_id)
    })
}

fn settle_on_merge(store: &mut Store) {
    let sync = &mut store.client_state;
    if sync.settled {
        return;
    }
    sync.settled = true;
    tracing::info!(target: "client_state", reason = "merged", "client state settled");
    store.note_change();
}

/// A merge that wrote a remote value leaves the stores hydrated from the old
/// local values, so they are reloaded now rather than at the next boot.
fn rehydrate_after_merge(store: &mut Store, storage: &ClientStateStore, applied: usize) {
    if applied == 0 {
        return;
    }
    crate::core::hydrate_from_local(store, storage);
    store.note_change();
    tracing::info!(
        target: "client_state",
        applied,
        "client state stores rehydrated from local"
    );
}

/// Apply a client-state answer, returning whether it was one of ours. Only
/// calls this module issued are claimed; anything else continues down the
/// result chain.
pub(crate) fn settle_result(
    store: &mut Store,
    storage: &ClientStateStore,
    result: &RpcResult,
) -> bool {
    match result {
        RpcResult::ClientStateListed { call_id, entries }
            if store.client_state.list_call_id == Some(*call_id) =>
        {
            store.client_state.list_call_id = None;
            let counts = merge_list_answer(storage, entries);
            tracing::info!(
                target: "client_state",
                call_id,
                applied = counts.applied,
                queued = counts.queued,
                "client state list merged"
            );
            rehydrate_after_merge(store, storage, counts.applied);
            settle_on_merge(store);
            true
        }
        RpcResult::ClientStateSetAnswered { call_id, entries }
            if owns_set_call(store, *call_id) =>
        {
            settle_set_answer(store, storage, *call_id, entries);
            true
        }
        RpcResult::Failed { call_id, error }
            if store.client_state.list_call_id == Some(*call_id) =>
        {
            store.client_state.list_call_id = None;
            tracing::warn!(
                target: "client_state",
                call_id,
                error = %error,
                "client state pull failed; retried on the next link"
            );
            true
        }
        RpcResult::Failed { call_id, error } if owns_set_call(store, *call_id) => {
            let parked = park_failed_set(store, *call_id);
            tracing::warn!(
                target: "client_state",
                call_id,
                parked,
                error = %error,
                "client state set failed; keys wait for the next link"
            );
            true
        }
        _ => false,
    }
}
