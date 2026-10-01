//! The loopback carrier's handshake and traffic, exercised.
//!
//! A fixture module rather than an inline `mod tests`: `CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture", and the file this
//! moved out of is at the 400-line cap. The allowance is declared here for
//! that reason and not out of habit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_client_core::client::local::door::LoopbackReady;
use roost_client_core::client::local::{GrantMintAnswer, LocalTerminalGrant};

use super::*;

fn grant(sessions: &[&str], peer_supported: bool) -> LocalTerminalGrant {
    LocalTerminalGrant::from_answer(
        GrantMintAnswer {
            grant_id: "grant-a".to_owned(),
            secret: "secret-a".to_owned(),
            ttl_ms: 60_000,
            worker_epoch: "epoch-a".to_owned(),
            peer_supported,
            stun_urls: Vec::new(),
            input_route_supported: true,
        },
        "worker-a",
        BTreeSet::from_iter(sessions.iter().map(|s| (*s).to_owned())),
        "tab-a",
        "device-a",
        0,
    )
    .unwrap()
}

fn ready(sessions: &[&str]) -> LoopbackReady {
    LoopbackReady {
        worker_fingerprint: "worker-a".to_owned(),
        session_ids: BTreeSet::from_iter(sessions.iter().map(|s| (*s).to_owned())),
        socket_generation: 7,
        worker_epoch: "epoch-a".to_owned(),
        socket_id: "socket-a".to_owned(),
        peer_id: String::new(),
    }
}

#[test]
fn an_admitted_carrier_carries_exactly_the_sessions_its_ready_named() {
    let connection = LoopbackConnection::admit(
        &grant(&["session-a"], false),
        "worker-a",
        "conn-a",
        &ready(&["session-a"]),
    )
    .unwrap();
    assert!(connection.allows_session("session-a"));
    assert!(
        !connection.allows_session("session-b"),
        "a grant names exact sessions, never 'all'"
    );
    assert_eq!(connection.carrier().granted_sessions.len(), 1);
    assert_eq!(connection.carrier().connection_id, "conn-a");
}

#[test]
fn a_ready_naming_a_session_outside_the_grant_is_refused() {
    // The scope rule is the whole reason a grant is scope-bound: a worker
    // that admitted a session this document never asked for is a worker that
    // has answered for something else.
    assert!(
        LoopbackConnection::admit(
            &grant(&["session-a"], false),
            "worker-a",
            "conn-a",
            &ready(&["session-a", "session-b"]),
        )
        .is_err()
    );
}

#[test]
fn a_ready_from_another_worker_is_refused() {
    let mut foreign = ready(&["session-a"]);
    foreign.worker_fingerprint = "worker-b".to_owned();
    assert!(
        LoopbackConnection::admit(
            &grant(&["session-a"], false),
            "worker-a",
            "conn-a",
            &foreign
        )
        .is_err()
    );
}

#[test]
fn a_peer_id_on_a_loopback_socket_is_refused() {
    // Loopback presents no peer id, and `client::local::door::admit_ready`
    // reads one as a carrier answering for a negotiation it is not in.
    let mut peered = ready(&["session-a"]);
    peered.peer_id = "peer-1".to_owned();
    assert!(
        LoopbackConnection::admit(&grant(&["session-a"], false), "worker-a", "conn-a", &peered)
            .is_err()
    );
}

#[test]
fn a_rolling_worker_gets_the_per_connection_namespace() {
    // No epoch and no socket id is the compatibility case: a rolling worker
    // has no identity of its own, so the connection id is the only fence a
    // redial's frames can be told apart by.
    let mut rolling = ready(&["session-a"]);
    rolling.worker_epoch = String::new();
    rolling.socket_id = String::new();
    let connection = LoopbackConnection::admit(
        &grant(&["session-a"], false),
        "worker-a",
        "conn-a",
        &rolling,
    )
    .unwrap();
    assert_eq!(connection.carrier().token.process_epoch, "conn-a");
}

#[test]
fn a_command_for_a_session_this_carrier_does_not_hold_is_not_encoded() {
    let connection = LoopbackConnection::admit(
        &grant(&["session-a"], false),
        "worker-a",
        "conn-a",
        &ready(&["session-a"]),
    )
    .unwrap();
    let refused = connection.encode(&DirectCommand::Resync {
        session_id: "session-b".to_owned(),
        view_id: "view-b".to_owned(),
        stream_id: String::new(),
        grid_epoch: String::new(),
        seq: 0,
    });
    assert!(
        refused.is_none(),
        "a command outside the grant must not reach the wire"
    );
    let allowed = connection.encode(&DirectCommand::Resync {
        session_id: "session-a".to_owned(),
        view_id: "view-a".to_owned(),
        stream_id: String::new(),
        grid_epoch: String::new(),
        seq: 0,
    });
    assert!(allowed.is_some());
}

/// A `LocalTerminalServerFrame` whose `cell_grid` arm (field 3, length
/// delimited) carries an empty `PbCellGridFrame`.
///
/// Hand-encoded because this crate takes `roost-protocol`, not `roost-proto`,
/// and the cross-check against `receive_from` below is what makes these two
/// bytes a cell frame rather than an assertion about a tag the reader
/// happens to believe.
const CELL_FRAME: [u8; 2] = [0x1a, 0x00];

#[test]
fn a_cell_frame_before_the_handshake_is_out_of_order_not_a_grid() {
    assert!(
        matches!(
            LoopbackConnection::receive_from(&CELL_FRAME),
            Ok(DirectInbound::CellGrid { .. })
        ),
        "these bytes must be a real cell grid once the carrier is authenticated, \
             or this test proves nothing about the pre-hello rule"
    );
    let error = LoopbackConnection::receive_pre_hello(&CELL_FRAME).unwrap_err();
    assert!(
        matches!(error, CarrierFault::OutOfOrder { .. }),
        "{error:?}"
    );
}

#[test]
fn the_hello_carries_no_peer_id_and_no_worker_epoch() {
    // Both are empty ON PURPOSE on loopback: `admit_ready` reads their
    // absence as the rolling-worker case, and a loopback socket that claims
    // a peer is refused by the rule above.
    let grant = grant(&["session-a"], false);
    let connection =
        LoopbackConnection::admit(&grant, "worker-a", "conn-a", &ready(&["session-a"])).unwrap();
    assert!(connection.allows_session("session-a"));
    let hello = LoopbackConnection::hello(&grant);
    assert!(!hello.is_empty());
}
