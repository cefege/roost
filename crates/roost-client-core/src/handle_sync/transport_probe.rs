//! Content-free worker control probes on a session's Sync or loopback route:
//! the request that sends one, and the answer that turns it into a round trip.
//!
//! Called from `handle_event` (`ClientEvent::TransportProbeRequested`), the
//! Sync fold and `handle_direct_frame`. A peer route's probes are its
//! heartbeat's, which the host's peer lane owns. Ports v2
//! `apps/web/src/store/transport/sync-terminal-control-probe.ts` and the
//! loopback connection's `probe`.

use crate::effect::{DirectCommand, Effect, SyncCommand};
use crate::store::Store;
use crate::store::sync_feeds::{
    PENDING_TRANSPORT_PROBES_MAX, PROBE_TELEMETRY_WORKERS_MAX, PendingTransportProbe, ProbeRoute,
    ProbeTelemetry, TRANSPORT_PROBE_TIMEOUT_MS,
};
use crate::sync::SyncDomain;
use crate::sync::inbound::TransportProbeResult;
use crate::terminal::token::TerminalTransport;

/// v2 `probeSyncTerminalWorker` refuses a worker identity longer than this.
const WORKER_FP_MAX_LEN: usize = 128;

/// Send one probe to the worker serving `session_id`, on the route its input
/// takes: the elected loopback carrier, else the ready Sync socket.
///
/// Nothing is sent, and nothing is recorded, when the session has no worker,
/// its route is a peer (whose heartbeat probes it), Sync is not ready, the id
/// is empty or already waited on, or 32 probes are already outstanding.
pub fn request_transport_probe(
    store: &mut Store,
    session_id: &str,
    request_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    store.pending_transport_probes.retain(|_, pending| {
        now_ms.saturating_sub(pending.started_at_ms) < TRANSPORT_PROBE_TIMEOUT_MS
    });
    let worker_fp = store
        .terminal(session_id)
        .map(|replica| replica.worker_fp.clone())
        .unwrap_or_default();
    let refusal = if worker_fp.is_empty() || worker_fp.len() > WORKER_FP_MAX_LEN {
        Some("the session has no valid worker")
    } else if request_id.is_empty() || store.pending_transport_probes.contains_key(request_id) {
        Some("the request id is empty or already waited on")
    } else if store.pending_transport_probes.len() >= PENDING_TRANSPORT_PROBES_MAX {
        Some("terminal control probe capacity reached")
    } else {
        None
    };
    if let Some(reason) = refusal {
        tracing::info!(target: "sync", session_id, request_id, reason, "transport probe not sent");
        return;
    }
    let (route, effect) = match store.routes.route(session_id).map(|route| &route.token) {
        Some(token) if token.transport == TerminalTransport::Peer => {
            tracing::debug!(target: "sync", session_id, "a peer route is probed by its heartbeat");
            return;
        }
        Some(token) => (
            ProbeRoute::Direct(token.clone()),
            Effect::SendDirect {
                token: token.clone(),
                command: DirectCommand::TransportProbe {
                    session_id: session_id.to_owned(),
                    request_id: request_id.to_owned(),
                    worker_fp: worker_fp.clone(),
                },
            },
        ),
        None => {
            let Some(token) = store
                .sync_terminal_token()
                .filter(|_| store.sync.domain_is_ready(SyncDomain::Terminal))
            else {
                tracing::info!(target: "sync", session_id, "terminal Sync is not connected");
                return;
            };
            (
                ProbeRoute::Sync {
                    socket_generation: token.socket_generation,
                },
                Effect::SendSync(SyncCommand::TerminalTransportProbe {
                    request_id: request_id.to_owned(),
                    worker_fp: worker_fp.clone(),
                }),
            )
        }
    };
    store.pending_transport_probes.insert(
        request_id.to_owned(),
        PendingTransportProbe {
            worker_fp,
            route,
            started_at_ms: now_ms,
        },
    );
    out.push(effect);
}

/// Settle one probe answer that arrived on `route`. An answer nothing on this
/// connection is waiting for, from another worker, or with no epoch (the
/// coordinator's refusal) never becomes telemetry (`resolveControlProbe`).
pub(super) fn fold_transport_probe_result(
    store: &mut Store,
    route: &ProbeRoute,
    result: &TransportProbeResult,
    now_ms: u64,
) {
    let answers_pending = store
        .pending_transport_probes
        .get(&result.request_id)
        .is_some_and(|pending| pending.worker_fp == result.worker_fp && pending.route == *route);
    if !answers_pending || result.worker_epoch.is_empty() {
        tracing::debug!(
            target: "sync",
            worker_fp = %result.worker_fp,
            request_id = %result.request_id,
            "transport probe answer settles nothing"
        );
        return;
    }
    let Some(pending) = store.pending_transport_probes.remove(&result.request_id) else {
        return;
    };
    if !store.transport_probes.contains_key(&result.worker_fp)
        && store.transport_probes.len() >= PROBE_TELEMETRY_WORKERS_MAX
    {
        let oldest = store
            .transport_probes
            .iter()
            .min_by_key(|(_, sample)| sample.received_at_ms)
            .map(|(worker_fp, _)| worker_fp.clone());
        if let Some(oldest) = oldest {
            store.transport_probes.remove(&oldest);
        }
    }
    let control_rtt_ms = now_ms.saturating_sub(pending.started_at_ms);
    store.transport_probes.insert(
        result.worker_fp.clone(),
        ProbeTelemetry {
            request_id: result.request_id.clone(),
            worker_epoch: result.worker_epoch.clone(),
            route: pending.route,
            received_at_ms: now_ms,
            control_rtt_ms,
        },
    );
    store.note_change();
    tracing::debug!(
        target: "sync",
        worker_fp = %result.worker_fp,
        control_rtt_ms,
        "transport probe answered"
    );
}
