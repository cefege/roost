//! Running each subscribed domain's snapshot call and publishing its answer:
//! apply the rows, send `domain_ready`, mark the domain ready.
//!
//! Called from `apply_frame` (a `subscribed` announcement or a subscribed
//! `domain_reset` triggers), from `handle_rpc_result` (every answer is offered
//! here first) and from the sweep (deadlines and retries). Ported from
//! `apps/web/src/store/sync-bootstrap-hydration.ts`,
//! `apps/web/src/store/sync-domain-hydration.ts` (`trigger`, `run`),
//! `apps/web/src/store/sync-domain-state.ts:103-130` (`applySyncDomainSnapshot`)
//! and `apps/web/src/store/sync-hydrated.ts`.

use crate::client::rpc::AuthFailureKind;
use crate::effect::{Effect, RpcCall, RpcResult, SyncCommand, hydration_call};
use crate::store::Store;
use crate::store::root::{mark_browser_device_rejected, mark_protected_snapshot_published};
use crate::sync::SyncDomain;
use crate::sync::hydration::{HydrationTicket, SYNC_HYDRATION_DEADLINE_MS};

/// Start this domain's snapshot call for its current generation, unless one is
/// already running for it (v2 `_triggerSyncDomainHydration`).
///
/// The audit domain is lazy: only a mounted audit surface hydrates it, so a
/// trigger for it here does nothing (`sync-domain-hydration.ts:41-64`).
pub(crate) fn trigger_hydration(
    store: &mut Store,
    domain: SyncDomain,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let (Some(socket_generation), Some(socket_id)) = (
        store.sync.link_generation(),
        store.sync.socket_id().map(str::to_owned),
    ) else {
        return;
    };
    let Some(state) = store.sync.domains.get(&domain) else {
        return;
    };
    if !state.subscribed || state.ready {
        return;
    }
    let domain_generation = state.generation;
    if store.sync.hydrations.is_running(domain, domain_generation) {
        return;
    }
    let call_id = store.next_call_id();
    let Some(call) = hydration_call(domain, call_id, &socket_id) else {
        return;
    };
    store.sync.hydrations.begin(
        call_id,
        HydrationTicket {
            domain,
            domain_generation,
            socket_generation,
            deadline_ms: now_ms.saturating_add(SYNC_HYDRATION_DEADLINE_MS),
        },
    );
    tracing::info!(
        target: "sync",
        domain = domain.as_str(),
        domain_generation,
        call_id,
        "domain hydration started"
    );
    out.push(Effect::Rpc(call));
}

/// Offer one RPC answer to the hydrators and the bootstrap probe. `true` when
/// it was theirs, whether or not it still applied.
pub(crate) fn settle_hydration_result(
    store: &mut Store,
    result: &RpcResult,
    now_ms: u64,
    out: &mut Vec<Effect>,
) -> bool {
    let call_id = result.call_id();
    if store.sync.hydrations.take_probe(call_id) {
        settle_probe(store, result);
        return true;
    }
    let Some(ticket) = store.sync.hydrations.take(call_id) else {
        return false;
    };
    if !ticket_is_current(store, &ticket) {
        tracing::debug!(
            target: "sync",
            domain = ticket.domain.as_str(),
            call_id,
            "hydration answer for a superseded generation dropped"
        );
        return true;
    }
    let token = match (ticket.domain, result) {
        (_, RpcResult::Failed { error, .. }) => {
            tracing::warn!(
                target: "sync",
                domain = ticket.domain.as_str(),
                %error,
                "domain hydration failed"
            );
            if ticket.domain == SyncDomain::Terminal
                && error.auth_failure_kind("SessionsList") == AuthFailureKind::Device
            {
                mark_browser_device_rejected(store, "sessions_hydration");
            }
            schedule_retry(store, &ticket, now_ms);
            return true;
        }
        (
            SyncDomain::Terminal,
            RpcResult::SessionsList {
                sessions,
                terminal_snapshot_token,
                ..
            },
        ) => {
            // Without the one-time token the snapshot can never be admitted;
            // v2 redials rather than publishing it (`:61-65`).
            let Some(token) = terminal_snapshot_token.clone() else {
                tracing::warn!(target: "sync", "sessions snapshot carried no token; redialing");
                request_link_replacement(store, "manual", out);
                return true;
            };
            store.sessions.apply_snapshot(sessions.clone());
            Some(token)
        }
        (
            SyncDomain::Workers,
            RpcResult::WorkersList {
                workers,
                routable_fps,
                ..
            },
        ) => {
            crate::store::replace_workers(store, workers.clone());
            store.routable_worker_fps = Some(routable_fps.clone());
            None
        }
        (SyncDomain::Workspaces, RpcResult::WorkspacesList { workspaces, .. }) => {
            store.workspaces = workspaces.clone();
            None
        }
        (SyncDomain::Tasks, RpcResult::TasksList { tasks, .. }) => {
            store.tasks = tasks.clone();
            None
        }
        (SyncDomain::Mcp, RpcResult::McpList { relays, .. }) => {
            store.mcp_relays = relays.clone();
            None
        }
        (SyncDomain::Pair, RpcResult::PairList { requests, .. }) => {
            store.pair_requests = requests.clone();
            None
        }
        (domain, other) => {
            tracing::error!(
                target: "sync",
                domain = domain.as_str(),
                answer = other.kind_name(),
                "hydration answered with the wrong message"
            );
            schedule_retry(store, &ticket, now_ms);
            return true;
        }
    };
    store.note_change();
    publish_domain(store, &ticket, token, now_ms, out);
    true
}

/// Close the snapshot/live gap for a domain whose rows were just applied.
fn publish_domain(
    store: &mut Store,
    ticket: &HydrationTicket,
    token: Option<String>,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    if let Some(token) = &token {
        store
            .sync
            .issue_snapshot_token(ticket.socket_generation, ticket.domain, token.clone());
    }
    if let Err(reason) =
        store
            .sync
            .mark_domain_ready(ticket.socket_generation, ticket.domain, token.as_deref())
    {
        tracing::warn!(target: "sync", domain = ticket.domain.as_str(), reason, "snapshot not published");
        schedule_retry(store, ticket, now_ms);
        return;
    }
    store.sync.hydrations.succeeded(ticket.domain);
    out.push(Effect::SendSync(SyncCommand::DomainReady {
        domain: ticket.domain,
        generation: ticket.domain_generation,
        snapshot_token: token,
    }));
    tracing::info!(
        target: "sync",
        domain = ticket.domain.as_str(),
        domain_generation = ticket.domain_generation,
        "domain ready"
    );
    if ticket.domain == SyncDomain::Terminal {
        mark_protected_snapshot_published(store);
        super::hydrate(store, now_ms, out);
        crate::handle_sweep::republish_open_views(store, now_ms, out);
    }
}

/// Deadlines and due retries, once per sweep.
pub(crate) fn sweep_hydrations(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    for ticket in store.sync.hydrations.take_expired(now_ms) {
        tracing::warn!(
            target: "sync",
            domain = ticket.domain.as_str(),
            deadline_ms = SYNC_HYDRATION_DEADLINE_MS,
            "domain hydration exceeded its deadline"
        );
        if ticket_is_current(store, &ticket) {
            schedule_retry(store, &ticket, now_ms);
        }
    }
    for (domain, socket_generation, domain_generation) in
        store.sync.hydrations.take_due_retries(now_ms)
    {
        let still_current = store.sync.link_generation() == Some(socket_generation)
            && store.sync.domain_generation(domain) == Some(domain_generation);
        if still_current {
            trigger_hydration(store, domain, now_ms, out);
        }
    }
}

/// Ask whether this device is known at all when `subscribed` never came
/// (v2 `_bootstrap`'s pre-barrier probe). The answer is never published.
pub(crate) fn start_bootstrap_probe(store: &mut Store, out: &mut Vec<Effect>) {
    let call_id = store.next_call_id();
    store.sync.hydrations.begin_probe(call_id);
    tracing::info!(target: "sync", call_id, "no subscribed yet; probing the device");
    out.push(Effect::Rpc(RpcCall::SessionsList {
        call_id,
        sync_socket_id: None,
    }));
}

fn settle_probe(store: &mut Store, result: &RpcResult) {
    if let RpcResult::Failed { error, .. } = result {
        if error.auth_failure_kind("SessionsList") == AuthFailureKind::Device {
            mark_browser_device_rejected(store, "bootstrap_probe");
            return;
        }
        tracing::info!(target: "sync", %error, "bootstrap probe failed; the redial loop keeps trying");
    }
}

/// Whether a ticket still names the live socket and the domain's current,
/// subscribed, not-yet-ready generation (v2 `isCurrentSyncDomainToken`).
fn ticket_is_current(store: &Store, ticket: &HydrationTicket) -> bool {
    store.sync.link_generation() == Some(ticket.socket_generation)
        && store.sync.domains.get(&ticket.domain).is_some_and(|state| {
            state.generation == ticket.domain_generation && state.subscribed && !state.ready
        })
}

fn schedule_retry(store: &mut Store, ticket: &HydrationTicket, now_ms: u64) {
    let delay_ms = store.sync.hydrations.schedule_retry(ticket, now_ms);
    tracing::info!(
        target: "sync",
        domain = ticket.domain.as_str(),
        delay_ms,
        "domain hydration retry scheduled"
    );
}

/// Replace the live socket through the redial loop.
pub(crate) fn request_link_replacement(store: &mut Store, reason: &str, out: &mut Vec<Effect>) {
    let Some(generation) = store.sync.link_generation() else {
        return;
    };
    if store.sync.request_redial(generation) {
        store.sync.redial.note_abort_reason(reason);
        out.push(Effect::CloseSyncLink {
            generation,
            reason: reason.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::MemoryKeyValueStore;
    use crate::sync::SyncState;

    #[test]
    fn a_refused_publication_sends_no_domain_ready() {
        // No socket announced this domain, so the store refuses to mark it ready.
        let mut store = Store::new(SyncState::new(&MemoryKeyValueStore::new()), "tab-1");
        let ticket = HydrationTicket {
            domain: SyncDomain::Workers,
            domain_generation: 1,
            socket_generation: 1,
            deadline_ms: 0,
        };
        let mut out = Vec::new();
        publish_domain(&mut store, &ticket, None, 0, &mut out);
        assert!(
            !out.iter()
                .any(|effect| matches!(effect, Effect::SendSync(SyncCommand::DomainReady { .. }))),
            "{out:?}"
        );
        assert!(!store.sync.domain_is_ready(SyncDomain::Workers));
    }
}
