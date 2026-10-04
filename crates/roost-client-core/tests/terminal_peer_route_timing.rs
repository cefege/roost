//! The machine's one route measurement: how long a worker's views waited before
//! the peer serving them was elected. Time is the host clock each input carries,
//! so every expected number below is exact rather than a bound.
//!
//! Depends on `tests/terminal_peer_support` for the described machine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_peer_support;

use roost_client_core::client::carriers::{
    CarrierEffect, PeerAnswer, PeerPhase, PeerSignalling, Signalling, SignallingInput,
};
use roost_client_core::terminal::token::{TerminalToken, TerminalTransport};
use terminal_peer_support::{
    EPOCH, FIRST_PEER_ID, NOW, SESSION, WORKER, authenticating, ready, usable_sdp,
};

const OTHER_SESSION: &str = "session-b";

/// A machine whose peer authenticated for the view opened at `NOW`, and the
/// attempt that peer is.
fn candidate() -> (Signalling, u64) {
    let (mut peer, attempt_id) = authenticating();
    authenticate(&mut peer, attempt_id);
    assert_eq!(peer.phase(), PeerPhase::Candidate);
    (peer, attempt_id)
}

fn authenticate(peer: &mut Signalling, attempt_id: u64) {
    peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready(EPOCH, &[SESSION]),
    });
}

fn elect(peer: &mut Signalling, session: &str, now_ms: u64) {
    peer.step(SignallingInput::PromotionCommitted {
        session_id: session.to_string(),
        token: TerminalToken::direct(7, TerminalTransport::Peer, WORKER, EPOCH, 7),
        now_ms,
    });
}

fn waited(peer: &Signalling) -> Option<u64> {
    peer.snapshot().time_to_direct_ms
}

/// Offer, answer and authenticate the attempt a retry just opened.
fn renegotiate(peer: &mut Signalling, retried: &[CarrierEffect]) {
    let attempt_id = retried
        .iter()
        .find_map(|effect| match effect {
            CarrierEffect::OpenTransport { attempt } => Some(attempt.attempt_id),
            _ => None,
        })
        .expect("the retry opens a new attempt");
    peer.step(SignallingInput::OfferReady {
        attempt_id,
        peer_id: FIRST_PEER_ID.to_string(),
        offer_sdp: usable_sdp(),
    });
    peer.step(SignallingInput::AnswerReceived {
        attempt_id,
        answer: PeerAnswer {
            peer_id: FIRST_PEER_ID.to_string(),
            worker_epoch: EPOCH.to_string(),
            answer_sdp: usable_sdp(),
        },
    });
    authenticate(peer, attempt_id);
}

/// The wait runs from the first view to the first election, and a session that
/// later joins the elected peer is not a second election.
#[test]
fn the_wait_runs_from_the_first_view_to_the_election() {
    let (mut peer, _) = candidate();
    assert_eq!(waited(&peer), None, "nothing is elected yet");

    elect(&mut peer, SESSION, NOW + 750);
    assert_eq!(peer.phase(), PeerPhase::Active);
    assert_eq!(waited(&peer), Some(750));

    elect(&mut peer, OTHER_SESSION, NOW + 9_000);
    assert_eq!(
        waited(&peer),
        Some(750),
        "a second session joining an elected peer re-measures nothing"
    );
}

/// An elected peer that falls back puts its still-open view back on the wait
/// from the FALLBACK, so the next election measures the time spent off the
/// direct route rather than the whole life of the view.
#[test]
fn a_fallback_restarts_the_wait_from_the_instant_it_left_direct() {
    let (mut peer, attempt_id) = candidate();
    elect(&mut peer, SESSION, NOW + 750);
    let fell_back_at = NOW + 2_000;
    peer.step(SignallingInput::Sweep {
        now_ms: fell_back_at,
    });
    peer.step(SignallingInput::IceFailed { attempt_id });
    assert_eq!(
        waited(&peer),
        None,
        "a route that fell back reports no time to direct"
    );

    // An elected peer's failure holds its worker down for thirty seconds.
    let retried_at = fell_back_at + 30_000;
    let retried = peer.step(SignallingInput::RetryDue { now_ms: retried_at });
    renegotiate(&mut peer, &retried);
    elect(&mut peer, SESSION, retried_at + 400);
    assert_eq!(waited(&peer), Some(30_400));
}

/// A view that leaves before anything is elected ends its wait unmeasured, and
/// the next view starts a fresh one.
#[test]
fn the_last_view_leaving_ends_the_wait_unmeasured() {
    let (mut peer, _) = candidate();
    peer.step(SignallingInput::Demand {
        session_id: SESSION.to_string(),
        view_id: format!("view-{SESSION}"),
        active: false,
        now_ms: NOW + 100,
    });
    peer.step(SignallingInput::Demand {
        session_id: SESSION.to_string(),
        view_id: "view-again".to_string(),
        active: true,
        now_ms: NOW + 5_000,
    });
    elect(&mut peer, SESSION, NOW + 5_300);
    assert_eq!(waited(&peer), Some(300));
}
