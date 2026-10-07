//! Durable recovery for a Sync socket that resumed from `since`: the events
//! above its cursor replayed from the log, in order, exactly once across the
//! seam with the live feed.
//!
//! Spawned by `sync_ws::socket_open` once the socket is open, and aborted by
//! `release_socket`. The per-event decisions are `sync_ws::session_replay`'s;
//! this file owns the reads, the pacing and the resets. Ports `backfill` of
//! `apps/coord/src/sync/sync-feed.ts` over `events::event_query`.
//!
//! v1 AND v2 RECOVER DIFFERENTLY, AS IN v2. A v1 socket replays one page above
//! `since` while live events keep flowing, and drops the ones it overlapped. A
//! v2 socket fixes the cutoff AFTER its feed is listening, replays the closed
//! interval `(since, cutoff]` in pages while live events wait, then releases
//! the held tail above the cutoff -- and any failure resets the terminal domain
//! so the client re-hydrates instead of believing a gap closed (contract §8.5).

use std::sync::Arc;

use roost_proto::SyncDomain;
use roost_protocol::wire::sync_ws::SYNC_RESET_CURSOR_AHEAD_OF_LOG;
use sqlx::AnyPool;
use tokio::sync::oneshot;

use crate::events::bus_messages::SessionBusMessage;
use crate::events::event_query::{
    EventQueryError, GET_EVENTS_SINCE_LIMIT, StoredEvent, get_event_max_id, get_events_since,
    get_events_through,
};
use crate::sync_ws::admission::EnqueueOutcome;
use crate::sync_ws::driver::{Delivery, LinkState, SyncLink, now_ms};
use crate::sync_ws::live_feed::emit_session_frame;

/// Replayed events delivered between two yields, so a live publisher on the
/// same runtime is never held behind a whole page (`sync-feed.ts:361-363`).
pub const REPLAY_BATCH_EVENTS: usize = 16;

/// One socket's recovery task; dropping it stops the recovery.
#[derive(Debug)]
pub struct BackfillTask(tokio::task::JoinHandle<()>);

impl Drop for BackfillTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start recovering `link` from `since`, after its retained seed when
/// `seeded` is given. A fresh socket (`since` of zero) recovers nothing: the
/// client's snapshot is taken after the socket subscribed.
#[must_use]
pub fn spawn_backfill(
    link: &Arc<SyncLink>,
    pool: &AnyPool,
    since: u64,
    v2: bool,
    seeded: Option<oneshot::Receiver<()>>,
) -> Option<BackfillTask> {
    if since == 0 {
        return None;
    }
    let link = Arc::clone(link);
    let pool = pool.clone();
    Some(BackfillTask(tokio::spawn(async move {
        if let Some(seeded) = seeded
            && seeded.await.is_err()
        {
            return;
        }
        tracing::info!(
            event = "sync-ws",
            action = "backfill_started",
            since,
            sync_v = if v2 { 2 } else { 1 },
            "durable recovery started"
        );
        if v2 {
            recover_through_cutoff(&link, &pool, since).await;
        } else {
            backfill_since(&link, &pool, since).await;
        }
    })))
}

/// Read the page above `since` and hand it to `emit` in batches of
/// [`REPLAY_BATCH_EVENTS`], yielding between them. Returns the rows read.
pub async fn replay_since(
    pool: &AnyPool,
    since: u64,
    mut emit: impl FnMut(&[StoredEvent]),
) -> Result<usize, EventQueryError> {
    let rows = get_events_since(pool, since, None).await?;
    for batch in rows.chunks(REPLAY_BATCH_EVENTS) {
        emit(batch);
        tokio::task::yield_now().await;
    }
    Ok(rows.len())
}

/// The v1 backfill: one page above `since`, then live events stop being
/// remembered as the boundary (`sync-feed.ts:356-372`).
async fn backfill_since(link: &SyncLink, pool: &AnyPool, since: u64) {
    let replayed = replay_since(pool, since, |batch| {
        link.deliver_with(|state| {
            for row in batch {
                deliver_recovered(state, row);
            }
            None
        });
    })
    .await;
    match replayed {
        Ok(count) if count == GET_EVENTS_SINCE_LIMIT => {
            tracing::warn!(
                event = "sync-ws",
                action = "backfill_truncated",
                since,
                returned = count,
                "the v1 backfill returned a full page; older clients re-list to recover the rest"
            );
        }
        Ok(count) => {
            tracing::info!(
                event = "sync-ws",
                action = "backfill_done",
                since,
                replayed = count,
                "the v1 backfill is delivered"
            );
        }
        Err(error) => {
            tracing::warn!(event = "sync-ws", action = "backfill_failed", since, error = %error, "the v1 backfill could not be read");
        }
    }
    link.lock().replay.finish_backfill();
}

/// The v2 recovery through a cutoff fixed after the feed is listening
/// (`sync-feed.ts:302-354`).
async fn recover_through_cutoff(link: &SyncLink, pool: &AnyPool, since: u64) {
    let cutoff = match get_event_max_id(pool).await {
        Ok(cutoff) => cutoff,
        Err(error) => return fail_recovery(link, since, &error),
    };
    if cutoff < since {
        link.deliver_with(|state| {
            state.replay.rewind_to_log_end(cutoff);
            None
        });
        return stop_recovery(link, SYNC_RESET_CURSOR_AHEAD_OF_LOG);
    }
    let mut cursor = since;
    while cursor < cutoff {
        let rows = match get_events_through(pool, cursor, cutoff, None).await {
            Ok(rows) => rows,
            Err(error) => return fail_recovery(link, since, &error),
        };
        let mut paged = false;
        link.deliver_with(|state| {
            paged = replay_page(state, &rows, &mut cursor, cutoff);
            None
        });
        if !paged {
            return;
        }
        tokio::task::yield_now().await;
    }
    let mut released = 0;
    link.deliver_with(|state| {
        if state.replay.is_aborted() {
            return None;
        }
        for message in state.replay.finish_recovery(cutoff) {
            released += 1;
            if let Some(frame) = emit_session_frame(state, &message) {
                state.deliver(frame, now_ms());
            }
        }
        None
    });
    tracing::info!(
        event = "sync-ws",
        action = "recovery_done",
        since,
        cutoff,
        held_released = released,
        "the v2 recovery reached its cutoff"
    );
}

/// Deliver one recovery page under the link, or reset for the gap or the
/// disorder it found. `false` stops the recovery.
fn replay_page(state: &mut LinkState, rows: &[StoredEvent], cursor: &mut u64, cutoff: u64) -> bool {
    if state.replay.is_aborted() {
        return false;
    }
    if rows.is_empty() {
        abort_recovery(state, "recovery_gap");
        return false;
    }
    for row in rows {
        if row.id <= *cursor || row.id > cutoff {
            abort_recovery(state, "recovery_order");
            return false;
        }
        deliver_recovered(state, row);
        *cursor = row.id;
    }
    true
}

/// Deliver one durable row unless the client already has it.
fn deliver_recovered(state: &mut LinkState, row: &StoredEvent) {
    if !state.replay.admit_recovered(row.id) {
        return;
    }
    let message = SessionBusMessage::committed(row.event.clone(), row.id);
    if let Some(frame) = emit_session_frame(state, &message) {
        state.deliver(frame, now_ms());
    }
}

/// A read failed: the recovery ends with v2's `recovery_failed` reset, which
/// v2 sends whatever the recovery's state (`sync-feed.ts:346-353`).
fn fail_recovery(link: &SyncLink, since: u64, error: &EventQueryError) {
    tracing::warn!(event = "sync-ws", action = "backfill_failed", since, error = %error, "the v2 recovery could not be read");
    link.deliver_with(|state| {
        abort_recovery(state, "recovery_failed");
        None
    });
}

/// End a recovery a live event has not already abandoned (`sync-feed.ts:305`).
fn stop_recovery(link: &SyncLink, reason: &'static str) {
    link.deliver_with(|state| {
        if !state.replay.is_aborted() {
            abort_recovery(state, reason);
        }
        None
    });
}

fn abort_recovery(state: &mut LinkState, reason: &'static str) {
    state.replay.abort();
    reset_terminal_for_recovery(state, reason);
}

/// Reset a v2 socket's terminal domain because its recovery could not close
/// the gap, so the client re-hydrates from the authoritative list
/// (`sync-ws-handler.ts:220-222`).
pub(in crate::sync_ws) fn reset_terminal_for_recovery(state: &mut LinkState, reason: &'static str) {
    let Delivery::V2(session) = &mut state.delivery else {
        return;
    };
    tracing::warn!(event = "sync-ws", action = "recovery_reset", socket_id = %state.socket_id, reason, "the terminal domain is reset because recovery cannot close the gap");
    if let EnqueueOutcome::Reset(notice) = session.reset_domain(SyncDomain::Terminal, reason) {
        state.send_control(&notice.to_frame(), now_ms());
    }
}
