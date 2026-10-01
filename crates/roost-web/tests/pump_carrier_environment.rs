//! The host's OWN declaration of what its document can do, on the way to a
//! peer attempt.
//!
//! `roost_client_core`'s `direct_carrier_lane.rs` and `smoke/stream_route_lane.rs`
//! both call `set_carrier_environment(true, _)` BY HAND before they assert a
//! phase, so neither can see the defect this file is about: in the running
//! browser nobody called it at all. `Signalling::start` reads
//! `env.peer_transport_available` on its fourth gate, so a lane whose host never
//! declared the document parks as `Disabled` and the session lives on Sync for
//! the life of the document with nothing anywhere to say why.
//!
//! Every test here builds a real `Pump` over a real `ClientCore` — the same
//! pair `App` hands `start_pump` — and drives it with the same three events a
//! peer's happy path is made of. Nothing calls the setter by hand, so these
//! fail on the exact omission that kept the recorded run's route on Sync.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dioxus::core::VNode;
use dioxus::prelude::{Element, VirtualDom};
use dioxus::signals::Signal;

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use roost_client_core::client::carriers::DirectGrant;
use roost_client_core::client::carriers::PeerPhase;
use roost_client_core::{ClientCore, ClientEvent};
use roost_web::platform::connect::CoordRpc;
use roost_web::platform::peer::force_availability_for_test;
use roost_web::pump::Pump;

const WORKER: &str = "worker-a";
const SESSION: &str = "session-a";
const VIEW: &str = "view-1";

thread_local! {
    /// The pump `pump_root` built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

/// The root `VirtualDom::new` wants: it runs `Pump::new` in a REAL scope, which
/// is what `Signal::new` reads and what a test binary has none of.
fn pump_root() -> Element {
    BUILT.with(|built| {
        *built.borrow_mut() = Some(Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-a"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        ));
    });
    Ok(VNode::default())
}

/// A pump and the scope that owns its revision signal.
///
/// Both are returned together because both are needed: the pump dispatches
/// through a `Signal` whose owner is the scope `rebuild` created, so a fixture
/// that dropped the dom would take the signal out from under a live pump.
struct Fixture {
    pump: Pump,
    /// Dropped after `pump`, which is why it is not read again.
    _dom: VirtualDom,
}

impl std::ops::Deref for Fixture {
    type Target = Pump;

    fn deref(&self) -> &Pump {
        &self.pump
    }
}

/// A pump over a document that reports `can_peer` to the carrier lane.
///
/// The capability is PINNED before the pump is built, because the pump declares
/// it during construction: that ordering is the defect under test, and a fixture
/// that set it afterwards would be asserting the opposite of what it claims.
fn pump_that_can_peer(can_peer: bool) -> Fixture {
    force_availability_for_test(can_peer);
    let mut dom = VirtualDom::new(pump_root);
    dom.rebuild_in_place();
    let pump = BUILT.with(|built| built.borrow_mut().take()).expect(
        "the root component runs during rebuild and always builds a pump; \
         an empty slot means the render pass never happened",
    );
    Fixture { pump, _dom: dom }
}

/// A pump over a document with no peer stack, which is what a test binary has.
fn pump() -> Fixture {
    pump_that_can_peer(false)
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

fn grant() -> DirectGrant {
    DirectGrant {
        grant_id: "grant-a".to_owned(),
        secret: "secret-a".to_owned(),
        worker_fp: WORKER.to_owned(),
        worker_epoch: "epoch-a".to_owned(),
        tab_id: "tab-a".to_owned(),
        device_fingerprint: "device-a".to_owned(),
        session_ids: BTreeSet::from([SESSION.to_owned()]),
        peer_supported: true,
        input_route_supported: true,
        stun_urls: Vec::new(),
        expires_at_ms: u64::MAX,
    }
}

/// THE REGRESSION. A document that can peer, asking for a session on a worker
/// that serves some OTHER machine, with a live credential, is the one state
/// that reaches `gathering`.
///
/// Before the pump declared its own environment, `is_available()` was read from
/// exactly one place — `peer_lane::close`, which runs only AFTER a close — so
/// `peer_transport_available` stayed `false` for the whole life of the
/// document, `start` returned on its fourth gate, and this phase was
/// unreachable however long the run waited. That is the recorded run's nine
/// `activeKind: "sync"` route readings: the lane was constructed, demanded, and
/// answered at the door, and never once reached a transport.
#[test]
fn a_document_that_can_peer_reaches_gathering_without_a_hand_declared_environment() {
    let pump = pump_that_can_peer(true);
    pump.dispatch(opened());
    pump.dispatch(ClientEvent::LocalDoorAnswered {
        worker_fp: WORKER.to_owned(),
        serving_worker_fp: String::new(),
    });
    pump.dispatch(ClientEvent::DirectGrantMinted { grant: grant() });

    assert_eq!(
        pump.core().borrow().store().direct.phase(WORKER),
        PeerPhase::Gathering,
        "a peer-capable document with a live credential must open exactly one attempt; \
         a phase short of this means the host never declared what the document can do"
    );
}

/// The declaration is the DOCUMENT's, and it is read from the host rather than
/// assumed — so a host that reports no WebRTC must not be talked into
/// allocating a peer.
///
/// This is the half that would be lost by hard-coding `true` at the boot site
/// instead of asking the platform: the gate has to stay answerable, and the
/// only way to know that is to watch a document that answers `false` stay put.
#[test]
fn a_document_that_cannot_peer_stays_off_the_peer_lane() {
    let pump = pump();

    pump.dispatch(opened());
    pump.dispatch(ClientEvent::LocalDoorAnswered {
        worker_fp: WORKER.to_owned(),
        serving_worker_fp: String::new(),
    });
    pump.dispatch(ClientEvent::DirectGrantMinted { grant: grant() });

    assert_eq!(
        pump.core().borrow().store().direct.phase(WORKER),
        PeerPhase::Disabled,
        "a document with no WebRTC must report the platform's answer rather than open a transport"
    );
}

/// A worker serving this page's OWN machine keeps the loopback slice, and the
/// peer cap is not what stops it. Declaring the environment must not turn
/// same-host into a WebRTC negotiation.
#[test]
fn a_page_the_worker_itself_serves_still_never_allocates_a_peer() {
    let pump = pump();
    pump.dispatch(opened());
    pump.dispatch(ClientEvent::LocalDoorAnswered {
        worker_fp: WORKER.to_owned(),
        serving_worker_fp: WORKER.to_owned(),
    });
    pump.dispatch(ClientEvent::DirectGrantMinted { grant: grant() });

    assert_eq!(
        pump.core().borrow().store().direct.phase(WORKER),
        PeerPhase::Idle,
        "loopback holds the carrier on the worker's own machine; the peer cap is not what stopped it"
    );
}
