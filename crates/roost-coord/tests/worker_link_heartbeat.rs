//! The worker link's application heartbeat over a real socket, on a paused
//! tokio clock: one ping per delay, only its exact pong beats the deadline, and
//! a superseded socket's deadline cannot touch its replacement.
//!
//! Ports `apps/coord/tests/workers/worker-ws-transport-heartbeat.test.ts`. The
//! upgrade and the first hello run on the real clock (they read the database);
//! the clock is paused only once every remaining step is socket traffic. While
//! paused, the client NEVER waits on a timer: an idle paused runtime
//! auto-advances to its next timer before real loopback bytes are observed, so
//! every read here polls between yields and time moves only by `advance`.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod worker_link_wire_support;
mod ws_client_support;
mod ws_credential_support;

use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::StreamExt as _;
use roost_coord::worker_link::keepalive::{WORKER_PING_DELAY, WORKER_PONG_TIMEOUT};
use roost_protocol::proto_adapters::coord_worker_proto::decode_downstream;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio_tungstenite::tungstenite::Message;
use worker_link_wire_support::{WireFixture, hello_bytes, pong_bytes};
use ws_client_support::{WsClient, send_binary};

const ONE_MS: Duration = Duration::from_millis(1);

/// Past a deadline by enough to cross it: tokio rounds a timer's deadline UP to
/// its 1 ms tick, and the link's instants carry a sub-millisecond phase from
/// the real clock the hello ran on, so "exactly at" may land one tick early.
/// Every "not yet" check stays at 1 ms BEFORE the deadline, which is exact.
const TICK_SLACK: Duration = Duration::from_millis(2);

/// How many yields a read may take before the frame is declared absent.
const POLL_YIELDS: usize = 20_000;

/// Let the coordinator's tasks run: each yield also polls the socket driver,
/// and a runtime that is never idle never auto-advances its paused clock.
async fn settle() {
    for _ in 0..256 {
        tokio::task::yield_now().await;
    }
}

/// The next data or close frame, polled between yields; `None` if none came.
async fn poll_frame(socket: &mut WsClient) -> Option<Message> {
    for _ in 0..POLL_YIELDS {
        match socket.next().now_or_never() {
            None | Some(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            Some(Some(Ok(message))) => return Some(message),
            Some(Some(Err(_)) | None) => return None,
        }
        tokio::task::yield_now().await;
    }
    None
}

/// The next coordinator frame, decoded; `None` for a close, an end, or nothing.
async fn poll_downstream(socket: &mut WsClient) -> Option<CoordWorkerDownstream> {
    match poll_frame(socket).await? {
        Message::Binary(bytes) => Some(decode_downstream(&bytes).expect("a coordinator frame")),
        _ => None,
    }
}

/// The close code the coordinator sent, reading past data frames.
async fn poll_close_code(socket: &mut WsClient) -> Option<Option<u16>> {
    loop {
        if let Message::Close(frame) = poll_frame(socket).await? {
            return Some(frame.map(|frame| u16::from(frame.code)));
        }
    }
}

/// The ping the coordinator just sent, as its generation.
async fn expect_ping(socket: &mut WsClient) -> i64 {
    match poll_downstream(socket).await {
        Some(CoordWorkerDownstream::Ping { ts, .. }) => ts,
        other => panic!("expected a ping, got {other:?}"),
    }
}

/// Nothing is waiting on the socket right now: no frame, no close.
async fn nothing_arrived(socket: &mut WsClient) -> bool {
    settle().await;
    socket.next().now_or_never().is_none()
}

// v2: "sends one ping and lets only its exact pong beat the deadline".
#[tokio::test]
async fn one_ping_is_sent_and_only_its_exact_pong_beats_the_deadline() {
    let fixture = WireFixture::start("heartbeat-exact").await;
    let (mut socket, _ack) = fixture.hello_link().await;
    tokio::time::pause();

    tokio::time::advance(WORKER_PING_DELAY + ONE_MS).await;
    let ping = expect_ping(&mut socket).await;
    send_binary(&mut socket, pong_bytes(ping + 1)).await;
    settle().await;

    tokio::time::advance(WORKER_PONG_TIMEOUT - ONE_MS).await;
    assert!(
        nothing_arrived(&mut socket).await,
        "no second ping and no close yet"
    );
    assert!(fixture.services.workers.current(&fixture.fp()).is_some());

    tokio::time::advance(TICK_SLACK).await;
    assert_eq!(
        poll_close_code(&mut socket).await,
        Some(None),
        "an unanswered ping closes the link with no code"
    );
    assert!(
        fixture.services.workers.current(&fixture.fp()).is_none(),
        "and the generation is gone"
    );
}

// v2: "an exact late pong cancels its deadline and starts a fresh 30s delay".
#[tokio::test]
async fn an_exact_late_pong_cancels_its_deadline_and_starts_a_fresh_delay() {
    let fixture = WireFixture::start("heartbeat-late").await;
    let (mut socket, _ack) = fixture.hello_link().await;
    tokio::time::pause();

    tokio::time::advance(WORKER_PING_DELAY + ONE_MS).await;
    let first = expect_ping(&mut socket).await;
    tokio::time::advance(Duration::from_secs(70)).await;
    send_binary(&mut socket, pong_bytes(first)).await;
    settle().await;

    tokio::time::advance(Duration::from_secs(20)).await;
    assert!(
        nothing_arrived(&mut socket).await,
        "the deadline was cancelled"
    );
    tokio::time::advance(WORKER_PING_DELAY - Duration::from_secs(20) - ONE_MS).await;
    assert!(
        nothing_arrived(&mut socket).await,
        "the fresh delay has not run out"
    );

    tokio::time::advance(TICK_SLACK).await;
    let second = expect_ping(&mut socket).await;
    assert_eq!(second, first + 1, "each ping carries the next generation");
    assert!(fixture.services.workers.current(&fixture.fp()).is_some());
}

// v2: "a superseded socket's captured deadline cannot close its replacement".
#[tokio::test]
async fn a_superseded_sockets_deadline_cannot_close_its_replacement() {
    let fixture = WireFixture::start("heartbeat-supersede").await;
    let (mut old_socket, _ack) = fixture.hello_link().await;
    // Upgraded before the pause: the upgrade verifies against the database.
    let mut new_socket = fixture.dial_worker().await.socket();
    tokio::time::pause();

    tokio::time::advance(WORKER_PING_DELAY + ONE_MS).await;
    expect_ping(&mut old_socket).await;
    send_binary(&mut new_socket, hello_bytes(&fixture.worker_fp)).await;
    assert!(matches!(
        poll_downstream(&mut new_socket).await,
        Some(CoordWorkerDownstream::HelloAck { .. })
    ));
    assert_eq!(
        poll_close_code(&mut old_socket).await,
        Some(None),
        "the newer hello closes the superseded socket"
    );
    let replacement = fixture
        .services
        .workers
        .current(&fixture.fp())
        .expect("a generation");

    tokio::time::advance(WORKER_PONG_TIMEOUT).await;
    settle().await;
    let current = fixture
        .services
        .workers
        .current(&fixture.fp())
        .expect("still registered");
    assert_eq!(
        current.connection_generation,
        replacement.connection_generation
    );
    assert!(!current.is_revoked(), "the old deadline reached nothing");
}
