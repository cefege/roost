//! The store half of the terminal diagnostic snapshot: one session's elected
//! view, replica watermarks, last wire frame, smoke fault counts, Sync
//! generation and route, in v2's `TerminalStreamDiagnosticsSnapshot` shape.
//! Native; read by `smoke::browser_snapshot`. Ports
//! `apps/web/src/store/terminal-stream-diagnostics.ts:77-190,269-338`.

use roost_client_core::Store;
use roost_client_core::sync::SyncDomain;
use roost_client_core::terminal::input::InputPhase;
use roost_client_core::terminal::{
    TerminalSession, TerminalToken, TerminalTransport, TerminalView, ViewIntent,
};
use roost_protocol::viewport::TERMINAL_VIEW_LEASE_MS;
use serde_json::{Value, json};

/// The two clocks one snapshot is read against: the store's monotonic clock,
/// which every recorded instant in the store uses, and the wall clock v2's
/// `Date.now()` fields report in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiagnosticClocks {
    /// The store clock (`performance.now()`), in milliseconds.
    pub monotonic_ms: u64,
    /// `Date.now()`, in milliseconds.
    pub epoch_ms: f64,
}

/// `terminalStreamDiagnosticSnapshot(sessionId)`: the `view`, `replica`,
/// `wire_received`, `faults`, `sync` and `route` members, keyed by name.
pub fn terminal_stream_diagnostics(
    store: &Store,
    session_id: &str,
    clocks: DiagnosticClocks,
) -> serde_json::Map<String, Value> {
    let replica = store.terminal(session_id);
    let faults = store.terminal_smoke_faults.counts(session_id);
    let mut out = serde_json::Map::new();
    out.insert("view".into(), view_json(store, replica, clocks));
    out.insert("replica".into(), replica_json(store, replica, clocks));
    out.insert(
        "wire_received".into(),
        json!({
            "stream_id": replica.and_then(|replica| replica.wire_stream_id.clone()),
            "grid_epoch": replica.and_then(|replica| replica.wire_grid_epoch.clone()),
            "seq": replica.and_then(|replica| replica.wire_seq),
        }),
    );
    out.insert(
        "faults".into(),
        json!({
            "blackhole_drop_count": faults.blackhole_drop_count,
            "wire_delta_drop_count": faults.wire_delta_drop_count,
            "wire_delta_dropped_seq": faults.wire_delta_dropped_seq,
            "wire_delta_post_drop_seq": faults.wire_delta_post_drop_seq,
        }),
    );
    out.insert("sync".into(), sync_json(store));
    out.insert("route".into(), route_json(store, session_id, replica));
    out
}

/// v2's `TerminalTransportKind` spelling: the peer carrier is `webrtc` there.
pub fn transport_kind(transport: TerminalTransport) -> &'static str {
    match transport {
        TerminalTransport::Sync => "sync",
        TerminalTransport::Loopback => "loopback",
        TerminalTransport::Peer => "webrtc",
    }
}

/// `TerminalGenerationDiagnosticToken`. The socket id is the live link's
/// when the token names the live socket; a token for an older socket has no
/// recorded id, which is `null` rather than a guessed one.
pub fn generation_json(store: &Store, token: Option<&TerminalToken>) -> Value {
    let Some(token) = token else {
        return Value::Null;
    };
    let socket_id = (store.sync.link_generation() == Some(token.socket_generation))
        .then(|| store.sync.socket_id())
        .flatten();
    json!({
        "socketGeneration": token.socket_generation,
        "socketId": socket_id,
        "processEpoch": token.process_epoch,
        "domainGeneration": token.domain_generation.to_string(),
        "transportKind": transport_kind(token.transport),
        "workerFp": token.worker_fp,
    })
}

fn age_ms(clocks: DiagnosticClocks, started_at_ms: u64) -> u64 {
    clocks.monotonic_ms.saturating_sub(started_at_ms)
}

/// The view a resync would name, as v2 prefers the active resync view.
fn view_json(store: &Store, replica: Option<&TerminalSession>, clocks: DiagnosticClocks) -> Value {
    let Some((replica, view)) = replica.and_then(|replica| Some((replica, replica.repair_view()?)))
    else {
        return json!({
            "view_id": null, "revision": null, "active": false, "status": null,
            "stream_id": null, "effective_cols": null, "effective_rows": null,
            "lease_deadline_ms": null, "pending_ack_age_ms": null,
            "pending_ack_generation": null,
        });
    };
    let active = matches!(view.intent, ViewIntent::Publish { .. });
    let status = view_status(view);
    let (stream_id, cols, rows) = match (status, active) {
        (Some("accepted"), true) => {
            let (cols, rows) = replica.effective_geometry();
            (
                replica.expected_stream_id().map(str::to_owned),
                Some(cols),
                Some(rows),
            )
        }
        (Some("accepted"), false) => (Some(String::new()), Some(0), Some(0)),
        _ => (None, None, None),
    };
    // v2 stamps the deadline in `Date.now()` time when an active intent is
    // sent; the store records the send on its monotonic clock.
    let lease_deadline_ms = (active && view.published_at_ms > 0).then(|| {
        clocks.epoch_ms - age_ms(clocks, view.published_at_ms) as f64
            + TERMINAL_VIEW_LEASE_MS as f64
    });
    let pending_generation = view.unacknowledged.and_then(|generation| {
        replica
            .generation()
            .filter(|token| token.domain_generation == generation)
    });
    json!({
        "view_id": view.view_id,
        "revision": view.revision.to_string(),
        "active": active,
        "status": status,
        "stream_id": stream_id,
        "effective_cols": cols,
        "effective_rows": rows,
        "lease_deadline_ms": lease_deadline_ms,
        "pending_ack_age_ms": view
            .unacknowledged
            .map(|_| age_ms(clocks, view.published_at_ms)),
        "pending_ack_generation": generation_json(store, pending_generation),
    })
}

/// `TerminalViewHandleStatus["status"]`: the answer to the CURRENT revision,
/// `pending` once an intent is out without one, `null` before any publish.
pub fn view_status(view: &TerminalView) -> Option<&'static str> {
    match view.answer {
        Some(answer) if answer.revision == view.revision => Some(if answer.accepted {
            "accepted"
        } else {
            "rejected"
        }),
        Some(_) => Some("pending"),
        None if view.unacknowledged.is_some() || view.published_at_ms > 0 => Some("pending"),
        None => None,
    }
}

/// The replica watermarks. The proof-challenge ladder is authority-side in
/// v3 (`roost_client_core::terminal::repair`), so its client members report
/// no challenge and no attempts.
fn replica_json(
    store: &Store,
    replica: Option<&TerminalSession>,
    clocks: DiagnosticClocks,
) -> Value {
    let canonical = replica.and_then(TerminalSession::canonical);
    let latch = replica.map(TerminalSession::latch).filter(|latch| latch.is_latched());
    json!({
        "expected_stream_id": replica.and_then(TerminalSession::expected_stream_id),
        "grid_epoch": canonical.map(|frame| frame.grid_epoch.clone()),
        "seq": canonical.map(|frame| frame.seq),
        "baseline_ready": replica.is_some_and(TerminalSession::baseline_ready),
        "resync_latched": replica.is_some_and(TerminalSession::repair_latched),
        "last_terminal_proof_age_ms": null,
        "last_terminal_proof_generation": null,
        "challenge_age_ms": null,
        "challenge_generation": null,
        "challenge_stream_id": null,
        "challenge_seq": null,
        "resync_latch_age_ms": latch.map(|latch| age_ms(clocks, latch.latched_at_ms())),
        "resync_latch_generation": generation_json(store, latch.and_then(|latch| latch.token())),
        "repair_attempts": 0,
        "repair_outcome": "none",
    })
}

/// The live socket's terminal generation, as v2's `currentSyncV2TerminalState`.
fn sync_json(store: &Store) -> Value {
    let token = store.sync_terminal_token();
    json!({
        "socket_generation": store.sync.link_generation(),
        "socket_id": store.sync.socket_id(),
        "process_epoch": token.map(|token| token.process_epoch),
        "domain_generation": store
            .sync
            .domain_generation(SyncDomain::Terminal)
            .map(|generation| generation.to_string()),
        "ready": store.sync.domain_is_ready(SyncDomain::Terminal),
    })
}

/// One `TerminalRouteDiagnosticEntry`. No carrier publishes worker-control
/// telemetry in this client, so the timing members are `null`.
fn route_entry(transport: TerminalTransport, phase: &str) -> Value {
    json!({
        "kind": transport_kind(transport),
        "worker_epoch": null,
        "peer_id": null,
        "phase": phase,
        "candidate_type": "none",
        "probe_age_ms": null,
        "rtt_ms": null,
        "worker_control_rtt_ms": null,
        "buffered_bytes": null,
    })
}

fn route_json(store: &Store, session_id: &str, replica: Option<&TerminalSession>) -> Value {
    let worker_fp = replica.map(|replica| replica.worker_fp.as_str());
    let owned_by_worker =
        |token: &TerminalToken| worker_fp.is_some() && token.worker_fp.as_deref() == worker_fp;
    let active_direct = store
        .routes
        .route(session_id)
        .filter(|route| owned_by_worker(&route.token));
    let active = match (active_direct, replica.and_then(TerminalSession::generation)) {
        (Some(route), _) => route_entry(route.token.transport, "active"),
        (None, Some(token)) if token.transport == TerminalTransport::Sync => {
            route_entry(TerminalTransport::Sync, "active")
        }
        _ => Value::Null,
    };
    let candidate = store
        .routes
        .candidate(session_id)
        .filter(|candidate| owned_by_worker(&candidate.token))
        .map_or(Value::Null, |candidate| {
            route_entry(candidate.token.transport, "candidate")
        });
    let lane = store.input.lane(session_id);
    json!({
        "active": active,
        "candidate": candidate,
        "peer_phase": null,
        "fallback_reason": null,
        "failure_detail": null,
        "input_phase": lane.map(|lane| input_phase(lane.phase)),
        "pending_input_count": store.input.outstanding(session_id).len(),
    })
}

fn input_phase(phase: InputPhase) -> &'static str {
    match phase {
        InputPhase::Sending => "sending",
        InputPhase::Holding => "holding",
        InputPhase::Claiming => "claiming",
        InputPhase::Blocked => "blocked",
        InputPhase::Closed => "closed",
    }
}
