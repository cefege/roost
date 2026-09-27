//! What must hold before an attempt may open or continue: a live grant, the
//! grant's bounded retry, and the per-document peer cap.
//!
//! Admission is the part of the peer's life a user waits on, so each bound is a
//! rule about time or count rather than about a decision. A carrier with no
//! grant waits rather than opening; a grant request that returned is a refusal,
//! not a pending state, and re-asks once at a fixed deadline rather than once
//! per demand; and a browser document gets a fixed number of peers, because one
//! tab that opens peers without bound is a fleet member.

mod terminal_peer_support;
use roost_client_core::Effect;
use roost_client_core::client::carriers::{
    CarrierEffect, CarrierFault, DirectGrant, GRANT_RETRY_MS, GrantInput, GrantPhase, PeerPhase,
    PeerSignalling, SignallingInput,
};
use terminal_peer_support::{
    NOW, SESSION, WORKER, demand, elsewhere, faults, grant_for, machine, opened_a_transport,
    usable_sdp,
};

#[test]
fn a_carrier_with_no_live_grant_never_authenticates() {
    let mut peer = machine(0);
    peer.step(demand(SESSION));
    peer.step(elsewhere());
    let effects = peer.step(SignallingInput::OfferReady {
        attempt_id: 1,
        offer_sdp: usable_sdp(),
    });
    assert!(
        !opened_a_transport(&effects),
        "no grant, no transport; {effects:?}"
    );
    let snapshot = peer.snapshot();
    assert_eq!(snapshot.grant_phase, GrantPhase::Requested);
    assert_eq!(snapshot.phase, PeerPhase::AwaitingGrant);
}

#[test]
fn retries_a_transient_initial_grant_failure_at_the_bounded_retry_deadline() {
    let mut peer = machine(0);
    let exact = CarrierEffect::Core(Effect::RequestDirectGrant {
        session_id: SESSION.to_string(),
        worker_fp: WORKER.to_string(),
    });
    let requested = peer.step(demand(SESSION));
    assert!(
        requested.contains(&exact),
        "new demand asks for a grant; {requested:?}"
    );

    // A request that RETURNS is a refusal, not a pending state: only a worker's
    // acknowledgement reveals the secret.
    let refused = peer.step(SignallingInput::Grant(GrantInput::Refused {
        now_ms: NOW,
        reason: "worker did not acknowledge".to_string(),
    }));
    assert_eq!(
        peer.snapshot().grant_phase,
        GrantPhase::Unavailable,
        "a returned request is a refusal, not a pending state"
    );
    let armed = refused.iter().find_map(|effect| match effect {
        CarrierEffect::RetryAt { at_ms } => Some(*at_ms),
        _ => None,
    });
    let armed = armed.unwrap_or_else(|| panic!("a refusal arms a retry; {refused:?}"));
    assert_eq!(armed, NOW + GRANT_RETRY_MS);
    let asked = |effect: &CarrierEffect| {
        matches!(
            effect,
            CarrierEffect::Core(Effect::RequestDirectGrant { .. })
        )
    };
    let early = peer.step(SignallingInput::RetryDue { now_ms: armed - 1 });
    assert!(!early.iter().any(asked), "the retry must not fire early");
    let retried = peer.step(SignallingInput::RetryDue { now_ms: armed });
    assert_eq!(
        retried.iter().filter(|effect| asked(effect)).count(),
        1,
        "the retry asks once, not once per demand; got {retried:?}"
    );
}

#[test]
fn refuses_a_ninth_simultaneously_demanded_browser_peer() {
    let mut opened = Vec::new();
    let mut refused = Vec::new();
    for index in 0..9u32 {
        let session = format!("session-{index}");
        let mut peer = machine(index);
        peer.step(demand(&session));
        peer.step(SignallingInput::Grant(GrantInput::Minted(DirectGrant {
            worker_fp: format!("worker-{index}"),
            ..grant_for(&[&session])
        })));
        let mut effects = peer.step(elsewhere());
        effects.extend(peer.step(SignallingInput::OfferReady {
            attempt_id: 1,
            offer_sdp: usable_sdp(),
        }));
        opened.push(opened_a_transport(&effects));
        refused.push(faults(&effects));
    }
    assert!(
        opened[..8].iter().all(|entry| *entry) && !opened[8],
        "eight peers fit the document-wide cap and the ninth does not; got {opened:?}"
    );
    assert_eq!(
        refused[8],
        vec![CarrierFault::DocumentCap],
        "a cap, not a fault"
    );
}
