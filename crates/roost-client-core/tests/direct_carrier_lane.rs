//! The lane that makes the election run at all: a view, a door answer, and a
//! grant, driven through `ClientCore` rather than through a `Signalling` the
//! test built itself.
//!
//! `terminal_peer_election.rs` drives the machine. This file drives the CLIENT,
//! and it is the layer the machine was missing: a state machine that is never
//! constructed cannot mint a credential, cannot learn which worker shares the
//! page's machine, and cannot open a carrier — so every symptom downstream is a
//! session quietly living on Sync with nothing anywhere to say why.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::carriers::{CarrierEffect, PeerPhase};
use roost_client_core::{ClientCore, ClientEvent, Effect};

const WORKER: &str = "worker-a";
const SESSION: &str = "session-a";
const VIEW: &str = "view-1";

/// A core whose document is a plain one: no browser WebRTC is asserted here,
/// only that the election runs at all.
fn core() -> ClientCore {
    ClientCore::in_memory("tab-a")
}

fn opened() -> ClientEvent {
    ClientEvent::ViewOpened {
        session_id: SESSION.to_owned(),
        worker_fp: WORKER.to_owned(),
        view_id: VIEW.to_owned(),
        cols: 80,
        rows: 24,
    }
}

fn opened_a_peer(effects: &[Effect]) -> bool {
    effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::Carrier(inner)
                if matches!(**inner, CarrierEffect::OpenTransport { .. })
        )
    })
}

/// The root question: does a view wanting a session on a worker produce the
/// credential request the whole carrier path is built on?
///
/// Before the lane existed this was an empty vector, and the empty vector is
/// why the loopback and WebRTC specs both time out on `waitForDirectRoute`:
/// nothing ever asks, so nothing ever dials.
#[test]
fn a_view_on_a_worker_asks_for_a_direct_grant() {
    let mut core = core();
    let effects = core.handle(opened());
    assert!(
        effects.contains(&Effect::RequestDirectGrant {
            session_id: SESSION.to_owned(),
            worker_fp: WORKER.to_owned(),
        }),
        "a demanded session must reach the coordinator; got {effects:?}"
    );
    let snapshot = core.store().direct.snapshot(WORKER);
    assert_eq!(snapshot.active_views, 1, "the machine counts the view");
    assert!(
        snapshot.demanded_sessions.contains(SESSION),
        "and knows which session it is for"
    );
}

/// A page the worker itself serves never allocates a peer, however long it is
/// asked to wait. `terminal-peer.spec.ts:62` is named for exactly this, and the
/// assertion is a NEGATIVE one: nothing here may open a transport.
#[test]
fn a_page_served_by_the_worker_never_allocates_a_peer() {
    let mut core = core();
    let mut effects = core.handle(opened());
    effects.extend(core.handle(ClientEvent::LocalDoorAnswered {
        worker_fp: WORKER.to_owned(),
        serving_worker_fp: WORKER.to_owned(),
    }));
    effects.extend(core.handle(ClientEvent::Sweep { now_ms: 60_000 }));
    assert!(
        !opened_a_peer(&effects),
        "loopback holds the carrier on the worker's own machine; got {effects:?}"
    );
    assert_eq!(core.store().direct.phase(WORKER), PeerPhase::Idle);
}

/// A page on ANOTHER machine is exactly the case a peer exists for, and it is
/// the half of the lane that a machine nobody instantiates could never reach.
#[test]
fn a_page_elsewhere_releases_one_peer_for_the_worker() {
    let mut core = core();
    // The host owns both answers; without them the machine parks as
    // `Unsupported`, which is correct for a document with no WebRTC and is not
    // what this test is about.
    core.set_carrier_environment(true, 0);
    let mut effects = core.handle(opened());
    effects.extend(core.handle(ClientEvent::LocalDoorAnswered {
        worker_fp: WORKER.to_owned(),
        serving_worker_fp: String::new(),
    }));
    // No credential yet, so the machine waits rather than opening: a carrier
    // that authenticates on nothing is the failure the grant exists to stop.
    assert!(!opened_a_peer(&effects), "no grant, no carrier");

    effects.extend(core.handle(ClientEvent::DirectGrantMinted { grant: grant() }));
    assert!(
        opened_a_peer(&effects),
        "a live credential for a peer-capable worker opens exactly one attempt; got {effects:?}"
    );
    assert_eq!(core.store().direct.phase(WORKER), PeerPhase::Gathering);
}

/// Closing the last view is what stops the asking. A machine that keeps
/// requesting after the pane is gone is a request the operator cannot stop.
#[test]
fn closing_the_view_stops_the_asking_and_the_machine_is_forgotten() {
    let mut core = core();
    let _ = core.handle(opened());
    let _ = core.handle(ClientEvent::ViewClosed {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
    });
    let effects = core.handle(ClientEvent::Sweep { now_ms: 10_000 });
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::RequestDirectGrant { .. })),
        "a session no view wants is not re-requested; got {effects:?}"
    );
    assert_eq!(
        core.store().direct.workers().count(),
        0,
        "a machine nothing wants is forgotten on the sweep"
    );
}

/// A removed worker's machine must not outlive it: the grant lifecycle treats
/// retirement as terminal, and a surviving machine would keep asking.
#[test]
fn a_retired_worker_makes_its_machine_ask_nothing_further() {
    let mut core = core();
    let _ = core.handle(opened());
    let effects = core.handle(ClientEvent::WorkerRetired {
        worker_fp: WORKER.to_owned(),
    });
    assert_eq!(core.store().direct.workers().count(), 0);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::RequestDirectGrant { .. })),
        "a deleted worker is not asked about again; got {effects:?}"
    );
}

/// A credential the coordinator minted and the worker did not acknowledge is a
/// REFUSAL, and a refusal has to be visible or the retry is never armed.
#[test]
fn a_refused_mint_arms_the_retry_the_election_is_waiting_on() {
    let mut core = core();
    let _ = core.handle(opened());
    let effects = core.handle(ClientEvent::DirectGrantRefused {
        worker_fp: WORKER.to_owned(),
        reason: "the worker acknowledged nothing".to_owned(),
    });
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::Carrier(inner) if matches!(**inner, CarrierEffect::RetryAt { .. })
        )),
        "a refused mint owes a retry; got {effects:?}"
    );
    assert_eq!(
        core.store()
            .direct
            .snapshot(WORKER)
            .last_failure_detail
            .as_deref(),
        Some("the worker acknowledged nothing"),
        "and the reason is readable rather than inferred from silence"
    );
}

fn grant() -> roost_client_core::client::carriers::DirectGrant {
    roost_client_core::client::carriers::DirectGrant {
        grant_id: "grant-a".to_owned(),
        secret: "secret-a".to_owned(),
        worker_fp: WORKER.to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        tab_id: "tab-a".to_owned(),
        device_fingerprint: "device-a".to_owned(),
        session_ids: std::collections::BTreeSet::from([SESSION.to_owned()]),
        peer_supported: true,
        input_route_supported: true,
        stun_urls: Vec::new(),
        expires_at_ms: u64::MAX,
    }
}
