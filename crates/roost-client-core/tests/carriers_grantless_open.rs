//! A peer transport opened while its first grant is minted: gathering overlaps
//! the mint's round trip on the STUN servers the coordinator advertised, and
//! the offer waits for the grant before it reaches the coordinator.
//!
//! Driven through `CarrierLane`, the surface the client core steps, with the
//! described environment from `terminal_peer_support`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_peer_support;

use roost_client_core::Effect;
use roost_client_core::client::carriers::{
    CarrierEffect, CarrierLane, PeerAttempt, PeerPhase, SignallingInput,
};
use terminal_peer_support::{EPOCH, FIRST_PEER_ID, SESSION, WORKER, grant_for, usable_sdp};

const DEMAND_MS: u64 = 1_000;
const OFFER_MS: u64 = 1_020;
const MINT_MS: u64 = 1_050;

fn carrier_effects(effects: &[Effect]) -> Vec<&CarrierEffect> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Carrier(inner) => Some(&**inner),
            _ => None,
        })
        .collect()
}

fn opened(effects: &[Effect]) -> Option<PeerAttempt> {
    carrier_effects(effects)
        .into_iter()
        .find_map(|effect| match effect {
            CarrierEffect::OpenTransport { attempt } => Some(attempt.clone()),
            _ => None,
        })
}

fn negotiated(effects: &[Effect]) -> Vec<PeerAttempt> {
    carrier_effects(effects)
        .into_iter()
        .filter_map(|effect| match effect {
            CarrierEffect::NegotiateOffer { attempt, .. } => Some(attempt.clone()),
            _ => None,
        })
        .collect()
}

fn closed(effects: &[Effect]) -> bool {
    carrier_effects(effects)
        .into_iter()
        .any(|effect| matches!(effect, CarrierEffect::CloseAttempt { .. }))
}

/// A lane on another machine, advertised STUN or not, with one view demanding
/// `SESSION`. Returns the lane and what the demand emitted.
fn demanded(stun_urls: Option<Vec<String>>) -> (CarrierLane, Vec<Effect>) {
    let mut lane = CarrierLane::new();
    lane.set_environment(true, 0);
    lane.set_stun_urls(stun_urls);
    let mut out = Vec::new();
    lane.local_door_answered(WORKER, "", &mut out);
    out.clear();
    lane.demand(SESSION, WORKER, "view-a", true, DEMAND_MS, &mut out);
    (lane, out)
}

fn advertised() -> Option<Vec<String>> {
    Some(vec!["stun:example".to_owned()])
}

/// The lane after a grantless open and an offer read before the mint.
fn holding_an_offer() -> (CarrierLane, u64) {
    let (mut lane, out) = demanded(advertised());
    let attempt_id = opened(&out).expect("the transport opens").attempt_id;
    let mut offered = Vec::new();
    lane.transport_observed(
        WORKER,
        SignallingInput::OfferReady {
            attempt_id,
            peer_id: FIRST_PEER_ID.to_owned(),
            offer_sdp: usable_sdp(),
        },
        OFFER_MS,
        &mut offered,
    );
    assert!(
        negotiated(&offered).is_empty(),
        "no offer leaves before the grant; got {offered:?}"
    );
    (lane, attempt_id)
}

#[test]
fn a_demand_with_advertised_stun_opens_the_transport_before_the_grant() {
    let (lane, out) = demanded(advertised());
    assert!(
        out.iter()
            .any(|effect| matches!(effect, Effect::RequestDirectGrant { .. })),
        "the demand mints; got {out:?}"
    );
    let attempt = opened(&out).expect("the same dispatch opens the transport");
    assert!(attempt.grant_id.is_empty());
    assert_eq!(attempt.stun_urls, vec!["stun:example".to_owned()]);
    assert_eq!(lane.snapshot(WORKER).phase, PeerPhase::Gathering);
}

#[test]
fn an_offer_read_before_the_grant_is_held_and_negotiated_on_the_mint() {
    let (mut lane, attempt_id) = holding_an_offer();
    assert_eq!(lane.snapshot(WORKER).phase, PeerPhase::AwaitingGrant);

    let mut minted = Vec::new();
    lane.grant_minted(grant_for(&[SESSION]), MINT_MS, &mut minted);
    let offers = negotiated(&minted);
    assert_eq!(offers.len(), 1, "exactly one offer leaves; got {minted:?}");
    let attempt = &offers[0];
    assert_eq!(attempt.attempt_id, attempt_id);
    assert_eq!(attempt.grant_id, "grant-a");
    assert_eq!(attempt.worker_epoch, EPOCH);
    assert_eq!(attempt.peer_id, FIRST_PEER_ID);
    assert!(attempt.session_ids.contains(SESSION));
    assert!(!closed(&minted), "the grantless attempt adopts the mint");

    let snapshot = lane.snapshot(WORKER);
    assert_eq!(snapshot.phase, PeerPhase::Negotiating);
    assert_eq!(snapshot.direct_phase_ms.gathering_ms, Some(0));
    assert_eq!(
        snapshot.direct_phase_ms.negotiating_ms,
        Some(MINT_MS - DEMAND_MS),
        "the offer left at the mint's instant"
    );
}

#[test]
fn a_grant_minted_before_the_offer_negotiates_when_the_offer_arrives() {
    let (mut lane, out) = demanded(advertised());
    let attempt_id = opened(&out).expect("the transport opens").attempt_id;
    let mut minted = Vec::new();
    lane.grant_minted(grant_for(&[SESSION]), OFFER_MS, &mut minted);
    assert!(negotiated(&minted).is_empty(), "nothing to send yet");
    assert!(!closed(&minted));

    let mut offered = Vec::new();
    lane.transport_observed(
        WORKER,
        SignallingInput::OfferReady {
            attempt_id,
            peer_id: FIRST_PEER_ID.to_owned(),
            offer_sdp: usable_sdp(),
        },
        MINT_MS,
        &mut offered,
    );
    let offers = negotiated(&offered);
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].grant_id, "grant-a");
}

#[test]
fn a_refused_mint_closes_the_grantless_transport() {
    let (mut lane, out) = demanded(advertised());
    assert!(opened(&out).is_some());
    let mut refused = Vec::new();
    lane.grant_refused(WORKER, MINT_MS, "worker did not acknowledge", &mut refused);
    let closes = carrier_effects(&refused)
        .into_iter()
        .filter(|effect| matches!(effect, CarrierEffect::CloseAttempt { .. }))
        .count();
    assert_eq!(closes, 1, "one close per refused mint; got {refused:?}");
    assert!(
        opened(&refused).is_none(),
        "a retry does not reopen at once"
    );
    let snapshot = lane.snapshot(WORKER);
    assert_eq!(snapshot.phase, PeerPhase::AwaitingGrant);
    assert!(!snapshot.has_carrier);
}

#[test]
fn no_advertised_stun_keeps_the_transport_behind_the_grant() {
    let (lane, out) = demanded(None);
    assert!(
        out.iter()
            .any(|effect| matches!(effect, Effect::RequestDirectGrant { .. }))
    );
    assert!(opened(&out).is_none(), "nothing opens before the grant");
    assert_eq!(lane.snapshot(WORKER).phase, PeerPhase::AwaitingGrant);
}

#[test]
fn a_mint_that_does_not_admit_a_peer_faults_the_grantless_attempt() {
    let (mut lane, _) = holding_an_offer();
    let mut grant = grant_for(&[SESSION]);
    grant.peer_supported = false;
    let mut minted = Vec::new();
    lane.grant_minted(grant, MINT_MS, &mut minted);
    assert!(closed(&minted), "the attempt closes; got {minted:?}");
    assert!(negotiated(&minted).is_empty());
    assert_eq!(lane.snapshot(WORKER).phase, PeerPhase::Disabled);
}
