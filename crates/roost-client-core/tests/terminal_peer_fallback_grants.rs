//! What each of the four direct-carrier faults does to the CREDENTIAL.
//!
//! A negotiation failure says one of four things about the secret the attempt
//! was presenting: nothing at all (the offer was unreadable), or one of three
//! verdicts — the worker no longer holds it, its own deadline has passed, or it
//! is perfectly good and the exchange is what failed. Getting that wrong in
//! either direction is visible to a user as a wait for a mint that was not
//! needed, or a credential kept past the point where it can authenticate
//! anything.
//!
//! Depends on `tests/terminal_peer_fallback_support` for the described machine,
//! the two SDPs, and the shared no-reopen assertion.

mod terminal_peer_fallback_support;
use roost_client_core::Effect;
use roost_client_core::client::carriers::{
    CarrierEffect, CarrierFault, FallbackReason, GrantInput, GrantPhase, PeerSignalling,
    SignallingInput,
};
use terminal_peer_fallback_support::{
    FIRST_PEER_ID, GRANT_TTL_MS, NOW, assert_fell_back_without_reopening_the_session,
    authenticating, demand, faults, grant, machine, peer_open, ready_with, unreadable_sdp,
};

#[test]
fn an_invalid_offer_keeps_the_grant_and_falls_back_to_sync() {
    let (mut peer, attempt_id) = peer_open();
    let effects = peer.step(SignallingInput::OfferReady {
        attempt_id,
        peer_id: FIRST_PEER_ID.to_string(),
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

/// The coordinator refuses an offer spent on a grant a restarted worker's new
/// epoch revoked. Keeping that credential would spend it on every retry and
/// never reach the new process; any other coordinator failure says nothing
/// about the credential.
#[test]
fn a_coordinator_grant_denial_drops_the_held_grant_and_other_refusals_keep_it() {
    let (mut denied, _) = authenticating();
    let effects = denied.step(SignallingInput::AttemptRefused {
        attempt_id: None,
        reason: "PermissionDenied: terminal peer grant is unavailable".to_string(),
    });
    assert_eq!(faults(&effects), vec![CarrierFault::GrantUnavailable]);
    assert_eq!(denied.snapshot().grant_phase, GrantPhase::Requested);
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            CarrierEffect::Core(Effect::RequestDirectGrant { .. })
        )),
        "a credential the coordinator denies is re-minted; got {effects:?}"
    );

    let (mut unreachable, _) = authenticating();
    let effects = unreachable.step(SignallingInput::AttemptRefused {
        attempt_id: None,
        reason: "Unavailable: coordinator is restarting".to_string(),
    });
    assert_eq!(faults(&effects), vec![CarrierFault::CoordinatorUnavailable]);
    assert_eq!(unreachable.snapshot().grant_phase, GrantPhase::Granted);
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

/// A worker that will not offer the carrier keeps SAYING so, across the gates.
///
/// `start` runs on every tick, and two of its branches only hold the attempt
/// while something else decides: the loopback probe, and the grant lifecycle.
/// Both used to park with no reason, so the one fact an operator can act on —
/// this worker does not do peer carriers at all — was true for a tick and gone
/// by the time anybody looked at it.
#[test]
fn a_worker_without_the_peer_capability_keeps_saying_disabled_across_the_gates() {
    let mut peer = machine();
    peer.step(demand());
    peer.step(SignallingInput::LocalDoorAnswered {
        worker_fp: String::new(),
    });
    let mut without_peers = grant();
    without_peers.peer_supported = false;
    peer.step(SignallingInput::Grant(GrantInput::Minted(without_peers)));
    assert_eq!(
        faults(&peer.step(demand())),
        vec![CarrierFault::Disabled],
        "a grant that cannot open a peer is the worker's own refusal"
    );
    assert_eq!(
        peer.snapshot().fallback_reason,
        Some(FallbackReason::Disabled)
    );

    peer.step(SignallingInput::Grant(GrantInput::Refused {
        now_ms: NOW,
        reason: "peer_unavailable".to_string(),
    }));
    assert_eq!(
        peer.snapshot().fallback_reason,
        Some(FallbackReason::Disabled),
        "waiting for the next grant is not a reason to forget this one"
    );
}
