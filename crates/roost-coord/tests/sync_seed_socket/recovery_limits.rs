//! Socket recovery of a durable event larger than its bounded queue/window.
//! Uses the parent Sync fixture to prove reset replaces an impossible wait.
//! The reset generation must still accept hydration and later live traffic.

use std::collections::BTreeSet;

use roost_coord::sync_ws::domain_table::DOMAIN_MAX_QUEUED_BYTES;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::SyncDomain;
use roost_protocol::wire::{SessionEvent, SessionId};

use super::sync_seed_support::{
    closed_message, dashboard, insert_closed_row, insert_session, insert_worker,
};
use super::sync_ws_socket_support::{
    EXPECT, SyncFixture, next_firehose, read_subscribed, send_client_frame,
};
use super::{SESSION, WORKER, expect_closed, terminal_ready};

#[tokio::test]
async fn an_unqueueable_replay_resets_then_accepts_hydration_and_live_events() {
    let fixture = SyncFixture::start("v2-oversized-recovery").await;
    let (fingerprint, token) = fixture.enroll_browser(65).await;
    insert_worker(&fixture, WORKER).await;
    insert_session(&fixture, SESSION, WORKER).await;
    let since = insert_closed_row(&fixture, SESSION, 1).await;
    let event = SessionEvent::Cwd {
        session_id: SessionId::try_from(SESSION).unwrap(),
        cwd: "x".repeat(DOMAIN_MAX_QUEUED_BYTES as usize),
        ts: 2,
        trace_id: None,
    };
    let oversized_id: i64 = sqlx::query_scalar(
        "INSERT INTO events (kind, session_id, payload_json, ts, dashboard_id) \
         VALUES ('cwd', $1, $2, 2, $3) RETURNING id",
    )
    .bind(SESSION)
    .bind(serde_json::to_string(&event).unwrap())
    .bind(dashboard(&fixture))
    .fetch_one(fixture.services.db.pool())
    .await
    .unwrap();
    let mut socket = fixture
        .dial_sync(&format!("flow=1&sync_v=2&tab=t1&since={since}"), &token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;
    let frame = next_firehose(&mut socket, EXPECT)
        .await
        .expect("recovery reset");
    let Some(Frame::DomainReset(reset)) = frame.frame else {
        panic!("expected recovery reset, got {:?}", frame.frame);
    };
    assert_eq!(reset.reason, "recovery_failed");
    assert_eq!(reset.domain.as_known(), Some(SyncDomain::Terminal));
    let snapshot = fixture
        .services
        .feed
        .bind_session_snapshot(
            &subscribed.socket_id,
            &fingerprint,
            BTreeSet::from([SESSION.to_owned()]),
        )
        .unwrap();
    send_client_frame(
        &mut socket,
        &subscribed.socket_id,
        None,
        Some(terminal_ready(reset.generation, &snapshot)),
    )
    .await;
    let live_id = u64::try_from(oversized_id).unwrap() + 1;
    fixture
        .services
        .buses
        .session_bus
        .publish(closed_message(SESSION, live_id));
    expect_closed(&mut socket, live_id).await;
}
