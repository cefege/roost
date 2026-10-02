//! The store half of the terminal diagnostic snapshot: one session's elected
//! view, replica watermarks, last wire frame, smoke fault counts, Sync
//! generation and route, in v2's `TerminalStreamDiagnosticsSnapshot` shape.
//! Native; read by `smoke::browser_snapshot`. Ports
//! `apps/web/src/store/terminal-stream-diagnostics.ts:77-190,269-338`.

use roost_client_core::Store;
use roost_client_core::store::sync_feeds::{ProbeRoute, ProbeTelemetry};
use roost_client_core::sync::SyncDomain;
use roost_client_core::terminal::input::InputPhase;
use roost_client_core::terminal::liveness::ForegroundLiveness;
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
    out.insert(
        "route".into(),
        route_json(store, session_id, replica, clocks.monotonic_ms),
    );
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

/// `TerminalGenerationDiagnosticToken`. A Sync token's socket id is the live
/// link's when the token names the live socket; a direct token's is the one its
/// carrier's `Ready` named, as v2's peer token carries it, so the proof keeps
/// its identity while the coordinator link drops and redials. A token nothing
/// presents any more has no recorded id, which is `null` rather than a guess.
pub fn generation_json(store: &Store, token: Option<&TerminalToken>) -> Value {
    let Some(token) = token else {
        return Value::Null;
    };
    let socket_id = match token.transport {
        TerminalTransport::Sync => (store.sync.link_generation() == Some(token.socket_generation))
            .then(|| store.sync.socket_id())
            .flatten(),
        TerminalTransport::Loopback | TerminalTransport::Peer => store
            .routes
            .carrier_presenting(token)
            .map(|carrier| carrier.socket_id.as_str()),
    };
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

/// The replica watermarks, the proof-challenge ladder, and the last frame this
/// replica accepted. The challenge members are the client's own
/// (`roost_client_core::terminal::liveness`), reported as they stand rather
/// than as `none`: a reader asking whether a pane was ever challenged cannot
/// tell a replica that was never watched from one whose answer was lost.
///
/// `last_terminal_proof_*` is NOT the challenge: it is the last frame this
/// replica accepted, which is the generation that last fed it — the whole
/// question it answers, with no second counter to drift.
fn replica_json(
    store: &Store,
    replica: Option<&TerminalSession>,
    clocks: DiagnosticClocks,
) -> Value {
    let canonical = replica.and_then(TerminalSession::canonical);
    let latch = replica
        .map(TerminalSession::latch)
        .filter(|latch| latch.is_latched());
    let liveness = replica.map(TerminalSession::liveness);
    json!({
        "expected_stream_id": replica.and_then(TerminalSession::expected_stream_id),
        "grid_epoch": canonical.map(|frame| frame.grid_epoch.clone()),
        "seq": canonical.map(|frame| frame.seq),
        "baseline_ready": replica.is_some_and(TerminalSession::baseline_ready),
        "resync_latched": replica.is_some_and(TerminalSession::repair_latched),
        "last_terminal_proof_age_ms": liveness
            .and_then(ForegroundLiveness::last_accepted_at_ms)
            .map(|accepted_at| age_ms(clocks, accepted_at)),
        "last_terminal_proof_generation": generation_json(store, replica.and_then(TerminalSession::generation)),
        "challenge_age_ms": liveness
            .and_then(ForegroundLiveness::challenged_at_ms)
            .map(|challenged_at| age_ms(clocks, challenged_at)),
        "challenge_generation": generation_json(
            store,
            liveness.and_then(ForegroundLiveness::challenge_generation),
        ),
        "challenge_stream_id": liveness.and_then(ForegroundLiveness::challenge_stream_id),
        "challenge_seq": liveness.and_then(ForegroundLiveness::challenge_seq),
        "resync_latch_age_ms": latch.map(|latch| age_ms(clocks, latch.latched_at_ms())),
        "resync_latch_generation": generation_json(store, latch.and_then(|latch| latch.token())),
        "repair_attempts": liveness.map_or(0, ForegroundLiveness::repair_attempts),
        "repair_outcome": liveness.map_or("none", |liveness| liveness.outcome().as_str()),
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

/// One `TerminalRouteDiagnosticEntry`, with the signalling lane's view of the
/// attempt beside it.
///
/// `worker_epoch` is the token's `process_epoch` — the coordinator's identity
/// for the worker PROCESS, which a restart changes and which is therefore the
/// field a reader compares across a restart.
///
/// A peer's five telemetry fields come from `super::stream_route_lane`, which
/// reads its heartbeat. A loopback or Sync route has no ICE candidate and no
/// carrier round trip of its own, so `candidate_type` stays `"none"` and only
/// the worker control probe answered on that exact connection
/// (`Store::transport_probes`) fills `probe_age_ms` and
/// `worker_control_rtt_ms`.
fn route_entry(
    store: &Store,
    transport: TerminalTransport,
    token: Option<&TerminalToken>,
    worker_fp: &str,
    now_ms: u64,
) -> Value {
    let mut entry = json!({
        "kind": transport_kind(transport),
        "worker_epoch": token.map(|token| token.process_epoch.clone()),
        "peer_id": null,
        "phase": if transport == TerminalTransport::Sync { "active" } else { "staged" },
        "candidate_type": "none",
        "probe_age_ms": null,
        "rtt_ms": null,
        "worker_control_rtt_ms": null,
        "buffered_bytes": null,
    });
    let is_peer = token.is_some_and(|token| token.transport == TerminalTransport::Peer);
    // The entry is built by `json!` from a literal, so it is an object; the
    // `if let` is there so a future edit that makes it something else fails to
    // compile rather than silently dropping the lane's half.
    if let Some(object) = entry.as_object_mut() {
        for (field, value) in
            super::stream_route_lane::telemetry_fields(&store.direct, worker_fp, is_peer, now_ms)
        {
            object.insert(field, value);
        }
        if let Some(sample) = token
            .filter(|_| !is_peer)
            .and_then(|token| control_probe_on(store, token, worker_fp))
        {
            object.insert(
                "probe_age_ms".into(),
                now_ms.saturating_sub(sample.received_at_ms).into(),
            );
            object.insert("worker_control_rtt_ms".into(), sample.control_rtt_ms.into());
        }
    }
    entry
}

/// The worker's newest control probe sample, when the connection `token`
/// names is the one that carried it.
fn control_probe_on<'store>(
    store: &'store Store,
    token: &TerminalToken,
    worker_fp: &str,
) -> Option<&'store ProbeTelemetry> {
    let sample = store.transport_probes.get(worker_fp)?;
    let carried = match &sample.route {
        ProbeRoute::Sync { socket_generation } => {
            token.transport == TerminalTransport::Sync
                && token.socket_generation == *socket_generation
        }
        ProbeRoute::Direct(direct) => direct == token,
    };
    carried.then_some(sample)
}

fn route_json(
    store: &Store,
    session_id: &str,
    replica: Option<&TerminalSession>,
    now_ms: u64,
) -> Value {
    let worker_fp = replica.map(|replica| replica.worker_fp.as_str());
    let owned_by_worker =
        |token: &TerminalToken| worker_fp.is_some() && token.worker_fp.as_deref() == worker_fp;
    let active_direct = store
        .routes
        .route(session_id)
        .filter(|route| owned_by_worker(&route.token));
    let serving = worker_fp.unwrap_or_default();
    let active = match (active_direct, replica.and_then(TerminalSession::generation)) {
        (Some(route), _) => route_entry(
            store,
            route.token.transport,
            Some(&route.token),
            serving,
            now_ms,
        ),
        (None, Some(token)) if token.transport == TerminalTransport::Sync => {
            route_entry(store, TerminalTransport::Sync, Some(token), serving, now_ms)
        }
        _ => Value::Null,
    };
    let candidate = store
        .routes
        .candidate(session_id)
        .filter(|candidate| owned_by_worker(&candidate.token))
        .map_or(Value::Null, |candidate| {
            route_entry(
                store,
                candidate.token.transport,
                Some(&candidate.token),
                serving,
                now_ms,
            )
        });
    let lane = store.input.lane(session_id);
    // The lane's two answers are reported whether or not a route is elected.
    // "No route, no attempt, and no reason" is the state that reads as healthy
    // in a snapshot and is not: it is what a machine with no credential, or one
    // waiting out a fault's cooldown, both look like from the route fields.
    let signalling =
        super::stream_route_lane::lane_fields(&store.direct, worker_fp.unwrap_or_default());
    json!({
        "active": active,
        "candidate": candidate,
        "peer_phase": signalling.peer_phase,
        "fallback_reason": signalling.fallback_reason,
        "failure_detail": signalling.failure_detail,
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
