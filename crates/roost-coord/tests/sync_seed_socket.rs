//! What a Sync socket is owed besides live frames, end to end over a real
//! listener: durable recovery above `since` (v1 after its seed, v2 behind the
//! terminal fence, and the reset a cursor ahead of the log gets), and the v2
//! retained replay each `domain_ready` asks for, ahead of the live frames the
//! domain buffered.
//!
//! Ports the behaviour of `apps/coord/src/sync/sync-feed.ts` (`backfill`),
//! `sync-feed-seed.ts` (`seedDomain`) and `sync-ws-v2-commands.ts:106-135`;
//! contract `docs/phase3-coord-contract.md` §8.5.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_seed_support;
mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::collections::BTreeSet;

use roost_coord::events::bus_messages::{LastActivityUpdate, WorkerRoutableSet};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::{FirehoseFrame, SyncDomain, SyncDomainReadyCommand};

use sync_seed_support::{
    closed_message, closed_of, insert_closed_row, insert_session, insert_worker,
};
use sync_ws_socket_support::{
    EXPECT, QUIET, SyncFixture, domain_ready, generation_of, next_firehose, read_subscribed,
    send_client_frame,
};

const WORKER: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const SESSION: &str = "0d7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c41";
const OTHER: &str = "0d7c9a52-3f4e-4d6a-9b1c-2e8f7a6d5c42";

/// An install with one session and five durable `closed` rows for it.
async fn logged_install(label: &str) -> (SyncFixture, String, String, Vec<u64>) {
    let fixture = SyncFixture::start(label).await;
    let (fingerprint, token) = fixture.enroll_browser(61).await;
    insert_worker(&fixture, WORKER).await;
    insert_session(&fixture, SESSION, WORKER).await;
    let mut ids = Vec::new();
    for ts in 1..=5 {
        ids.push(insert_closed_row(&fixture, SESSION, ts).await);
    }
    (fixture, fingerprint, token, ids)
}

fn terminal_ready(generation: u64, token: &str) -> Command {
    Command::DomainReady(Box::new(SyncDomainReadyCommand {
        domain: SyncDomain::Terminal.into(),
        generation,
        snapshot_token: Some(token.to_owned()),
        ..SyncDomainReadyCommand::default()
    }))
}

async fn expect_closed(socket: &mut ws_client_support::WsClient, event_id: u64) -> FirehoseFrame {
    let frame = next_firehose(socket, EXPECT)
        .await
        .expect("a session frame");
    assert_eq!(closed_of(&frame), Some((event_id, SESSION.to_owned())));
    frame
}

// sync-feed.ts:356-372: a v1 socket resuming from `since` gets its retained
// seed, then every durable row above `since` once, in order; a live repeat of
// a replayed row is dropped and a newer one delivered.
#[tokio::test]
async fn a_v1_reconnect_replays_above_since_once_after_its_seed() {
    let (fixture, _fingerprint, token, ids) = logged_install("v1-backfill").await;
    let mut socket = fixture
        .dial_sync(&format!("since={}", ids[1]), &token)
        .await
        .socket();
    let seed = next_firehose(&mut socket, EXPECT).await.expect("the seed");
    assert!(matches!(seed.frame, Some(Frame::WorkerRoutable(_))));
    for event_id in &ids[2..] {
        expect_closed(&mut socket, *event_id).await;
    }
    let bus = &fixture.services.buses.session_bus;
    bus.publish(closed_message(SESSION, ids[4]));
    bus.publish(closed_message(SESSION, ids[4] + 1));
    expect_closed(&mut socket, ids[4] + 1).await;
}

// sync-feed.ts:299-345: a v2 socket fixes its cutoff after the feed listens,
// replays (since, cutoff] behind the terminal fence, and neither repeats a
// replayed row nor loses a newer live one.
#[tokio::test]
async fn a_v2_reconnect_replays_the_closed_interval_behind_the_terminal_fence() {
    let (fixture, fingerprint, token, ids) = logged_install("v2-backfill").await;
    let mut socket = fixture
        .dial_sync(&format!("flow=1&sync_v=2&tab=t1&since={}", ids[1]), &token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;
    let snapshot = fixture
        .services
        .feed
        .bind_session_snapshot(
            &subscribed.socket_id,
            &fingerprint,
            BTreeSet::from([SESSION.to_owned()]),
        )
        .expect("the live socket binds");
    let generation = generation_of(&subscribed, SyncDomain::Terminal);
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(terminal_ready(generation, &snapshot)),
    )
    .await;
    for (sequence, event_id) in (1..).zip(&ids[2..]) {
        let frame = expect_closed(&mut socket, *event_id).await;
        assert_eq!(frame.delivery_seq, sequence);
    }
    let bus = &fixture.services.buses.session_bus;
    bus.publish(closed_message(SESSION, ids[3]));
    bus.publish(closed_message(SESSION, ids[4] + 1));
    let live = expect_closed(&mut socket, ids[4] + 1).await;
    assert_eq!(live.delivery_seq, 4);
}

// Contract §8.5 and sync-feed.ts:306-310: a cursor past the newest durable row
// cannot be recovered from, so the terminal domain is reset and the client
// re-hydrates instead of believing it is current.
#[tokio::test]
async fn a_cursor_ahead_of_the_log_resets_the_terminal_domain() {
    let (fixture, _fingerprint, token, ids) = logged_install("v2-ahead").await;
    let mut socket = fixture
        .dial_sync(
            &format!("flow=1&sync_v=2&tab=t1&since={}", ids[4] + 100),
            &token,
        )
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;
    let frame = next_firehose(&mut socket, EXPECT)
        .await
        .expect("a domain reset");
    let Some(Frame::DomainReset(reset)) = frame.frame else {
        panic!("expected a domain reset, got {:?}", frame.frame);
    };
    assert_eq!(reset.reason, "cursor_ahead_of_log");
    assert_eq!(reset.domain.as_known(), Some(SyncDomain::Terminal));
    assert_ne!(
        reset.generation,
        generation_of(&subscribed, SyncDomain::Terminal)
    );
}

// sync-feed-seed.ts:119-202 via sync-ws-v2-commands.ts:133: `domain_ready`
// replays the domain's retained state ahead of the live frames it buffered --
// routability as one chunked snapshot, the terminal as the title, activity
// and viewers of the sessions the snapshot token admitted and no others.
#[tokio::test]
async fn domain_ready_replays_retained_state_ahead_of_the_buffered_live_segment() {
    let fixture = SyncFixture::start("domain-seed").await;
    let (fingerprint, token) = fixture.enroll_browser(62).await;
    insert_worker(&fixture, WORKER).await;
    insert_session(&fixture, SESSION, WORKER).await;
    insert_session(&fixture, OTHER, WORKER).await;
    let services = &fixture.services;
    for session_id in [SESSION, OTHER] {
        services.feed.last_activity().observe(session_id, 1_000);
        services
            .titles
            .observe_title(&services.buses, session_id, "vim");
    }
    let mut socket = fixture
        .dial_sync("flow=1&sync_v=2&tab=t1", &token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;

    services
        .buses
        .worker_routable_bus
        .publish(WorkerRoutableSet { fps: Vec::new() });
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "workers is fenced"
    );
    let workers = generation_of(&subscribed, SyncDomain::Workers);
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(domain_ready(SyncDomain::Workers, workers)),
    )
    .await;
    let retained = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the routable seed");
    let Some(Frame::WorkerRoutable(chunk)) = retained.frame else {
        panic!("expected the retained routable chunk first");
    };
    assert!(!chunk.snapshot_id.is_empty());
    assert_eq!((chunk.chunk_index, chunk.chunk_count), (0, 1));
    let live = next_firehose(&mut socket, EXPECT)
        .await
        .expect("the buffered live set");
    let Some(Frame::WorkerRoutable(live)) = live.frame else {
        panic!("expected the buffered live routable set");
    };
    assert!(
        live.snapshot_id.is_empty(),
        "a live set is a full replacement"
    );

    let snapshot = services
        .feed
        .bind_session_snapshot(
            &subscribed.socket_id,
            &fingerprint,
            BTreeSet::from([SESSION.to_owned()]),
        )
        .expect("the live socket binds");
    services
        .buses
        .last_activity_bus
        .publish(LastActivityUpdate {
            session_id: SESSION.to_owned(),
            ts_ms: 2_000,
        });
    let terminal = generation_of(&subscribed, SyncDomain::Terminal);
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        Some(2),
        Some(terminal_ready(terminal, &snapshot)),
    )
    .await;
    let mut seen = Vec::new();
    for _ in 0..3 {
        let frame = next_firehose(&mut socket, EXPECT)
            .await
            .expect("terminal frames");
        seen.push(match frame.frame {
            Some(Frame::TerminalTitle(title)) => {
                (title.session_id, format!("title {}", title.title))
            }
            Some(Frame::LastActivity(activity)) => {
                (activity.session_id, format!("activity {}", activity.ts_ms))
            }
            other => panic!("unexpected terminal frame {other:?}"),
        });
    }
    let session = SESSION.to_owned();
    assert_eq!(
        seen,
        vec![
            (session.clone(), "title vim".to_owned()),
            (session.clone(), "activity 1000".to_owned()),
            (session, "activity 2000".to_owned()),
        ],
        "retained title and activity precede the live activity; OTHER was not admitted"
    );
    assert!(next_firehose(&mut socket, QUIET).await.is_none());
}
