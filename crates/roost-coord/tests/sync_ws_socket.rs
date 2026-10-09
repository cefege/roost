//! The Sync WebSocket over a real listener: an admitted upgrade switches
//! protocols with the marker echoed, a v2 domain's frames are held until
//! `domain_ready` and then flow in order, an ACK releases a frame the full
//! window held, and a v1 socket is sequenced and live.
//!
//! Ports the socket-level cases of `apps/coord/tests/sync/sync-ws-keepalive*.test.ts`
//! and `sync-audit-subscription.test.ts` that the live path owns.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::SyncDomain;

use sync_ws_socket_support::{
    EXPECT, QUIET, SyncFixture, domain_ready, generation_of, next_firehose, read_subscribed,
    send_client_frame, task_id_of,
};
use ws_client_support::{Dialed, close_code};

/// A task payload that fits the 4 MiB window three times and not four.
const LARGE_PAYLOAD: usize = 1_200_000;

// v2 sync-ws-keepalive-upgrade.test.ts: an admitted upgrade answers 101 and
// echoes `roost-auth`, never the credential; the v2 socket opens with the
// `subscribed` barrier naming this process's epoch and all eight domains.
#[tokio::test]
async fn an_admitted_sync_upgrade_switches_protocols_and_echoes_only_the_marker() {
    let fixture = SyncFixture::start("upgrade").await;
    let (_fp, token) = fixture.enroll_browser(7).await;
    let Dialed::Upgraded {
        protocol,
        mut socket,
    } = fixture.dial_sync("flow=1&sync_v=2", &token).await
    else {
        panic!("the admitted upgrade was refused");
    };
    assert_eq!(protocol.as_deref(), Some("roost-auth"));
    let subscribed = read_subscribed(&mut socket).await;
    assert!(!subscribed.socket_id.is_empty());
    assert_eq!(
        subscribed.process_epoch,
        fixture.services.feed.process_epoch()
    );
    assert_eq!(subscribed.generations.len(), 8);
    let audit = subscribed
        .generations
        .iter()
        .find(|entry| entry.domain.as_known() == Some(SyncDomain::Audit))
        .expect("audit is announced");
    assert!(!audit.subscribed, "audit is the only lazy domain");
}

// v2 sync-ws-keepalive-upgrade.test.ts "rejects missing or malformed auth
// subprotocols before upgrade": an unverifiable credential never switches.
#[tokio::test]
async fn an_unverifiable_credential_is_refused_before_the_upgrade() {
    let fixture = SyncFixture::start("refused").await;
    match fixture.dial_sync("flow=1&sync_v=2", "not.a.jwt").await {
        Dialed::Refused { status } => assert_eq!(status, 401),
        Dialed::Upgraded { .. } => panic!("an unverifiable credential was upgraded"),
    }
}

// v2 sync-audit-subscription.test.ts "holds live rows through the snapshot
// barrier", for the tasks domain: frames published before `domain_ready` are
// held, and after it they flow in publication order under sequences 1, 2, 3
// stamped with the domain's announced generation.
#[tokio::test]
async fn a_domain_is_held_until_ready_and_then_flows_in_order() {
    let fixture = SyncFixture::start("ready").await;
    let (_fp, token) = fixture.enroll_browser(8).await;
    let mut socket = fixture.dial_sync("flow=1&sync_v=2", &token).await.socket();
    let subscribed = read_subscribed(&mut socket).await;
    let generation = generation_of(&subscribed, SyncDomain::Tasks);

    fixture.publish_task("task-1", 8);
    fixture.publish_task("task-2", 8);
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "a domain that is not ready delivers nothing"
    );

    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(domain_ready(SyncDomain::Tasks, generation)),
    )
    .await;
    fixture.publish_task("task-3", 8);
    for (expected_seq, expected_id) in [(1, "task-1"), (2, "task-2"), (3, "task-3")] {
        let frame = next_firehose(&mut socket, EXPECT)
            .await
            .expect("a task frame");
        assert_eq!(frame.delivery_seq, expected_seq);
        assert_eq!(frame.domain.as_known(), Some(SyncDomain::Tasks));
        assert_eq!(frame.domain_generation, generation);
        assert_eq!(task_id_of(&frame), expected_id);
    }
}

// v2 sync-ws-keepalive-flow-control.test.ts "application byte preflight
// accepts through 4 MiB and rejects the next candidate" and "cumulative ACK
// releases records": on v2 the rejected candidate is HELD, not closed, and the
// acknowledgement that frees the window sends it.
#[tokio::test]
async fn an_ack_releases_the_frame_a_full_window_held() {
    let fixture = SyncFixture::start("window").await;
    let (_fp, token) = fixture.enroll_browser(9).await;
    let mut socket = fixture.dial_sync("flow=1&sync_v=2", &token).await.socket();
    let subscribed = read_subscribed(&mut socket).await;
    let generation = generation_of(&subscribed, SyncDomain::Tasks);
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(domain_ready(SyncDomain::Tasks, generation)),
    )
    .await;

    for (seq, id) in [(1, "big-1"), (2, "big-2"), (3, "big-3")] {
        fixture.publish_task(id, LARGE_PAYLOAD);
        let frame = next_firehose(&mut socket, EXPECT)
            .await
            .expect("a large frame");
        assert_eq!((frame.delivery_seq, task_id_of(&frame).as_str()), (seq, id));
    }
    fixture.publish_task("big-4", LARGE_PAYLOAD);
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "the fourth frame would pass 4 MiB unacknowledged"
    );

    send_client_frame(&mut socket, &subscribed.socket_id, Some(3), None).await;
    let held = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the held frame");
    assert_eq!(
        (held.delivery_seq, task_id_of(&held).as_str()),
        (4, "big-4")
    );

    // v2 "equal and stale cumulative ACKs are idempotent": a repeated or
    // older acknowledgement with no command releases nothing and keeps the
    // socket open, so the next frame still flows.
    send_client_frame(&mut socket, &subscribed.socket_id, Some(3), None).await;
    send_client_frame(&mut socket, &subscribed.socket_id, Some(2), None).await;
    fixture.publish_task("small-5", 8);
    let next = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the socket is still open");
    assert_eq!(
        (next.delivery_seq, task_id_of(&next).as_str()),
        (5, "small-5")
    );
}

// v2 sync-ws-keepalive-flow-control.test.ts "future and malformed ACKs close
// 1008": an acknowledgement above the last sent sequence is a violation.
#[tokio::test]
async fn an_ack_above_the_last_sent_sequence_closes_1008() {
    let fixture = SyncFixture::start("future-ack").await;
    let (_fp, token) = fixture.enroll_browser(10).await;
    let mut socket = fixture.dial_sync("flow=1&sync_v=2", &token).await.socket();
    let subscribed = read_subscribed(&mut socket).await;
    send_client_frame(&mut socket, &subscribed.socket_id, Some(5), None).await;
    assert_eq!(close_code(&mut socket, EXPECT).await, Some(Some(1008)));
}

// v2 sync-ws-keepalive-flow-control.test.ts "legacy sockets remain
// unsequenced" and the v1 live path of sync-ws-v1-delivery.ts: a v1 socket
// gets no barrier and no domain fence -- its retained seed, then live frames
// -- a `flow=1` one is sequenced from 1, and a v2-shaped frame on it closes
// 1008.
#[tokio::test]
async fn a_v1_socket_is_live_and_sequenced_only_with_flow() {
    let fixture = SyncFixture::start("v1").await;
    let (_fp, token) = fixture.enroll_browser(11).await;

    let mut sequenced = fixture.dial_sync("flow=1", &token).await.socket();
    let mut legacy = fixture.dial_sync("", &token).await.socket();
    for (socket, sequence) in [(&mut sequenced, 1), (&mut legacy, 0)] {
        let seed = next_firehose(socket, EXPECT)
            .await
            .expect("the retained seed");
        assert!(
            matches!(seed.frame, Some(Frame::WorkerRoutable(_))),
            "no barrier on v1"
        );
        assert_eq!(seed.delivery_seq, sequence);
    }
    send_client_frame(&mut sequenced, "", Some(1), None).await;
    tokio::time::sleep(QUIET).await;
    fixture.publish_task("live-1", 8);

    let frame = next_firehose(&mut sequenced, EXPECT)
        .await
        .expect("a live frame");
    assert!(matches!(frame.frame, Some(Frame::TaskDelta(_))));
    assert_eq!(frame.delivery_seq, 2);
    let frame = next_firehose(&mut legacy, EXPECT)
        .await
        .expect("a live frame");
    assert_eq!(
        (frame.delivery_seq, task_id_of(&frame).as_str()),
        (0, "live-1")
    );

    send_client_frame(&mut sequenced, "", Some(2), None).await;
    send_client_frame(&mut sequenced, "a-v2-socket", Some(2), None).await;
    assert_eq!(close_code(&mut sequenced, EXPECT).await, Some(Some(1008)));
}
