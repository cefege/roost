//! The offer/answer/Ready exchange, and what the tuple must match.
//!
//! A peer's Ready tuple is the peer's own claim about who it is talking to and
//! what it may carry: the worker fingerprint, the worker epoch, and the session
//! scope. Every field is checked against the grant, because a tuple that is
//! right about the worker but wrong about the scope is how a peer ends up
//! carrying a session it was never issued.
//!
//! A matching tuple stages a candidate; staging is not election. The transport
//! the page will actually use is still chosen afterwards.

mod terminal_peer_support;
use roost_client_core::TerminalTransport;
use roost_client_core::client::carriers::{
    CarrierEffect, CarrierFault, PeerPhase, PeerSignalling, ReadyTuple, SignallingInput,
};
use terminal_peer_support::{
    EPOCH, FIRST_PEER_ID, SESSION, WORKER, authenticating, demand, elsewhere, faults, grant_minted,
    machine, opened_a_transport, ready, usable_sdp,
};

#[test]
fn a_matching_answer_and_ready_stage_a_carrier_that_is_not_yet_elected() {
    let (mut peer, attempt_id) = authenticating();
    let staged = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready(EPOCH, &[SESSION]),
    });
    assert!(
        faults(&staged).is_empty(),
        "a matching tuple is not a fault"
    );
    assert!(
        staged
            .iter()
            .any(|effect| matches!(effect, CarrierEffect::StageCarrier { .. })),
        "a matching Ready is what stages a candidate; got {staged:?}"
    );
    let snapshot = peer.snapshot();
    assert_eq!(snapshot.phase, PeerPhase::Candidate);
    assert!(snapshot.has_carrier, "held, not yet elected");
    assert_eq!(snapshot.transport_held, Some(TerminalTransport::Peer));
}

#[test]
fn rejects_a_ready_tuple_with_a_different_worker_epoch() {
    let (mut peer, attempt_id) = authenticating();
    let effects = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id,
        ready: ready("epoch-b", &[SESSION]),
    });
    assert_eq!(faults(&effects), vec![CarrierFault::IdentityMismatch]);
    let staged = |effect: &CarrierEffect| matches!(effect, CarrierEffect::StageCarrier { .. });
    assert!(
        !effects.iter().any(staged),
        "a refused tuple stages nothing"
    );
    assert!(
        !peer.snapshot().has_carrier,
        "nothing is held after a refusal"
    );
}

#[test]
fn accepts_a_bounded_ready_for_the_full_256_session_grant() {
    let sessions: Vec<String> = (0..256)
        .map(|index| format!("00000000-0000-4000-8000-{index:012}"))
        .collect();
    let borrowed: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let mut peer = machine(0);
    for session in &sessions {
        peer.step(demand(session));
    }
    peer.step(grant_minted(&borrowed));
    peer.step(elsewhere());
    let offer = peer.step(SignallingInput::OfferReady {
        attempt_id: 1,
        offer_sdp: usable_sdp(),
    });
    assert!(
        opened_a_transport(&offer),
        "a 256-session grant is exactly in bounds, not over it"
    );
    let widest = ReadyTuple {
        worker_fp: WORKER.to_string(),
        worker_epoch: EPOCH.to_string(),
        peer_id: FIRST_PEER_ID.to_string(),
        socket_generation: 8,
        socket_id: "socket-max-scope".to_string(),
        session_ids: sessions.iter().cloned().collect(),
    };
    let staged = peer.step(SignallingInput::PeerAuthenticated {
        attempt_id: 1,
        ready: widest,
    });
    let admitted = staged
        .iter()
        .any(|effect| matches!(effect, CarrierEffect::StageCarrier { .. }));
    assert!(admitted, "a Ready inside the grant's scope is admitted");
}
