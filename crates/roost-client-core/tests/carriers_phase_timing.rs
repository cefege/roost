//! The climb to a direct route, decomposed: how far into the attempt each phase
//! was first entered, on the host clock each input carries.
//!
//! Depends on `tests/terminal_peer_support` for the described machine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_peer_support;

use roost_client_core::client::carriers::{
    CarrierEffect, DirectPhaseTimings, PeerAnswer, PeerPhase, PeerSignalling, Signalling,
    SignallingInput,
};
use roost_client_core::terminal::token::{TerminalToken, TerminalTransport};
use terminal_peer_support::{
    EPOCH, FIRST_PEER_ID, NOW, SESSION, WORKER, demand, elsewhere, grant_minted, machine, ready,
    usable_sdp,
};

fn advance(peer: &mut Signalling, now_ms: u64) {
    peer.step(SignallingInput::Sweep { now_ms });
}

fn phases(peer: &Signalling) -> DirectPhaseTimings {
    peer.snapshot().direct_phase_ms
}

#[test]
fn each_phase_is_stamped_on_entry_and_active_matches_time_to_direct() {
    let mut peer = machine(0);
    peer.step(demand(SESSION));
    peer.step(elsewhere());
    let opened = peer.step(grant_minted(&[SESSION]));
    let attempt_id = opened
        .iter()
        .find_map(|effect| match effect {
            CarrierEffect::OpenTransport { attempt } => Some(attempt.attempt_id),
            _ => None,
        })
        .expect("a grant on another machine opens a transport");
    assert_eq!(peer.phase(), PeerPhase::Gathering);

    advance(&mut peer, NOW + 30);
    peer.step(SignallingInput::OfferReady {
        attempt_id,
        peer_id: FIRST_PEER_ID.to_string(),
        offer_sdp: usable_sdp(),
    });
    advance(&mut peer, NOW + 110);
    peer.step(SignallingInput::AnswerReceived {
        attempt_id,
        answer: PeerAnswer {
            peer_id: FIRST_PEER_ID.to_string(),
            worker_epoch: EPOCH.to_string(),
            answer_sdp: usable_sdp(),
        },
    });
    advance(&mut peer, NOW + 260);
    peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready(EPOCH, &[SESSION]),
    });
    assert_eq!(phases(&peer).active_ms, None, "nothing is elected yet");

    peer.step(SignallingInput::PromotionCommitted {
        session_id: SESSION.to_string(),
        token: TerminalToken::direct(7, TerminalTransport::Peer, WORKER, EPOCH, 7),
        now_ms: NOW + 400,
    });
    assert_eq!(peer.phase(), PeerPhase::Active);

    let timings = phases(&peer);
    assert_eq!(
        timings,
        DirectPhaseTimings {
            gathering_ms: Some(0),
            negotiating_ms: Some(30),
            authenticating_ms: Some(110),
            candidate_ms: Some(260),
            active_ms: Some(400),
        }
    );
    let ordered = [
        timings.gathering_ms,
        timings.negotiating_ms,
        timings.authenticating_ms,
        timings.candidate_ms,
        timings.active_ms,
    ];
    assert!(ordered.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(
        timings.active_ms,
        peer.snapshot().time_to_direct_ms,
        "the view and the attempt began at one instant, on one clock"
    );
}

#[test]
fn a_closed_attempt_clears_its_phase_stamps() {
    let (mut peer, attempt_id) = terminal_peer_support::authenticating();
    assert!(phases(&peer).authenticating_ms.is_some());
    peer.step(SignallingInput::IceFailed { attempt_id });
    let timings = phases(&peer);
    assert_eq!(timings.gathering_ms, None);
    assert_eq!(timings.authenticating_ms, None);
}
