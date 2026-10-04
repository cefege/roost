//! The three states a keeper probe tells apart on the endpoint's socket path:
//! never published, published by a keeper that has since died, and published
//! by a keeper that accepted and is saying nothing. The first two mean "start a
//! fresh keeper"; only the third is a busy keeper and a timeout. A reboot
//! leaves the second state behind, and reading it as the third refused every
//! boot until someone deleted the socket by hand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use roost_keeper::capability::KeeperCapability;
use roost_keeper::client::KeeperEndpoint;
use roost_worker::boot_keeper::IDENTITY_DEADLINE;
use roost_worker::runtime::keeper_boot::KeeperProbe;
use roost_worker::runtime::keeper_probe::probe;
use scratch::Scratch;

fn endpoint_in(scratch: &Scratch) -> KeeperEndpoint {
    KeeperEndpoint {
        socket: scratch.path("mux-keeper.sock"),
        capability: KeeperCapability::load_or_create(&scratch.path("mux-keeper.cap"))
            .expect("the fixture creates a capability"),
    }
}

#[tokio::test]
async fn a_published_socket_nothing_listens_on_is_an_empty_endpoint() {
    let scratch = Scratch::new("probe");
    let endpoint = endpoint_in(&scratch);
    // The socket file outlives its listener: the state a reboot leaves behind.
    drop(UnixListener::bind(&endpoint.socket).expect("the fixture binds the socket"));

    let started = Instant::now();
    let (outcome, client) = probe(&endpoint, "").await;

    assert!(client.is_none());
    assert!(
        matches!(&outcome, KeeperProbe::Probed(result) if !result.reachable),
        "a refused connection is nothing listening, got {outcome:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a refused connection must not wait out the identity deadline, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_published_socket_that_accepts_and_says_nothing_times_out() {
    let scratch = Scratch::new("probe");
    let endpoint = endpoint_in(&scratch);
    let listener = UnixListener::bind(&endpoint.socket).expect("the fixture binds the socket");
    // Accepted connections are held open and never answered, as a keeper busy
    // serving another worker does.
    std::thread::spawn(move || {
        let mut held: Vec<UnixStream> = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });

    let (outcome, client) = probe(&endpoint, "").await;

    assert!(client.is_none());
    assert_eq!(
        outcome,
        KeeperProbe::TimedOut {
            deadline: IDENTITY_DEADLINE
        }
    );
}
