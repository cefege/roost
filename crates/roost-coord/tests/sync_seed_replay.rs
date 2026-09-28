//! Which durable session events a resuming Sync socket is owed: the v1
//! boundary the backfill must not repeat, the live events a v2 recovery holds
//! until its interval is out, the aborts that reset the terminal domain, and
//! a replay that yields to live publishers between batches.
//!
//! Ports `apps/coord/tests/sync/sync-backfill-priority.test.ts` against
//! `sync_ws::session_replay` and `sync_ws::backfill::replay_since`; the v2
//! hold rules come from `apps/coord/src/sync/sync-feed.ts:136-165, 334-345`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::events::bus_messages::SessionBusMessage;
use roost_coord::sync_ws::backfill::replay_since;
use roost_coord::sync_ws::session_replay::{LiveVerdict, RECOVERY_HOLD_MAX_EVENTS, SessionReplay};
use roost_protocol::wire::{SessionEvent, SessionId};

const SESSION: &str = "00000000-0000-4000-8000-111111111111";

fn closed(ts: i64) -> SessionEvent {
    SessionEvent::Closed {
        session_id: SessionId::try_from(SESSION).unwrap(),
        exit_code: None,
        ts,
        trace_id: None,
    }
}

fn live(event_id: u64) -> SessionBusMessage {
    SessionBusMessage::committed(closed(1), event_id)
}

// v2 sync-backfill-priority.test.ts "recovery dedupe keeps only its cutoff
// boundary": a live event the v1 backfill overlapped goes out once; after the
// backfill the cutoff drops its repeat, and later live ids are not remembered,
// so a repeat of one of THOSE is delivered again.
#[test]
fn a_v1_backfill_delivers_its_boundary_once_and_then_stops_remembering() {
    let since = 10;
    let boundary = 11;
    let mut replay = SessionReplay::new(since, false);
    assert_eq!(replay.admit_live(&live(boundary)), LiveVerdict::Emit);
    assert!(!replay.admit_recovered(since), "the cursor itself is held");
    assert!(
        !replay.admit_recovered(boundary),
        "the live copy already went"
    );
    replay.finish_backfill();
    assert_eq!(replay.admit_live(&live(boundary)), LiveVerdict::Duplicate);

    let first = boundary + 1;
    for event_id in first..first + 2_048 {
        assert_eq!(replay.admit_live(&live(event_id)), LiveVerdict::Emit);
    }
    assert_eq!(
        replay.admit_live(&live(first + 2_047)),
        LiveVerdict::Emit,
        "the boundary set stopped growing with the backfill"
    );
}

// sync-feed.ts:136-165 and :334-345: a v2 recovery holds live events, replays
// the closed interval up to its cutoff, then releases only the held events
// above the cutoff in id order, remembering them so a repeat is dropped.
#[test]
fn a_v2_recovery_holds_live_events_and_releases_only_the_tail() {
    let mut replay = SessionReplay::new(5, true);
    for event_id in [9, 7, 12, 3] {
        assert_eq!(replay.admit_live(&live(event_id)), LiveVerdict::Held);
    }
    assert!(!replay.admit_recovered(5));
    for event_id in 6..=8 {
        assert!(replay.admit_recovered(event_id));
    }
    let tail: Vec<Option<u64>> = replay
        .finish_recovery(8)
        .into_iter()
        .map(|message| message.event_id)
        .collect();
    assert_eq!(tail, vec![Some(9), Some(12)]);
    assert_eq!(replay.admit_live(&live(12)), LiveVerdict::Duplicate);
    assert_eq!(replay.admit_live(&live(8)), LiveVerdict::Duplicate);
    assert_eq!(replay.admit_live(&live(13)), LiveVerdict::Emit);
}

// sync-feed.ts:138-145: an unstamped live event cannot be ordered against the
// log, so the recovery is abandoned (terminal reset) and the event is not sent.
#[test]
fn an_unstamped_live_event_abandons_a_v2_recovery() {
    let mut replay = SessionReplay::new(5, true);
    let unstamped = SessionBusMessage {
        event: closed(1),
        event_id: None,
    };
    assert_eq!(
        replay.admit_live(&unstamped),
        LiveVerdict::Abort {
            reason: "unstamped_session_event",
            emit: false
        }
    );
    assert!(replay.is_aborted());
    assert_eq!(replay.admit_live(&live(6)), LiveVerdict::Emit);
}

// sync-feed.ts:146-162: the hold is bounded at 512 distinct events and 4 MiB;
// the event that overflows it abandons the recovery and goes out live.
#[test]
fn an_overflowing_hold_abandons_the_recovery_and_sends_the_event_live() {
    let mut replay = SessionReplay::new(5, true);
    let first = 6;
    let limit = u64::try_from(RECOVERY_HOLD_MAX_EVENTS).unwrap();
    for event_id in first..first + limit {
        assert_eq!(replay.admit_live(&live(event_id)), LiveVerdict::Held);
    }
    assert_eq!(
        replay.admit_live(&live(first)),
        LiveVerdict::Held,
        "a repeat replaces its held copy instead of counting twice"
    );
    assert_eq!(
        replay.admit_live(&live(first + limit)),
        LiveVerdict::Abort {
            reason: "recovery_live_overflow",
            emit: true
        }
    );
    assert!(replay.is_aborted());

    let mut bytes = SessionReplay::new(5, true);
    let huge = SessionBusMessage::committed(
        SessionEvent::Cwd {
            session_id: SessionId::try_from(SESSION).unwrap(),
            cwd: "x".repeat(4 * 1024 * 1024),
            ts: 1,
            trace_id: None,
        },
        6,
    );
    assert!(matches!(
        bytes.admit_live(&huge),
        LiveVerdict::Abort {
            reason: "recovery_live_overflow",
            emit: true
        }
    ));
}

// v2 sync-backfill-priority.test.ts "a queued live session event lands between
// sixteen-event replay batches": the replay yields after every batch, so a
// publisher scheduled on the same runtime runs between the first sixteen and
// the next.
#[tokio::test]
async fn a_live_publisher_runs_between_sixteen_event_replay_batches() {
    let root =
        std::env::temp_dir().join(format!("roost-sync-seed-priority-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let db = roost_coord::db::open(&root.join("coord.db")).await.unwrap();
    let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&db, 0)
        .await
        .unwrap();
    let mut since = 0;
    for ts in 0..33_i64 {
        let payload = serde_json::to_string(&closed(ts)).unwrap();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO events (kind, session_id, payload_json, ts, dashboard_id) \
             VALUES ('closed', ?, ?, ?, ?) RETURNING id",
        )
        .bind(SESSION)
        .bind(payload)
        .bind(ts)
        .bind(&tenant.dashboard_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        if ts == 0 {
            since = u64::try_from(id).unwrap();
        }
    }

    // What the publisher recorded is drained before each batch is recorded,
    // so `order` is the order the two tasks actually ran in.
    let (published, arrivals) = std::sync::mpsc::channel::<&str>();
    let mut publisher = Some(published);
    let mut order = Vec::new();
    let replayed = replay_since(db.pool(), since, |batch| {
        order.extend(arrivals.try_iter());
        order.extend(batch.iter().map(|_| "replay"));
        if let Some(published) = publisher.take() {
            tokio::spawn(async move { published.send("live").unwrap() });
        }
    })
    .await
    .unwrap();
    order.extend(arrivals.try_iter());

    assert_eq!(replayed, 32);
    let mut expected = vec!["replay"; 16];
    expected.push("live");
    expected.extend(["replay"; 16]);
    assert_eq!(order, expected);
    let _ = std::fs::remove_dir_all(&root);
}
