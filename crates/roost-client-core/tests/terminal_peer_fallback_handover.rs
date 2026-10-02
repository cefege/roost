//! Where a faulted peer hands the session, and when the attempt is over.
//!
//! This is the property the direct-terminal oracle exists to prove: a fault
//! hands the session to the other transport WITHOUT recreating the PTY. Painted
//! output and trusted input are one thing here — a fault may not re-dial Sync,
//! close the Sync link, or re-subscribe a domain
//! (`smoke/terminal/terminal-peer.spec.ts:263`).
//!
//! Also the lifecycle half: a dead attempt is dead, so a message that names it
//! must change nothing, and a worker that was taken off the roster must not
//! attract a fresh one.
//!
//! Depends on `tests/terminal_peer_fallback_support` for the described machine,
//! the two SDPs, the fault/handover readers, and the session-authority oracle.

mod terminal_peer_fallback_support;
use roost_client_core::client::carriers::{
    CarrierFault, FallbackReason, FaultFallback, GrantPhase, PeerPhase, PeerSignalling,
    SignallingInput,
};
use terminal_peer_fallback_support::{
    EPOCH, FIRST_PEER_ID, GRANT_TTL_MS, NOW, assert_fell_back_without_reopening_the_session,
    assert_no_session_teardown, authenticating, demand, faults, handovers, peer_open, ready_with,
    unreadable_sdp,
};

#[test]
#[rustfmt::skip]
fn invalid_offers_unavailable_grants_expired_grants_and_identity_mismatches_fall_back_without_recreating_the_pty() {
    // `invalid_sdp`: the worker refuses the offer as unreadable.
    let (mut invalid, attempt_id) = peer_open();
    let from_offer = invalid.step(SignallingInput::OfferReady {
        attempt_id,
        peer_id: FIRST_PEER_ID.to_string(),
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

    // The coordinator relaying a worker's refusal names no attempt: it is the
    // coordinator's failure, and v2 still records it as `network_failed`.
    let (mut relayed, _) = authenticating();
    let from_coordinator = relayed.step(SignallingInput::AttemptRefused {
        attempt_id: None,
        reason: "Unavailable: terminal peer worker rejected negotiation: invalid_offer".to_string(),
    });
    assert_eq!(faults(&from_coordinator), vec![CarrierFault::CoordinatorUnavailable]);
    assert_fell_back_without_reopening_the_session(&from_coordinator);

    // The smoke spec waits on ONE coarse reason for all four, because v2 reports
    // one; the machine reports that coarse reason AND the rule that fired, which
    // is what makes the four separately testable in terminal_peer_fallback_grants.
    for peer in [invalid, missing, expired, mismatch, relayed] {
        assert_eq!(
            peer.snapshot().fallback_reason,
            Some(FallbackReason::NetworkFailed),
            "v2 reports network_failed for every negotiation failure"
        );
    }
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
