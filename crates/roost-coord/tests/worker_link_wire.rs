//! The worker link over a real socket: the upgrade, the hello and its
//! acknowledgement, the snapshot that makes a generation routable, the frames
//! the link ignores, and the frames it writes for the rest of the coordinator.
//!
//! Every test dials the REAL router on an ephemeral loopback port, because the
//! properties here are what a worker observes on the wire. Ports
//! `apps/coord/tests/workers/worker-ws-transport{,-lifecycle}.test.ts` and
//! `worker-conn-pending-supersession.test.ts`.
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

use roost_protocol::wire::SessionEvent;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream, EventAck};
use worker_link_wire_support::{WireFixture, hello_bytes, next_downstream, upstream_bytes};
use ws_client_support::{Dialed, close_code, send_binary};

/// The authoritative snapshot a worker sends once its replay drained: here,
/// no live sessions.
fn empty_snapshot(fixture: &WireFixture, client_seq: u64) -> Vec<u8> {
    upstream_bytes(&CoordWorkerUpstream::Event {
        event: SessionEvent::Snapshot {
            worker_fp: fixture.fp(),
            sessions: Vec::new(),
            ts: 1_500,
            trace_id: None,
        },
        client_seq,
        trace_id: None,
    })
}

/// Wait until the registry answers `want` for "is this worker routable".
async fn routable_becomes(fixture: &WireFixture, want: bool) {
    for _ in 0..200 {
        if fixture
            .services
            .workers
            .routable_fps()
            .contains(&fixture.fp())
            == want
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the worker never became routable={want}");
}

// v2: worker-ws-transport.test.ts "dial + hello → helloAck, worker registered
// in the hub" — plus the echoed marker of `worker-ws-upgrade.ts:112-117`.
#[tokio::test]
async fn an_admitted_worker_upgrades_and_its_hello_is_acknowledged_before_it_is_routable() {
    let fixture = WireFixture::start("hello").await;

    let Dialed::Upgraded { protocol, socket } = fixture.dial_worker().await else {
        panic!("an enrolled worker's upgrade answers 101");
    };
    assert_eq!(
        protocol.as_deref(),
        Some("roost-worker-auth"),
        "the marker is echoed and the credential never is"
    );
    let mut socket = *socket;
    send_binary(&mut socket, hello_bytes(&fixture.worker_fp)).await;
    let ack = next_downstream(&mut socket).await.expect("a hello-ack");
    assert!(matches!(ack, CoordWorkerDownstream::HelloAck { .. }));

    // The hello owns the generation but it stays unroutable until its exact
    // snapshot commits and publishes.
    let current = fixture.services.workers.current(&fixture.fp());
    assert!(current.is_some(), "the hello claimed the fingerprint");
    assert!(
        !fixture
            .services
            .workers
            .routable_fps()
            .contains(&fixture.fp())
    );

    send_binary(&mut socket, empty_snapshot(&fixture, 1)).await;
    let acked = next_downstream(&mut socket)
        .await
        .expect("the snapshot's ack");
    assert_eq!(
        acked,
        CoordWorkerDownstream::EventAck(EventAck { client_seq: 1 })
    );
    routable_becomes(&fixture, true).await;
}

// v2: worker-ws-transport-lifecycle.test.ts "acknowledges terminal metadata
// only for an advertising worker", and worker-conn-pending-supersession.test.ts
// "tracks worker boot and socket generations without retaining superseded
// capabilities".
#[tokio::test]
async fn a_hello_is_acknowledged_with_only_the_capabilities_this_coordinator_serves() {
    let fixture = WireFixture::start("capabilities").await;

    let (_socket, ack) = fixture.hello_link().await;

    let CoordWorkerDownstream::HelloAck { capabilities, .. } = ack else {
        panic!("the first frame is the hello-ack");
    };
    assert_eq!(capabilities, vec!["terminal_metadata_v1".to_owned()]);
    let handle = fixture
        .services
        .workers
        .current(&fixture.fp())
        .expect("a generation");
    assert_eq!(
        handle.capabilities.iter().cloned().collect::<Vec<_>>(),
        vec!["terminal_metadata_v1".to_owned()],
        "the handle keeps what was acknowledged, never what was advertised"
    );
    assert_eq!(handle.process_epoch.as_deref(), Some("worker-epoch-1"));
}

// Standing ruling (v2 `worker-ws-handler.ts:209-212`): an undecodable frame is
// logged `decode_failed` and ignored; the link stays open.
#[tokio::test]
async fn an_undecodable_frame_is_ignored_and_the_next_frame_is_still_dispatched() {
    let fixture = WireFixture::start("garbage").await;
    let (mut socket, _ack) = fixture.hello_link().await;

    send_binary(&mut socket, vec![0xff, 0xff, 0xff, 0xff]).await;
    send_binary(&mut socket, empty_snapshot(&fixture, 1)).await;

    let acked = next_downstream(&mut socket)
        .await
        .expect("the link is still open");
    assert_eq!(
        acked,
        CoordWorkerDownstream::EventAck(EventAck { client_seq: 1 })
    );
}

// v2: worker-ws-transport.test.ts "downstream command + upstream rpc-ok
// resolves the pending RPC", the downstream half: what the rest of the
// coordinator sends through the registered handle reaches the socket.
#[tokio::test]
async fn a_frame_sent_through_the_registered_handle_reaches_the_worker() {
    let fixture = WireFixture::start("send").await;
    let (mut socket, _ack) = fixture.hello_link().await;
    let handle = fixture
        .services
        .workers
        .current(&fixture.fp())
        .expect("a generation");

    let sequence = handle.send(CoordWorkerDownstream::EventAck(EventAck { client_seq: 42 }));

    assert_ne!(sequence, 0, "a live generation never answers dropped");
    assert_eq!(
        next_downstream(&mut socket).await,
        Some(CoordWorkerDownstream::EventAck(EventAck { client_seq: 42 }))
    );
}

// v2: worker-conn-pending-supersession.test.ts "a replacement worker
// generation rejects predecessor RPCs", plus v2's `superseded.close()`.
#[tokio::test]
async fn a_replacement_hello_rejects_predecessor_requests_and_closes_the_old_socket() {
    let fixture = WireFixture::start("supersede").await;
    let (mut old_socket, _ack) = fixture.hello_link().await;
    let old_handle = fixture
        .services
        .workers
        .current(&fixture.fp())
        .expect("a generation");
    let mut pending = fixture
        .services
        .scrollback
        .pending()
        .create("predecessor-request", Some(&fixture.worker_fp), 0)
        .expect("a fresh request id");

    let (_new_socket, _ack) = fixture.hello_link().await;

    assert!(
        pending.settle().await.is_err(),
        "the predecessor's request fails fast"
    );
    assert_eq!(
        close_code(&mut old_socket, Duration::from_secs(5)).await,
        Some(None),
        "the superseded socket is closed with no code"
    );
    let current = fixture
        .services
        .workers
        .current(&fixture.fp())
        .expect("a generation");
    assert_ne!(
        current.connection_generation,
        old_handle.connection_generation
    );
    assert_eq!(
        old_handle.send(CoordWorkerDownstream::Ping {
            ts: 1,
            trace_id: None
        }),
        0
    );
}

// v2: worker-conn.ts `hello` — "Worker must announce the same fp that authed
// the JWT", else the socket closes.
#[tokio::test]
async fn a_hello_for_another_fingerprint_closes_the_socket_unregistered() {
    let fixture = WireFixture::start("fp-mismatch").await;
    let mut socket = fixture.dial_worker().await.socket();

    send_binary(&mut socket, hello_bytes(&"0".repeat(64))).await;

    assert_eq!(
        close_code(&mut socket, Duration::from_secs(5)).await,
        Some(None)
    );
    assert!(fixture.services.workers.current(&fixture.fp()).is_none());
}
