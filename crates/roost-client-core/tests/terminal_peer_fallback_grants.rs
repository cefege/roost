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
    CarrierEffect, CarrierFault, GrantPhase, PeerSignalling, SignallingInput,
};
use terminal_peer_fallback_support::{
    GRANT_TTL_MS, NOW, assert_fell_back_without_reopening_the_session, authenticating, faults,
    peer_open, ready_with, unreadable_sdp,
};

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
