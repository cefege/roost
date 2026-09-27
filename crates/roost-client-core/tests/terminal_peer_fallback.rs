//! The four direct-carrier faults, one rule each, and the property the
//! direct-terminal oracle exists to prove: a fault hands the session to the
//! other transport WITHOUT recreating the PTY. Painted output and trusted input
//! are one thing here — a fault may not re-dial Sync, close the Sync link, or
//! re-subscribe a domain (`smoke/terminal/terminal-peer.spec.ts:263`).

use roost_client_core::client::carriers::{
    CarrierEffect, CarrierEnvironment, CarrierFault, DirectGrant, FallbackReason, FaultFallback,
    GrantInput, GrantPhase, PeerAnswer, PeerPhase, PeerSignalling, ReadyTuple, Signalling,
    SignallingInput,
};
use roost_client_core::{Effect, SyncCommand};

const WORKER: &str = "worker-a";
const SESSION: &str = "session-a";
const EPOCH: &str = "epoch-a";
const SYNC_GENERATION: u64 = 7;
const NOW: u64 = 1_000;
const GRANT_TTL_MS: u64 = 60 * 60_000;
const FIRST_PEER_ID: &str = "peer-1";

fn usable_sdp() -> String {
    let fingerprint = vec!["AA"; 32].join(":");
    [
        "v=0",
        "o=- 1 1 IN IP4 127.0.0.1",
        "s=-",
        "t=0 0",
        "a=ice-ufrag:test",
        "a=ice-pwd:0123456789012345678901",
        &format!("a=fingerprint:sha-256 {fingerprint}"),
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
        "a=setup:actpass",
        "a=sctp-port:5000",
        "a=max-message-size:65536",
        "a=candidate:1 1 udp 1 192.0.2.1 5000 typ host",
        "",
    ]
    .join("\r\n")
}

/// An offer the worker cannot read: exactly what the smoke fault `invalid_sdp`
/// injects by replacing the SDP before the worker ever sees it.
fn unreadable_sdp() -> String {
    "smoke-invalid-sdp".to_string()
}

fn grant() -> DirectGrant {
    DirectGrant {
        grant_id: "grant-a".to_string(),
        secret: "secret-a".to_string(),
        worker_fp: WORKER.to_string(),
        worker_epoch: EPOCH.to_string(),
        tab_id: "tab-a".to_string(),
        device_fingerprint: "device-a".to_string(),
        session_ids: [SESSION.to_string()].into_iter().collect(),
        peer_supported: true,
        input_route_supported: true,
        stun_urls: Vec::new(),
        expires_at_ms: NOW + GRANT_TTL_MS,
    }
}

fn machine() -> Signalling {
    Signalling::new(
        WORKER,
        CarrierEnvironment {
            peers_allocated: 0,
            peer_transport_available: true,
            sync_generation: SYNC_GENERATION,
        },
        NOW,
    )
}

fn demand() -> SignallingInput {
    SignallingInput::Demand {
        session_id: SESSION.to_string(),
        view_id: "view-a".to_string(),
        active: true,
    }
}

fn answer_for(attempt_id: u64) -> SignallingInput {
    SignallingInput::AnswerReceived {
        attempt_id,
        answer: PeerAnswer {
            peer_id: FIRST_PEER_ID.to_string(),
            worker_epoch: EPOCH.to_string(),
            answer_sdp: usable_sdp(),
        },
    }
}

fn ready_with(worker_epoch: &str) -> ReadyTuple {
    ReadyTuple {
        worker_fp: WORKER.to_string(),
        worker_epoch: worker_epoch.to_string(),
        peer_id: FIRST_PEER_ID.to_string(),
        socket_generation: 7,
        socket_id: "socket-a".to_string(),
        session_ids: [SESSION.to_string()].into_iter().collect(),
    }
}

/// A machine whose peer is open, with the grant live and the page on a machine
/// that is not the worker's own. Returns the live attempt id.
fn peer_open() -> (Signalling, u64) {
    let mut peer = machine();
    peer.step(demand());
    peer.step(SignallingInput::Grant(GrantInput::Minted(grant())));
    peer.step(SignallingInput::LocalDoorAnswered {
        worker_fp: String::new(),
    });
    let attempt_id = match peer
        .step(SignallingInput::OfferReady {
            attempt_id: 1,
            offer_sdp: usable_sdp(),
        })
        .as_slice()
    {
        [CarrierEffect::NegotiateOffer { attempt_id, .. }] => *attempt_id,
        other => panic!("a page on another machine must negotiate a peer, got {other:?}"),
    };
    (peer, attempt_id)
}

/// A machine whose peer has answered and is authenticating.
fn authenticating() -> (Signalling, u64) {
    let (mut peer, attempt_id) = peer_open();
    peer.step(answer_for(attempt_id));
    (peer, attempt_id)
}

/// Whether any effect in `effects` would make the worker look like it had lost
/// the session.
///
/// THE property `smoke/terminal/terminal-peer.spec.ts:263` exists for. A PTY
/// lives in the worker and no client can recreate it, but a client that re-dials
/// Sync, closes the Sync link, or re-subscribes a domain drops the session's
/// metadata authority, and the worker's next frame then has nowhere to land. So
/// "the PTY is not recreated" is observable here as the absence of exactly those
/// three, and a fault that emitted one would fail this.
fn session_authority_teardowns(effects: &[CarrierEffect]) -> Vec<&'static str> {
    let mut torn = Vec::new();
    for effect in effects {
        let CarrierEffect::Core(core) = effect else {
            continue;
        };
        match core {
            Effect::DialSync { .. } => torn.push("dial_sync"),
            Effect::CloseSyncLink { .. } => torn.push("close_sync_link"),
            Effect::SendSync(SyncCommand::DomainReady { .. }) => torn.push("domain_ready"),
            Effect::SendSync(SyncCommand::Unsubscribe { .. }) => torn.push("unsubscribe"),
            _ => {}
        }
    }
    torn
}

fn faults(effects: &[CarrierEffect]) -> Vec<CarrierFault> {
    let named = |effect: &CarrierEffect| match effect {
        CarrierEffect::Faulted { fault, .. } => Some(*fault),
        _ => None,
    };
    effects.iter().filter_map(named).collect()
}

fn handovers(effects: &[CarrierEffect]) -> Vec<FaultFallback> {
    let named = |effect: &CarrierEffect| match effect {
        CarrierEffect::Fallback { transport, .. } => Some(*transport),
        _ => None,
    };
    effects.iter().filter_map(named).collect()
}

/// The session keeps its Sync authority. Named so a failure says which half of
/// the property broke.
fn assert_no_session_teardown(effects: &[CarrierEffect]) {
    assert_eq!(
        session_authority_teardowns(effects),
        Vec::<&str>::new(),
        "a direct-carrier fault must not re-open the session; got {effects:?}"
    );
}

/// The shared assertion: the session keeps its Sync authority and, with no
/// loopback carrier staged, is handed to Sync.
fn assert_fell_back_without_reopening_the_session(effects: &[CarrierEffect]) {
    assert_no_session_teardown(effects);
    assert_eq!(
        handovers(effects),
        vec![FaultFallback::Sync],
        "with no loopback carrier staged, the session is served by Sync; got {effects:?}"
    );
}

#[test]
#[rustfmt::skip]
fn invalid_offers_unavailable_grants_expired_grants_and_identity_mismatches_fall_back_without_recreating_the_pty() {
    // `invalid_sdp`: the worker refuses the offer as unreadable.
    let (mut invalid, attempt_id) = peer_open();
    let from_offer = invalid.step(SignallingInput::OfferReady {
        attempt_id,
        offer_sdp: unreadable_sdp(),
    });
    assert_eq!(faults(&from_offer), vec![CarrierFault::InvalidOffer]);
    assert_fell_back_without_reopening_the_session(&from_offer);

    // `missing_grant`: the worker has no live credential for this device and tab.
    let (mut missing, attempt_id) = authenticating();
    let from_refusal = missing.step(SignallingInput::AttemptRefused {
        attempt_id: Some(attempt_id),
        reason: "grant_unavailable".to_string(),
    });
    assert_eq!(faults(&from_refusal), vec![CarrierFault::GrantUnavailable]);
    assert_fell_back_without_reopening_the_session(&from_refusal);

    // `expired_grant`: the credential's own deadline passed.
    let (mut expired, _) = authenticating();
    let from_clock = expired.step(SignallingInput::Sweep {
        now_ms: NOW + GRANT_TTL_MS,
    });
    assert_eq!(faults(&from_clock), vec![CarrierFault::GrantExpired]);
    assert_fell_back_without_reopening_the_session(&from_clock);

    // `identity_mismatch`: the far end proved a tuple that is not this attempt's.
    let (mut mismatch, attempt_id) = authenticating();
    let from_ready = mismatch.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready_with("epoch-mismatch"),
    });
    assert_eq!(faults(&from_ready), vec![CarrierFault::IdentityMismatch]);
    assert_fell_back_without_reopening_the_session(&from_ready);

    // The smoke spec waits on ONE coarse reason for all four, because v2 reports
    // one; the machine reports that coarse reason AND the rule that fired, which
    // is what makes the four separately testable below.
    for peer in [invalid, missing, expired, mismatch] {
        assert_eq!(
            peer.snapshot().fallback_reason,
            Some(FallbackReason::NetworkFailed),
            "v2 reports network_failed for every negotiation failure"
        );
    }
}

#[test]
fn an_invalid_offer_keeps_the_grant_and_falls_back_to_sync() {
    let (mut peer, attempt_id) = peer_open();
    let effects = peer.step(SignallingInput::OfferReady {
        attempt_id,
        offer_sdp: unreadable_sdp(),
    });
    assert_eq!(faults(&effects), vec![CarrierFault::InvalidOffer]);
    assert!(
        CarrierFault::InvalidOffer.keeps_grant(),
        "an unreadable offer says nothing about the credential: it never became a peer"
    );
    assert_eq!(
        peer.snapshot().grant_phase,
        GrantPhase::Granted,
        "the grant survives an invalid offer; got {:?}",
        peer.snapshot().grant_phase
    );
    assert_fell_back_without_reopening_the_session(&effects);
    assert!(
        !peer.snapshot().has_carrier,
        "no carrier is held after a fault"
    );
}

#[test]
fn an_unavailable_grant_is_dropped_and_a_new_one_is_requested() {
    let (mut peer, attempt_id) = authenticating();
    let effects = peer.step(SignallingInput::AttemptRefused {
        attempt_id: Some(attempt_id),
        reason: "grant_unavailable".to_string(),
    });
    assert_eq!(faults(&effects), vec![CarrierFault::GrantUnavailable]);
    assert!(
        !CarrierFault::GrantUnavailable.keeps_grant(),
        "a credential the worker no longer holds cannot authenticate anything"
    );
    assert_eq!(peer.snapshot().grant_phase, GrantPhase::Requested);
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            CarrierEffect::Core(Effect::RequestDirectGrant { .. })
        )),
        "only a fresh mint replaces a credential the worker revoked; got {effects:?}"
    );
    assert_fell_back_without_reopening_the_session(&effects);
}

#[test]
fn an_expired_grant_is_dropped_on_the_clients_own_clock() {
    let (mut peer, _) = authenticating();
    let due = NOW + GRANT_TTL_MS;
    let early = peer.step(SignallingInput::Sweep { now_ms: due - 1 });
    assert!(
        !faults(&early).contains(&CarrierFault::GrantExpired),
        "a live grant is not expired early; got {early:?}"
    );
    let effects = peer.step(SignallingInput::Sweep { now_ms: due });
    assert_eq!(faults(&effects), vec![CarrierFault::GrantExpired]);
    assert_eq!(peer.snapshot().grant_phase, GrantPhase::Requested);
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            CarrierEffect::Core(Effect::RequestDirectGrant { .. })
        )),
        "a dead secret must be re-minted, never re-presented; got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, CarrierEffect::CloseAttempt { .. })),
        "the attempt that was using the dead secret is closed; got {effects:?}"
    );
    assert_fell_back_without_reopening_the_session(&effects);
}

#[test]
fn an_identity_mismatch_closes_the_attempt_and_keeps_the_grant() {
    let (mut peer, attempt_id) = authenticating();
    let effects = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready_with("epoch-mismatch"),
    });
    assert_eq!(faults(&effects), vec![CarrierFault::IdentityMismatch]);
    assert!(
        CarrierFault::IdentityMismatch.keeps_grant(),
        "a refused tuple is a fact about the exchange, not about the credential"
    );
    assert_eq!(
        peer.snapshot().grant_phase,
        GrantPhase::Granted,
        "discarding a good grant for a tuple fault would make the next attempt wait for a mint"
    );
    assert!(
        !effects.iter().any(|effect| matches!(
            effect,
            CarrierEffect::Core(Effect::RequestDirectGrant { .. })
        )),
        "a kept grant is not re-requested; got {effects:?}"
    );
    assert_fell_back_without_reopening_the_session(&effects);
    assert!(
        !peer.snapshot().has_carrier,
        "a refused tuple stages nothing"
    );
}

#[test]
fn a_staged_loopback_carrier_is_what_a_faulted_peer_hands_the_session_to() {
    let (mut peer, attempt_id) = authenticating();
    peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready_with(EPOCH),
    });
    // The loopback slice brings the fast path up while the peer is still held.
    peer.step(SignallingInput::LoopbackCarrierStaged { staged: true });
    let effects = peer.step(SignallingInput::IceFailed { attempt_id });
    assert_eq!(handovers(&effects), vec![FaultFallback::Loopback]);
    assert_eq!(
        peer.snapshot().fallback_reason,
        Some(FallbackReason::IceFailed)
    );
    assert_no_session_teardown(&effects);
}

#[test]
fn a_late_message_for_a_dead_attempt_is_discarded() {
    let (mut peer, attempt_id) = authenticating();
    peer.step(SignallingInput::AttemptRefused {
        attempt_id: Some(attempt_id),
        reason: "expired".to_string(),
    });
    for late in [
        SignallingInput::AttemptRefused {
            attempt_id: Some(attempt_id),
            reason: "grant_unavailable".to_string(),
        },
        SignallingInput::IceFailed { attempt_id },
    ] {
        let effects = peer.step(late);
        assert!(
            effects.is_empty(),
            "a message naming a dead attempt must change nothing; got {effects:?}"
        );
    }
    assert_eq!(peer.snapshot().phase, PeerPhase::Cooldown);
}

#[test]
fn does_not_recreate_a_peer_owner_after_explicit_worker_retirement() {
    let (mut peer, _) = authenticating();
    peer.step(SignallingInput::WorkerRetired);
    let after = peer.step(demand());
    assert!(
        after.is_empty(),
        "a removed worker must not attract a fresh attempt; got {after:?}"
    );
    assert_eq!(peer.snapshot().phase, PeerPhase::Disabled);
    assert_eq!(peer.snapshot().grant_phase, GrantPhase::Retired);
}
