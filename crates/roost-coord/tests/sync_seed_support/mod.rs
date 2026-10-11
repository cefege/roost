// The rows and principals the seed, backfill and revocation binaries need on
// top of `sync_ws_socket_support`: workers, sessions and durable events a
// socket's index and backfill read, a worker principal whose Sync socket is
// scoped to its own resources, and readers for the frames those produce.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use roost_coord::events::bus_messages::SessionBusMessage;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::session_event_proto::Kind;
use roost_proto::FirehoseFrame;
use roost_protocol::wire::{SessionEvent, SessionId};
use sqlx::AssertSqlSafe;

use super::sync_ws_socket_support::{EXPECT, SyncFixture, next_firehose, send_client_frame};
use super::ws_client_support::WsClient;
use super::ws_credential_support::{mint_coordinator_jwt, now_secs};

/// Run one statement against the fixture's database.
pub async fn exec(fixture: &SyncFixture, sql: &str) {
    sqlx::query(AssertSqlSafe(sql.to_owned()))
        .execute(fixture.services.db.pool())
        .await
        .expect("the statement applies");
}

/// The self-hosted dashboard every scoped row belongs to.
pub fn dashboard(fixture: &SyncFixture) -> String {
    let tenant = fixture.services.boot.tenant.as_ref().expect("a tenant");
    tenant.dashboard_id.clone()
}

/// A live worker row with no key: a machine a browser index lists.
pub async fn insert_worker(fixture: &SyncFixture, fp: &str) {
    let dashboard = dashboard(fixture);
    exec(
        fixture,
        &format!(
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES ('{fp}', 'w', 'linux', 1000, 1000, '{dashboard}')"
        ),
    )
    .await;
}

/// A worker principal for the key derived from `seed`: its key row and its
/// machine row, and a fresh credential. Its Sync socket is read-only and
/// scoped to its own resources.
pub async fn enroll_worker(fixture: &SyncFixture, seed: u8) -> (String, String) {
    let now = now_secs();
    let (fingerprint, public_key, token) = mint_coordinator_jwt([seed; 32], now, now + 300);
    super::db_support::insert_authorized_key(
        &fixture.services.db,
        &fingerprint,
        &public_key,
        "worker",
        None,
    )
    .await;
    insert_worker(fixture, &fingerprint).await;
    (fingerprint, token)
}

/// An open session row owned by `worker_fp`.
pub async fn insert_session(fixture: &SyncFixture, id: &str, worker_fp: &str) {
    let dashboard = dashboard(fixture);
    exec(
        fixture,
        &format!(
            "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, status, created_at, \
             dashboard_id) VALUES ('{id}', '{worker_fp}', 1, 'shell', '/', 'open', 1000, \
             '{dashboard}')"
        ),
    )
    .await;
}

/// `count` open sessions owned by `worker_fp`, in one statement, and their ids.
pub async fn insert_sessions(fixture: &SyncFixture, count: usize, worker_fp: &str) -> Vec<String> {
    let ids: Vec<String> = (0..count)
        .map(|index| format!("00000000-0000-4000-8000-{index:012}"))
        .collect();
    let dashboard = dashboard(fixture);
    let rows: Vec<String> = ids
        .iter()
        .map(|id| format!("('{id}', '{worker_fp}', 1, 'shell', '/', 'open', 1000, '{dashboard}')"))
        .collect();
    exec(
        fixture,
        &format!(
            "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, status, created_at, \
             dashboard_id) VALUES {}",
            rows.join(", ")
        ),
    )
    .await;
    ids
}

/// A `closed` event for `session_id`.
pub fn closed_event(session_id: &str, ts: i64) -> SessionEvent {
    SessionEvent::Closed {
        session_id: SessionId::try_from(session_id).unwrap(),
        exit_code: None,
        ts,
        trace_id: None,
    }
}

/// Commit one durable `closed` row and return its id.
pub async fn insert_closed_row(fixture: &SyncFixture, session_id: &str, ts: i64) -> u64 {
    let payload = serde_json::to_string(&closed_event(session_id, ts)).unwrap();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO events (kind, session_id, payload_json, ts, dashboard_id) \
         VALUES ('closed', $1, $2, $3, $4) RETURNING id",
    )
    .bind(session_id)
    .bind(payload)
    .bind(ts)
    .bind(dashboard(fixture))
    .fetch_one(fixture.services.db.pool())
    .await
    .expect("the event row");
    u64::try_from(id).unwrap()
}

/// `count` durable `closed` rows for `session_id`, stamped 1..=count, and their
/// ids in commit order.
pub async fn insert_closed_rows(fixture: &SyncFixture, session_id: &str, count: usize) -> Vec<u64> {
    let mut ids = Vec::with_capacity(count);
    for ts in 1..=count {
        let ts = i64::try_from(ts).unwrap();
        ids.push(insert_closed_row(fixture, session_id, ts).await);
    }
    ids
}

/// A live `closed` message stamped with `event_id`.
pub fn closed_message(session_id: &str, event_id: u64) -> SessionBusMessage {
    SessionBusMessage::committed(closed_event(session_id, 1), event_id)
}

/// The durable id and session of a `closed` session frame.
pub fn closed_of(frame: &FirehoseFrame) -> Option<(u64, String)> {
    match &frame.frame {
        Some(Frame::SessionEvent(event)) => match &event.kind {
            Some(Kind::Closed(closed)) => Some((event.event_id, closed.session_id.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Read one replayed run of `closed` frames from `socket`: each must be the
/// expected durable row at its sequence, and the client acknowledges every
/// `ack_every` frames when asked to.
pub async fn read_closed_frames(
    socket: &mut WsClient,
    socket_id: &str,
    session_id: &str,
    expected_ids: &[u64],
    first_sequence: u64,
    ack_every: Option<usize>,
) {
    for (offset, event_id) in expected_ids.iter().enumerate() {
        let frame = next_firehose(socket, EXPECT)
            .await
            .expect("a session frame");
        let sequence = first_sequence + offset as u64;
        assert_eq!(closed_of(&frame), Some((*event_id, session_id.to_owned())));
        assert_eq!(frame.delivery_seq, sequence);
        if ack_every.is_some_and(|every| (offset + 1).is_multiple_of(every)) {
            send_client_frame(socket, socket_id, Some(sequence), None).await;
        }
    }
}
