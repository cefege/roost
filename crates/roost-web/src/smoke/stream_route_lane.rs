//! The signalling lane's half of a terminal route diagnostic: the fields a
//! negotiated peer produces, and the ones the lane owns outright.
//!
//! Owned by `smoke`, read by `super::stream_diagnostics`. It is a PROJECTION and
//! nothing else: every value here comes out of
//! `roost_client_core::client::carriers::CarrierLane`, which is the only writer.
//! A file that computed a round trip or inferred a candidate would be a second
//! source of truth about the same connection, and the reader could not tell
//! which of the two it was reading.

use serde_json::Value;

use roost_client_core::client::carriers::{CarrierLane, GrantPhase, PeerPhase, PeerTelemetry};

/// The lane's answers about one worker, in the diagnostic's spelling.
///
/// Typed as `Value` because the diagnostic is assembled with `serde_json::json!`
/// and a struct of `Option<&str>` would have to be unwrapped field by field at
/// the one call site — which is how a `null` and a `""` end up meaning the same
/// thing.
#[derive(Debug)]
pub struct LaneFields {
    /// Where the attempt is, or `null` for a worker with no machine.
    pub peer_phase: Value,
    /// The coarse recorded reason, or `null`.
    pub fallback_reason: Value,
    /// The host's own last detail, never the value that failed to match.
    pub failure_detail: Value,
    /// Whether pre-warm holds the worker's peer ready; `false` with no machine.
    pub prewarmed: bool,
}

/// The telemetry fields of a route entry, keyed by the name the diagnostic
/// publishes.
///
/// `is_peer` is the gate, and it is passed rather than inferred because the
/// caller already knows the route's transport and re-deciding it here would be a
/// second place that could answer "is this a peer route" differently. Off a peer
/// route every value is the documented absence: `null` for the measurements and
/// `"none"` for the candidate, because a loopback carrier has no ICE candidate
/// and a Sync route has no carrier at all. `time_to_direct_ms` is the machine's
/// own measurement of the wait that ended in this peer's election.
pub fn telemetry_fields(
    lane: &CarrierLane,
    worker_fp: &str,
    is_peer: bool,
    now_ms: u64,
) -> Vec<(String, Value)> {
    let (telemetry, time_to_direct_ms) = if is_peer {
        let snapshot = lane.snapshot(worker_fp);
        (snapshot.telemetry, snapshot.time_to_direct_ms)
    } else {
        (PeerTelemetry::default(), None)
    };
    let probe_age_ms = telemetry
        .last_probe_at_ms
        .map(|answered_ms| now_ms.saturating_sub(answered_ms));
    vec![
        ("peer_id".to_owned(), telemetry.peer_id.into()),
        (
            "candidate_type".to_owned(),
            telemetry.candidate_type.as_str().into(),
        ),
        ("probe_age_ms".to_owned(), probe_age_ms.into()),
        ("rtt_ms".to_owned(), telemetry.rtt_ms.into()),
        (
            "worker_control_rtt_ms".to_owned(),
            telemetry.worker_control_rtt_ms.into(),
        ),
        ("buffered_bytes".to_owned(), telemetry.buffered_bytes.into()),
        ("time_to_direct_ms".to_owned(), time_to_direct_ms.into()),
    ]
}

/// What the lane says about the worker serving a session.
pub fn lane_fields(lane: &CarrierLane, worker_fp: &str) -> LaneFields {
    if worker_fp.is_empty() {
        return absent();
    }
    let snapshot = lane.snapshot(worker_fp);
    // A machine with no view and no credential has nothing to say, and saying
    // `idle` would claim an election ran. This is the session that has always
    // been on Sync: a working state, reported as an absence.
    if snapshot.active_views == 0 && snapshot.grant_phase == GrantPhase::Absent {
        return absent();
    }
    LaneFields {
        peer_phase: peer_phase(snapshot.phase).into(),
        fallback_reason: snapshot
            .fallback_reason
            .map_or(Value::Null, |reason| reason.as_str().into()),
        failure_detail: snapshot.last_failure_detail.into(),
        prewarmed: snapshot.prewarmed,
    }
}

/// The fields for a worker this lane holds no machine for.
fn absent() -> LaneFields {
    LaneFields {
        peer_phase: Value::Null,
        fallback_reason: Value::Null,
        failure_detail: Value::Null,
        prewarmed: false,
    }
}

/// `PeerPhase`'s diagnostic spelling, which is v2's and not the enum's.
fn peer_phase(phase: PeerPhase) -> &'static str {
    match phase {
        PeerPhase::Idle => "idle",
        PeerPhase::AwaitingGrant => "awaiting_grant",
        PeerPhase::Gathering => "gathering",
        PeerPhase::Negotiating => "negotiating",
        PeerPhase::Authenticating => "authenticating",
        PeerPhase::Candidate => "candidate",
        PeerPhase::Active => "active",
        PeerPhase::Cooldown => "cooldown",
        PeerPhase::Disabled => "disabled",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use roost_client_core::client::carriers::CandidateType;

    use super::*;

    fn fields_of(lane: &CarrierLane, is_peer: bool) -> BTreeMap<String, Value> {
        telemetry_fields(lane, "worker-a", is_peer, 1_000)
            .into_iter()
            .collect()
    }

    /// A loopback route names no candidate and reports no round trip, and the
    /// snapshot says so with `null` rather than with a zero.
    #[test]
    fn a_non_peer_route_reports_the_documented_absence() {
        let mut lane = CarrierLane::new();
        let mut out = Vec::new();
        lane.demand("session-a", "worker-a", "view-1", true, 0, &mut out);
        lane.local_door_answered("worker-a", "worker-a", &mut out);
        let fields = fields_of(&lane, false);
        assert_eq!(fields["peer_id"], Value::Null);
        assert_eq!(fields["candidate_type"], Value::from("none"));
        assert_eq!(fields["rtt_ms"], Value::Null);
        assert_eq!(fields["buffered_bytes"], Value::Null);
    }

    /// A peer route reports what the transport measured, under the names the
    /// smoke reader compares.
    #[test]
    fn a_peer_route_reports_the_measured_telemetry() {
        let mut lane = CarrierLane::new();
        let mut out = Vec::new();
        lane.demand("session-a", "worker-a", "view-1", true, 0, &mut out);
        lane.local_door_answered("worker-a", "", &mut out);
        lane.record_telemetry(
            "worker-a",
            PeerTelemetry {
                peer_id: Some("peer-1".to_owned()),
                candidate_type: CandidateType::Host,
                last_probe_at_ms: Some(880),
                liveness_qualified: true,
                rtt_ms: Some(7),
                worker_control_rtt_ms: Some(9),
                buffered_bytes: Some(0),
            },
        );
        let fields = fields_of(&lane, true);
        assert_eq!(fields["peer_id"], Value::from("peer-1"));
        assert_eq!(fields["candidate_type"], Value::from("host"));
        assert_eq!(fields["probe_age_ms"], Value::from(120));
        assert_eq!(fields["rtt_ms"], Value::from(7));
        assert_eq!(fields["worker_control_rtt_ms"], Value::from(9));
        assert_eq!(fields["buffered_bytes"], Value::from(0));
    }

    /// A worker with no machine reports three absences, which is a working
    /// state and not a fault to be spelled out.
    #[test]
    fn a_worker_with_no_machine_reports_three_absences() {
        let lane = CarrierLane::new();
        let fields = lane_fields(&lane, "worker-a");
        assert_eq!(fields.peer_phase, Value::Null);
        assert_eq!(fields.fallback_reason, Value::Null);
        assert_eq!(fields.failure_detail, Value::Null);
    }

    /// A worker with a machine reports where the attempt is, so a snapshot is
    /// never `null` for a session that is actively asking for a credential.
    #[test]
    fn a_worker_with_a_machine_reports_its_phase() {
        let mut lane = CarrierLane::new();
        let mut out = Vec::new();
        lane.set_environment(true, 4);
        lane.demand("session-a", "worker-a", "view-1", true, 0, &mut out);
        let fields = lane_fields(&lane, "worker-a");
        // `idle`, not `awaiting_grant`: the machine is waiting on the LOOPBACK
        // probe, and the probe gate is asked before the grant gate. Reporting
        // `awaiting_grant` here would tell a reader the fast path had already
        // been ruled out, which is the opposite of what is true.
        assert_eq!(fields.peer_phase, Value::from("idle"));
        assert_eq!(
            lane.snapshot("worker-a").grant_phase,
            GrantPhase::Requested,
            "the machine is waiting on the credential it asked for"
        );
    }

    /// A page elsewhere with a live credential is the one state that reaches
    /// `gathering`, and the phase is what says an attempt exists at all.
    #[test]
    fn a_released_peer_reports_gathering() {
        let mut lane = CarrierLane::new();
        let mut out = Vec::new();
        lane.set_environment(true, 0);
        lane.demand("session-a", "worker-a", "view-1", true, 0, &mut out);
        lane.local_door_answered("worker-a", "", &mut out);
        lane.grant_minted(grant(), &mut out);
        assert_eq!(
            lane_fields(&lane, "worker-a").peer_phase,
            Value::from("gathering")
        );
    }

    fn grant() -> roost_client_core::client::carriers::DirectGrant {
        roost_client_core::client::carriers::DirectGrant {
            grant_id: "grant-a".to_owned(),
            secret: "secret-a".to_owned(),
            worker_fp: "worker-a".to_owned(),
            worker_epoch: "epoch-a".to_owned(),
            tab_id: "tab-a".to_owned(),
            device_fingerprint: "device-a".to_owned(),
            session_ids: std::collections::BTreeSet::from(["session-a".to_owned()]),
            peer_supported: true,
            input_route_supported: true,
            stun_urls: Vec::new(),
            expires_at_ms: u64::MAX,
        }
    }
}
