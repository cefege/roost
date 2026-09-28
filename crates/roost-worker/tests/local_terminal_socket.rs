//! What a browser on this machine may do with the worker's loopback terminal
//! socket: only a Hello that matches a coordinator-installed grant is
//! admitted, membership is confined to that grant's sessions, a device
//! revocation ends the socket, a socket that stops draining is dropped alone,
//! and a socket that never says Hello is expired. The real grant store, view
//! owner, session manager, cell sinks and cadence run; only the carrier is a
//! stub. Ports `apps/worker/tests/local-door/local-terminal-socket.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod local_terminal_support;
mod terminal_stream_support;

use std::time::Duration;

use local_terminal_support::{
    DEVICE, Fixture, GRANT_ID, GRANTED_DEAD_SESSION, SECRET, TAB, UNGRANTED_SESSION, WORKER_EPOCH,
    WORKER_FP, closed_reason, grant, hello, settle, view,
};
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::TerminalViewStatus;
use roost_worker::link_ports::LocalTerminalGrantPort;
use roost_worker::local_terminal::PacketSendResult;
use terminal_stream_support::{SESSION, held};

#[tokio::test]
async fn a_hello_whose_secret_does_not_match_is_closed_and_registers_nothing() {
    let fixture = Fixture::new();
    let stub = fixture.open();

    fixture.send(&stub, hello(GRANT_ID, "not-the-secret", TAB));

    assert_eq!(stub.cases(), ["closed"]);
    assert_eq!(
        closed_reason(&stub.frames()[0]),
        "local terminal grant secret mismatch"
    );
    assert!(!roost_worker::local_terminal::TerminalPacketPort::is_open(
        stub.as_ref()
    ));
    assert!(!fixture.has_sink(&stub));
}

#[tokio::test]
async fn a_socket_without_a_grant_is_refused() {
    let fixture = Fixture::new();
    let stub = fixture.open();

    fixture.send(
        &stub,
        hello("7a7a7a7a-7a7a-4a7a-8a7a-7a7a7a7a7a7a", SECRET, TAB),
    );

    assert_eq!(stub.cases(), ["closed"]);
    assert_eq!(
        closed_reason(&stub.frames()[0]),
        "unknown local terminal grant"
    );
    assert_eq!(stub.close_reasons(), ["unknown local terminal grant"]);
    assert!(!fixture.has_sink(&stub));
}

#[tokio::test]
async fn any_frame_before_the_hello_closes_the_socket() {
    let fixture = Fixture::new();
    let stub = fixture.open();

    fixture.send(&stub, view(SESSION, 1));

    assert_eq!(stub.cases(), ["closed"]);
    assert_eq!(closed_reason(&stub.frames()[0]), "hello required");
}

#[tokio::test]
async fn a_valid_hello_is_ready_with_the_granted_sessions_and_its_own_generation() {
    let fixture = Fixture::new();
    let first = fixture.open();
    let second = fixture.open();

    fixture.send(&first, hello(GRANT_ID, SECRET, TAB));
    fixture.send(&second, hello(GRANT_ID, SECRET, TAB));

    let ready = |frames: &[ServerFrame]| match &frames[0] {
        ServerFrame::Ready(ready) => (**ready).clone(),
        other => panic!(
            "expected ready, got {}",
            local_terminal_support::case(other)
        ),
    };
    let first_ready = ready(&first.frames());
    assert_eq!(first_ready.worker_fingerprint, WORKER_FP);
    assert_eq!(first_ready.session_ids, [SESSION, GRANTED_DEAD_SESSION]);
    assert_eq!(
        (
            first_ready.worker_epoch.as_str(),
            first_ready.peer_id.as_str()
        ),
        (WORKER_EPOCH, "")
    );
    assert_eq!(
        first_ready.socket_id,
        roost_worker::local_terminal::TerminalPacketPort::socket_id(first.as_ref())
    );
    assert!(first_ready.socket_generation > 0);
    assert_ne!(
        ready(&second.frames()).socket_generation,
        first_ready.socket_generation
    );
    assert_eq!(
        first.close_reasons(),
        ["local terminal grant connection replaced"]
    );
    assert!(
        !fixture.has_sink(&first),
        "the replaced socket stops painting"
    );
    assert!(fixture.has_sink(&second));

    // A second hello on a live socket would silently re-point membership.
    fixture.send(&second, hello(GRANT_ID, SECRET, TAB));
    assert_eq!(second.cases(), ["ready", "closed"]);
    assert_eq!(closed_reason(&second.frames()[1]), "duplicate hello");
}

#[tokio::test]
async fn a_view_for_a_session_outside_the_grant_is_refused() {
    let fixture = Fixture::new();
    let stub = fixture.open();
    fixture.send(&stub, hello(GRANT_ID, SECRET, TAB));

    fixture.send(&stub, view(UNGRANTED_SESSION, 1));

    assert_eq!(stub.cases(), ["ready", "terminalViewState"]);
    let ServerFrame::TerminalViewState(state) = &stub.frames()[1] else {
        panic!("a view state")
    };
    assert_eq!(state.session_id, UNGRANTED_SESSION);
    assert_eq!(state.status.as_known(), Some(TerminalViewStatus::Rejected));
    assert_eq!(state.reason, "terminal session is unavailable");
    settle().await;
    assert!(
        fixture
            .harness
            .manager
            .current_terminal_stream_id(&terminal_stream_support::session_id())
            .is_none(),
        "no stream was minted"
    );
    assert!(roost_worker::local_terminal::TerminalPacketPort::is_open(
        stub.as_ref()
    ));
}

#[tokio::test]
async fn a_granted_socket_receives_cells_for_its_view() {
    let fixture = Fixture::new();
    let stub = fixture.open();
    fixture.send(&stub, hello(GRANT_ID, SECRET, TAB));

    fixture.send(&stub, view(SESSION, 1));
    settle().await;
    fixture.harness.deliver(b"local-marker");
    settle().await;

    let cases = stub.cases();
    let accepted = stub.frames().iter().any(|frame| {
        matches!(frame, ServerFrame::TerminalViewState(state) if state.status.as_known() == Some(TerminalViewStatus::Accepted))
    });
    assert!(accepted, "the view was accepted: {cases:?}");
    let cells: Vec<_> = stub
        .frames()
        .into_iter()
        .filter_map(|frame| match frame {
            ServerFrame::CellGrid(cells) => Some(*cells),
            _ => None,
        })
        .collect();
    assert!(!cells.is_empty(), "cells reached the socket: {cases:?}");
    assert!(
        cells.iter().all(|frame| frame.session_id == SESSION),
        "every direct frame names its session"
    );
}

#[tokio::test]
async fn revoking_the_device_closes_its_socket() {
    let fixture = Fixture::new();
    let stub = fixture.open();
    fixture.send(&stub, hello(GRANT_ID, SECRET, TAB));

    fixture.door.revoke_device(DEVICE);
    settle().await;

    assert_eq!(stub.cases(), ["ready", "closed"]);
    assert_eq!(
        closed_reason(&stub.frames()[1]),
        "local terminal grant revoked"
    );
    assert!(!roost_worker::local_terminal::TerminalPacketPort::is_open(
        stub.as_ref()
    ));
    assert!(fixture.grants.current(GRANT_ID).is_none());
    assert!(!fixture.has_sink(&stub));
}

#[tokio::test]
async fn a_socket_that_stops_draining_is_dropped_alone() {
    let fixture = Fixture::new();
    let stalled = fixture.open();
    let healthy_secret = "4ea1744ea1744ea1744ea1744ea1744ea1744ea1744ea1744ea1744ea1744ea1";
    let healthy_grant = "1e1e1e1e-1e1e-4e1e-8e1e-1e1e1e1e1e1e";
    let healthy_tab = "tab-local-healthy";
    fixture
        .grants
        .install(&grant(
            healthy_grant,
            healthy_secret,
            &[SESSION],
            healthy_tab,
            60_000,
        ))
        .unwrap();
    let healthy = fixture.open();
    fixture.send(&stalled, hello(GRANT_ID, SECRET, TAB));
    fixture.send(&healthy, hello(healthy_grant, healthy_secret, healthy_tab));
    fixture.send(&stalled, view(SESSION, 1));
    fixture.send(&healthy, view(SESSION, 1));
    settle().await;
    assert!(fixture.has_sink(&stalled));

    // A carrier that cannot accept a whole cell frame is retired alone.
    *held(&stalled.send_result) = Some(PacketSendResult::Refused);
    for attempt in 0..8u8 {
        if !roost_worker::local_terminal::TerminalPacketPort::is_open(stalled.as_ref()) {
            break;
        }
        fixture
            .harness
            .deliver(format!("burst-{attempt}\r\n").as_bytes());
        settle().await;
    }

    assert_eq!(stalled.close_reasons(), ["local delivery overflow"]);
    assert!(!fixture.has_sink(&stalled));
    assert!(roost_worker::local_terminal::TerminalPacketPort::is_open(
        healthy.as_ref()
    ));
    assert!(fixture.has_sink(&healthy));
    assert!(healthy.cases().contains(&"cellGrid"));
}

/// v2 `local-terminal-prehello.ts` through the socket owner: a loopback
/// socket that never says Hello is closed at the deadline.
#[tokio::test(start_paused = true)]
async fn a_socket_that_never_says_hello_is_closed_at_the_deadline() {
    let fixture = Fixture::new();
    let stub = fixture.open();

    tokio::time::sleep(Duration::from_millis(2_999)).await;
    assert!(stub.cases().is_empty(), "not before the deadline");
    tokio::time::sleep(Duration::from_millis(2)).await;
    settle().await;

    assert_eq!(stub.cases(), ["closed"]);
    assert_eq!(
        closed_reason(&stub.frames()[0]),
        "local terminal hello timed out"
    );
    assert!(!roost_worker::local_terminal::TerminalPacketPort::is_open(
        stub.as_ref()
    ));
}
