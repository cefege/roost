//! A retired worker's direct carriers go, and the host is told to close them.
//!
//! The core can retire a route because a route is a value in the store. A socket
//! is not — it belongs to the host — so a retirement that only reached the store
//! leaves a live carrier to a machine an operator deleted, and PTY input keeps
//! being accepted until the grant's own TTL expires. `docs/FAILURE-INDEX.md` "A
//! deleted worker's direct terminal still accepts input" is that defect; this is
//! the Rust half of its guard.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_client_core::ClientEvent;
use roost_client_core::terminal::token::{TerminalToken, TerminalTransport};
use roost_client_core::{ClientCore, DirectCarrier, Effect};

const CONNECTION: &str = "loopback-worker-a-7-1";

fn carrier(worker: &str, connection_id: &str, session_id: &str) -> DirectCarrier {
    DirectCarrier {
        connection_id: connection_id.to_owned(),
        worker_fp: worker.to_owned(),
        transport: TerminalTransport::Loopback,
        token: TerminalToken::direct(7, TerminalTransport::Loopback, worker, "epoch-a", 7),
        socket_id: format!("{connection_id}-socket"),
        granted_sessions: BTreeSet::from([session_id.to_owned()]),
    }
}

#[test]
fn retiring_a_worker_asks_the_host_to_close_its_carriers() {
    let mut core = ClientCore::in_memory("tab-a");
    core.handle(ClientEvent::CarrierReady(carrier(
        "worker-a",
        CONNECTION,
        "session-a",
    )));
    let effects = core.handle(ClientEvent::WorkerRetired {
        worker_fp: "worker-a".to_owned(),
    });
    assert!(
        effects.contains(&Effect::CloseDirectCarriers {
            worker_fp: "worker-a".to_owned()
        }),
        "the route is a value the core can retire; the socket is not, so the host \
         must be told or the carrier outlives the machine: {effects:?}"
    );
}

#[test]
fn a_retirement_leaves_another_workers_carrier_alone() {
    let mut core = ClientCore::in_memory("tab-a");
    core.handle(ClientEvent::CarrierReady(carrier(
        "worker-a",
        CONNECTION,
        "session-a",
    )));
    core.handle(ClientEvent::CarrierReady(carrier(
        "worker-b",
        "loopback-worker-b-7-2",
        "session-b",
    )));
    let effects = core.handle(ClientEvent::WorkerRetired {
        worker_fp: "worker-a".to_owned(),
    });
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::CloseDirectCarriers { .. }))
            .count(),
        1,
        "a retirement names one worker, and a host that closed the whole \
         document's fleet would drop live terminals on machines still present: \
         {effects:?}"
    );
    assert!(
        !effects.contains(&Effect::CloseDirectCarriers {
            worker_fp: "worker-b".to_owned()
        }),
        "{effects:?}"
    );
}
