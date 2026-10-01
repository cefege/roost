//! The direct loopback route, end to end, at every step a host can reach
//! without a browser: a minted grant, a discovered door, the socket's exact
//! arguments, the worker's `Ready`, the admitted carrier, and what a
//! `SendDirect` becomes on it.
//!
//! The socket itself is a `WebSocket` and cannot be constructed in a native
//! test binary, so the browser step is the ONE thing proved by reading
//! `platform/loopback.rs` rather than by running it. Everything the socket
//! depends on — the URL, the credential, the admission, the fold, the send
//! decision — is proved here, and each of those is a place where a wrong answer
//! produces a carrier that carries nothing.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_client_core::TerminalToken;
use roost_client_core::TerminalTransport;
use roost_client_core::client::carriers::wire::{decode_server_frame, encode_direct_command};
use roost_client_core::client::carriers::{Delivery, SendFault};
use roost_client_core::client::local::discovery::LocalWorkerDoor;
use roost_client_core::client::local::door::LoopbackReady;
use roost_client_core::client::local::{GrantMintAnswer, LocalTerminalGrant};
use roost_client_core::effect::DirectCommand;
use roost_client_core::terminal::view::ViewIntent;
use roost_web::platform::carriers::LoopbackConnection;
use roost_web::platform::carriers::delivery;
use roost_web::platform::carriers::dial::plan;

const CONNECTION_ID: &str = "loopback-workeraa-7-4242";

fn grant(sessions: &[&str]) -> LocalTerminalGrant {
    LocalTerminalGrant::from_answer(
        GrantMintAnswer {
            grant_id: "grant-a".to_owned(),
            secret: "secret-a".to_owned(),
            ttl_ms: 60_000,
            worker_epoch: "epoch-a".to_owned(),
            peer_supported: false,
            stun_urls: Vec::new(),
            input_route_supported: true,
        },
        "worker-a",
        BTreeSet::from_iter(sessions.iter().map(|session| (*session).to_owned())),
        "tab-a",
        "device-a",
        0,
    )
    .unwrap()
}

fn door() -> LocalWorkerDoor {
    LocalWorkerDoor {
        origin: "http://127.0.0.1:4114".to_owned(),
        worker_fingerprint: "worker-a".to_owned(),
    }
}

fn ready(sessions: &[&str]) -> LoopbackReady {
    LoopbackReady {
        worker_fingerprint: "worker-a".to_owned(),
        session_ids: BTreeSet::from_iter(sessions.iter().map(|session| (*session).to_owned())),
        socket_generation: 7,
        worker_epoch: "epoch-a".to_owned(),
        socket_id: "socket-a".to_owned(),
        peer_id: String::new(),
    }
}

fn publish(session_id: &str) -> DirectCommand {
    DirectCommand::View {
        session_id: session_id.to_owned(),
        view_id: "view-a".to_owned(),
        intent: ViewIntent::Publish { cols: 80, rows: 24 },
        revision: 1,
    }
}

#[test]
fn a_mint_dials_the_door_and_spends_the_credential_on_it() {
    // Steps one and two: the grant the coordinator installed, the door this page
    // adopted, and the exact arguments the socket is opened with. A `Ready` on a
    // socket opened with anything else is refused by the worker, and the
    // refusal arrives as a close with no reason on it.
    let dial = plan(&grant(&["session-a"]), Some(&door())).expect("a matching door dials");
    assert_eq!(dial.url, "ws://127.0.0.1:4114/ws/local-terminal");
    assert!(!dial.hello.is_empty());
    assert_eq!(
        dial.hello,
        LoopbackConnection::hello(&grant(&["session-a"])),
        "the plan's credential is the one the carrier's own encoder produces"
    );
}

#[test]
fn a_ready_admits_a_carrier_that_carries_exactly_the_sessions_it_named() {
    // Step three: the worker's proof, judged by the core's rule. This is the
    // only gate between an open socket and an elected route.
    let connection = LoopbackConnection::admit(
        &grant(&["session-a"]),
        "worker-a",
        CONNECTION_ID,
        &ready(&["session-a"]),
    )
    .expect("a matching Ready is admitted");
    let carrier = connection.carrier();
    assert_eq!(carrier.connection_id, CONNECTION_ID);
    assert_eq!(carrier.worker_fp, "worker-a");
    assert_eq!(carrier.transport, TerminalTransport::Loopback);
    assert_eq!(
        carrier.token.process_epoch, "epoch-a",
        "the route is fenced to the worker process, not to the socket"
    );
    assert_eq!(
        carrier.granted_sessions,
        BTreeSet::from(["session-a".to_owned()])
    );
}

#[test]
fn a_send_direct_on_an_admitted_carrier_is_encoded_rather_than_refused() {
    // Step four, and the whole point of the file: a command on a live carrier
    // becomes bytes. `NoLiveCarrier` here is the defect this layer was opened
    // for, and it is indistinguishable from a working path that lost them.
    let connection = LoopbackConnection::admit(
        &grant(&["session-a"]),
        "worker-a",
        CONNECTION_ID,
        &ready(&["session-a"]),
    )
    .expect("a matching Ready is admitted");
    let outcome = delivery(
        &connection.carrier().token,
        &publish("session-a"),
        Some(&connection.carrier().granted_sessions),
    );
    let Delivery::Encoded(bytes) = outcome else {
        panic!("a command on an admitted session must be encoded: {outcome:?}");
    };
    assert_eq!(
        bytes,
        encode_direct_command(&publish("session-a")),
        "the bytes on the wire are the effect's, never a re-read of the store"
    );
}

#[test]
fn a_send_direct_with_no_live_carrier_is_a_named_fault_naming_the_epoch() {
    // The rule the whole layer exists to keep. The fault is not a bare `None`:
    // it names the worker, the transport AND the process epoch, because a fault
    // with no generation in it cannot be traced to a route.
    let token = TerminalToken::direct(7, TerminalTransport::Loopback, "worker-a", "epoch-a", 7);
    let Delivery::Refused(fault) = delivery(&token, &publish("session-a"), None) else {
        panic!("a command with no carrier must be refused");
    };
    assert_eq!(
        fault,
        SendFault::NoLiveCarrier {
            worker_fp: "worker-a".to_owned(),
            transport: TerminalTransport::Loopback,
            process_epoch: "epoch-a".to_owned(),
        }
    );
    let rendered = fault.to_string();
    assert!(
        rendered.contains("epoch-a") && rendered.contains("worker-a"),
        "the report has to say which route died: {rendered}"
    );
}

#[test]
fn a_send_direct_outside_the_granted_scope_is_refused_by_name_and_not_merely_absent() {
    // The distinction the host exists to preserve: a carrier IS presenting this
    // generation and its grant does not name the session, which is a different
    // repair from a carrier that is gone.
    let connection = LoopbackConnection::admit(
        &grant(&["session-a"]),
        "worker-a",
        CONNECTION_ID,
        &ready(&["session-a"]),
    )
    .expect("a matching Ready is admitted");
    let Delivery::Refused(fault) = delivery(
        &connection.carrier().token,
        &publish("session-b"),
        Some(&connection.carrier().granted_sessions),
    ) else {
        panic!("a command outside the grant must be refused");
    };
    assert_eq!(
        fault,
        SendFault::SessionNotAdmitted {
            session_id: "session-b".to_owned()
        }
    );
}

#[test]
fn a_send_direct_naming_a_sync_token_finds_no_generation_at_all() {
    // A Sync route named as direct is the caller electing the wrong route, and
    // it is a third fault again: there is no worker to look for.
    let token = TerminalToken::sync(1, "socket-a", "epoch-a", 1);
    let Delivery::Refused(fault) = delivery(&token, &publish("session-a"), None) else {
        panic!("a Sync token cannot be delivered directly");
    };
    assert_eq!(fault, SendFault::NoGeneration);
}

#[test]
fn the_first_frame_after_admission_is_folded_and_never_read_as_a_handshake() {
    // The last step: a cell frame off an authenticated carrier becomes a
    // `SyncFrame` the core's direct fold can stage. `[0x1a, 0x00]` is field 3
    // length-delimited — the `cell_grid` arm — carrying an empty grid, and the
    // authenticated read is what makes those two bytes a cell frame rather than
    // a claim about a tag.
    const CELL_FRAME: [u8; 2] = [0x1a, 0x00];
    let inbound = decode_server_frame(&CELL_FRAME, true).expect("a cell frame decodes");
    let Some(frame) = inbound.as_sync_frame(7) else {
        panic!("a cell frame must translate into something the direct fold can stage");
    };
    assert_eq!(frame.session_id(), Some(""));
    assert_eq!(frame.kind_name(), "cell_grid");
}

#[test]
fn a_second_ready_is_refused_rather_than_re_admitting_a_live_carrier() {
    // The handshake is spent once. A worker that repeats it is either a
    // reconnecting authority or a fault, and neither may silently replace the
    // generation this carrier is presenting.
    let inbound = decode_server_frame(&[0x0a, 0x00], true);
    assert!(
        inbound.is_err(),
        "a repeated Ready on an authenticated carrier must not decode as a frame"
    );
    assert!(
        decode_server_frame(&[0x0a, 0x00], false).is_ok(),
        "the same bytes ARE a legal first frame, which is what makes this the \
         repeated-handshake case rather than an undecodable one"
    );
}
