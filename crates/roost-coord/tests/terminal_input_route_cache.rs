//! The keystroke path's route lookup answers from the byte hub's route cache
//! before the database, and a committed `closed` event takes the session out
//! of that cache so the next lookup falls through to the (absent) row.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use roost_coord::db::CoordDb;
use roost_coord::events::append::LiveEffects;
use roost_coord::terminal_input::control_lane::resolve_session_route;
use roost_coord::terminal_screen::byte_hub::ByteHub;
use roost_coord::terminal_screen::live_effects::{OrphanPtyKill, TerminalLiveEffects};
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, WorkerFp};

struct NoKills;

impl OrphanPtyKill for NoKills {
    fn kill(&self, _worker_fp: &WorkerFp, _session_id: &str) {}
}

const SESSION: &str = "00000000-0000-4000-8000-000000000031";

/// A migrated database whose `sessions` table holds no row at all.
async fn empty_database(label: &str) -> CoordDb {
    let root = std::env::temp_dir().join(format!("roost-route-cache-{label}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    roost_coord::db::open(&root.join("coord.db"))
        .await
        .expect("a migrated database")
}

fn primed_hub() -> (Arc<ByteHub>, WorkerFp, ChannelId) {
    let hub = Arc::new(ByteHub::with_defaults());
    let worker = WorkerFp::try_from("b".repeat(64)).expect("a 64-hex fingerprint");
    let channel = ChannelId::try_from(7).expect("a real channel id");
    let session = SessionId::try_from(SESSION).expect("a well-formed session id");
    hub.prime_channel_map(&[(session, worker.clone(), channel)]);
    (hub, worker, channel)
}

#[tokio::test]
async fn a_cached_route_resolves_without_a_session_row() {
    let database = empty_database("hit").await;
    let (hub, worker, channel) = primed_hub();

    let route = resolve_session_route(&database, &hub, SESSION)
        .await
        .expect("the lookup does not fail")
        .expect("the cached route answers before the database");
    assert_eq!(route.worker_fp, worker);
    assert_eq!(route.channel_id, channel);
}

#[tokio::test]
async fn a_closed_session_falls_through_to_the_database() {
    let database = empty_database("closed").await;
    let (hub, _worker, _channel) = primed_hub();
    let effects = TerminalLiveEffects::new(Arc::clone(&hub), Arc::new(NoKills));

    effects.index_durable_channel(
        &SessionEvent::Closed {
            session_id: SessionId::try_from(SESSION).expect("a well-formed session id"),
            exit_code: Some(0),
            ts: 0,
            trace_id: None,
        },
        None,
    );

    let route = resolve_session_route(&database, &hub, SESSION)
        .await
        .expect("the lookup does not fail");
    assert!(
        route.is_none(),
        "a closed session must not keep resolving from the cache"
    );
}
