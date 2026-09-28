//! Unauthenticated loopback sockets are finite and expire before they can hold
//! worker descriptors or socket-owner rows indefinitely; replaying one grant
//! replaces its socket and authenticated grants stay capped. Ports
//! `apps/worker/tests/local-door/local-terminal-prehello.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_HELLO_DEADLINE_MS, TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER,
};
use roost_worker::local_door::{AuthenticatedAdmission, LocalTerminalPreHelloOwner};

#[tokio::test(start_paused = true)]
async fn caps_and_expires_loopback_sockets_awaiting_hello() {
    let expired = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&expired);
    let owner = LocalTerminalPreHelloOwner::new(
        Arc::new(move |socket_id: &str| sink.lock().unwrap().push(socket_id.to_owned())),
        tokio::runtime::Handle::current(),
    );
    for index in 0..TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER {
        assert!(owner.admit(&format!("socket-{index}")));
    }
    assert!(!owner.admit("socket-overflow"));
    owner.clear("socket-0");
    assert!(owner.admit("socket-replacement"));

    tokio::time::sleep(Duration::from_millis(TERMINAL_PEER_HELLO_DEADLINE_MS - 1)).await;
    assert!(expired.lock().unwrap().is_empty(), "nothing expires early");
    tokio::time::sleep(Duration::from_millis(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    let expired = expired.lock().unwrap().clone();
    assert_eq!(expired.len(), TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER);
    assert!(
        !expired.contains(&"socket-0".to_owned()),
        "a cleared socket never expires"
    );
    assert!(expired.contains(&"socket-replacement".to_owned()));
    owner.dispose();
}

#[tokio::test]
async fn replaying_one_grant_replaces_its_socket_and_authenticated_grants_stay_capped() {
    let owner =
        LocalTerminalPreHelloOwner::new(Arc::new(|_: &str| {}), tokio::runtime::Handle::current());
    assert!(owner.admit("socket-a"));
    assert_eq!(
        owner.authenticate("grant-a", "socket-a"),
        AuthenticatedAdmission {
            admitted: true,
            replaced_socket_id: None
        }
    );
    assert!(owner.admit("socket-b"));
    assert_eq!(
        owner.authenticate("grant-a", "socket-b"),
        AuthenticatedAdmission {
            admitted: true,
            replaced_socket_id: Some("socket-a".to_owned())
        }
    );
    owner.retire("socket-a");

    for index in 1..TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER {
        let socket_id = format!("authenticated-{index}");
        assert!(owner.admit(&socket_id));
        assert!(
            owner
                .authenticate(&format!("grant-{index}"), &socket_id)
                .admitted
        );
    }
    assert!(owner.admit("authenticated-overflow"));
    assert!(
        !owner
            .authenticate("grant-overflow", "authenticated-overflow")
            .admitted
    );
    owner.retire("authenticated-overflow");
    owner.dispose();
}
