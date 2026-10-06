//! The Sync v1 retained seed on a `flow=1` socket: paced one frame per
//! acknowledgement however large it is, live frames held behind it in a FIFO
//! bounded like the window, and a seed nobody acknowledges closed at the ACK
//! deadline.
//!
//! Ports `apps/coord/tests/sync/sync-ws-keepalive-flow-control.test.ts` "ACK-paced
//! retained seed crosses 512 frames and a stalled seed exits at 3 seconds" at
//! the socket boundary. v2 filled the seed with 520 UI states by reaching into
//! the owner's map; the owner here caps retained tabs at 256, so the seed is
//! 520 retained last-activity observations instead -- the same frames-per-seed
//! property through the owner's public API.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod sync_seed_support;
mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::time::{Duration, Instant};

use roost_coord::events::bus_messages::SessionTitleUpdate;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::FirehoseFrame;

use sync_seed_support::{insert_sessions, insert_worker};
use sync_ws_socket_support::{EXPECT, QUIET, SyncFixture, next_firehose, send_client_frame};
use ws_client_support::close_code;

const WORKER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SEEDED_SESSIONS: usize = 520;

/// An install retaining more activity than one ACK window can carry, and a
/// browser credential for it.
async fn crowded_install(label: &str) -> (SyncFixture, String, Vec<String>) {
    let fixture = SyncFixture::start(label).await;
    let (_fingerprint, token) = fixture.enroll_browser(41).await;
    insert_worker(&fixture, WORKER).await;
    let sessions = insert_sessions(&fixture, SEEDED_SESSIONS, WORKER).await;
    for session_id in &sessions {
        fixture
            .services
            .feed
            .last_activity()
            .observe(session_id, 1_000);
    }
    (fixture, token, sessions)
}

fn publish_title(fixture: &SyncFixture, session_id: &str, title: String) {
    fixture
        .services
        .buses
        .title_bus
        .publish(SessionTitleUpdate {
            session_id: session_id.to_owned(),
            title,
        });
}

fn kind_of(frame: &FirehoseFrame) -> &'static str {
    match frame.frame {
        Some(Frame::WorkerRoutable(_)) => "worker_routable",
        Some(Frame::LastActivity(_)) => "last_activity",
        Some(Frame::TerminalTitle(_)) => "terminal_title",
        _ => "other",
    }
}

// v2 "ACK-paced retained seed crosses 512 frames" (healthy socket): each seed
// frame waits for the previous one's acknowledgement, the seed runs past the
// 512-frame window, and a title published while it runs arrives after all of
// it. Once the seed is acknowledged, live frames flow without waiting.
#[tokio::test]
async fn an_ack_paced_retained_seed_crosses_the_window_and_then_goes_live() {
    let (fixture, token, sessions) = crowded_install("paced").await;
    let mut socket = fixture.dial_sync("flow=1", &token).await.socket();
    let mut kinds = Vec::new();
    loop {
        let frame = next_firehose(&mut socket, EXPECT)
            .await
            .expect("an acknowledged seed keeps flowing");
        assert_eq!(frame.delivery_seq, kinds.len() as u64 + 1);
        if kinds.is_empty() {
            publish_title(&fixture, &sessions[0], "live-during-seed".to_owned());
            assert!(
                next_firehose(&mut socket, QUIET).await.is_none(),
                "the next seed frame waits for this one's acknowledgement"
            );
        }
        kinds.push(kind_of(&frame));
        send_client_frame(&mut socket, "", Some(frame.delivery_seq), None).await;
        if kind_of(&frame) == "terminal_title" {
            break;
        }
    }
    assert_eq!(kinds[0], "worker_routable", "routability seeds first");
    let activity = kinds
        .iter()
        .filter(|kind| **kind == "last_activity")
        .count();
    assert_eq!(activity, SEEDED_SESSIONS);
    assert_eq!(kinds.len(), SEEDED_SESSIONS + 2);
    assert!(kinds.len() > 512, "the seed crossed one window's worth");

    // The seed ends when its last frame's acknowledgement is read; a title
    // published before that would still be paced behind it.
    tokio::time::sleep(QUIET).await;
    let sent = kinds.len() as u64;
    publish_title(&fixture, &sessions[0], "after-seed-1".to_owned());
    publish_title(&fixture, &sessions[0], "after-seed-2".to_owned());
    for expected in [sent + 1, sent + 2] {
        let frame = next_firehose(&mut socket, EXPECT)
            .await
            .expect("live flows");
        assert_eq!(
            (kind_of(&frame), frame.delivery_seq),
            ("terminal_title", expected)
        );
    }
}

// v2 "ACK-paced retained seed crosses 512 frames" (bounded and byte-bounded
// sockets): live frames that arrive while a seed frame waits are held in a
// FIFO bounded by 512 frames and 4 MiB; the frame past either bound closes
// the socket 1013 instead of growing the queue.
#[tokio::test]
async fn live_frames_held_behind_a_seed_are_bounded_like_the_window() {
    let (fixture, token, sessions) = crowded_install("paced-bounded").await;
    let mut socket = fixture.dial_sync("flow=1", &token).await.socket();
    let first = next_firehose(&mut socket, EXPECT).await.expect("seed");
    assert_eq!(first.delivery_seq, 1);
    for index in 0..512 {
        publish_title(&fixture, &sessions[0], format!("queued-live-{index}"));
    }
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "512 held live frames fit behind the unacknowledged seed frame"
    );
    publish_title(&fixture, &sessions[0], "queued-live-overflow".to_owned());
    assert_eq!(close_code(&mut socket, EXPECT).await, Some(Some(1013)));

    let mut bytes = fixture.dial_sync("flow=1", &token).await.socket();
    let first = next_firehose(&mut bytes, EXPECT).await.expect("seed");
    assert_eq!(first.delivery_seq, 1);
    publish_title(&fixture, &sessions[0], "x".repeat(4 * 1024 * 1024));
    assert_eq!(close_code(&mut bytes, EXPECT).await, Some(Some(1013)));
}

// v2 "a stalled seed exits at 3 seconds": a seed frame nobody acknowledges
// holds the window, and the ACK deadline closes the socket 1013.
#[tokio::test]
async fn an_unacknowledged_seed_closes_at_the_ack_deadline() {
    let (fixture, token, _sessions) = crowded_install("paced-stalled").await;
    let mut socket = fixture.dial_sync("flow=1", &token).await.socket();
    let first = next_firehose(&mut socket, EXPECT).await.expect("seed");
    let sent_at = Instant::now();
    assert_eq!(first.delivery_seq, 1);
    assert_eq!(
        close_code(&mut socket, Duration::from_secs(6)).await,
        Some(Some(1013))
    );
    assert!(
        sent_at.elapsed() >= Duration::from_millis(2_500),
        "the close waited out the 3 s deadline, not a shorter one"
    );
}
