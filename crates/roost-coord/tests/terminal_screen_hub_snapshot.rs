//! Snapshot sources and the first-byte watchdog: a source is planned only when
//! a socket asks, once per cache version, and a leased predecessor never
//! changes under its cursor; a stream whose baseline never arrives climbs the
//! same repair ladder a lost baseline does.
//!
//! Ports `apps/coord/tests/terminal/screen/terminal-screen-hub-snapshot.test.ts`
//! and `terminal-screen-frames.test.ts`. v2 defers frame production from source
//! creation to the first cursor; the Rust hub plans at the first seed demand
//! instead, so the frames test's observable half -- one source per cache
//! version, however many sockets it seeds -- is asserted here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;

use std::sync::{Arc, Mutex};

use roost_coord::sync_ws::retained_frame::SharedCellFrame;
use roost_coord::sync_ws::terminal::TerminalDeltaOutcome;
use roost_coord::sync_ws::terminal::snapshot::{TerminalSnapshotCursor, TerminalSnapshotSource};
use roost_coord::terminal_screen::hub_contract::TerminalScreenSocketSink;
use roost_coord::terminal_screen::snapshot_controller::TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
use roost_proto::{FirehoseFrame, PbCellGridFrame};
use roost_protocol::cell::CELL_GRID_CHUNK_STALL_MS;
use roost_protocol::wire::SessionId;
use terminal_screen_hub_support::{
    OTHER_STREAM, SESSION, SNAPSHOT_A, STREAM, baseline, delta, harness, request, row_chunks,
    session, texts,
};

const FIRST_BYTE: u64 = TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;

/// A socket that keeps the sources it is handed without opening them.
#[derive(Default)]
struct DeferredSnapshotSink {
    sources: Mutex<Vec<Arc<dyn TerminalSnapshotSource>>>,
}

impl DeferredSnapshotSink {
    fn source(&self, index: usize) -> Arc<dyn TerminalSnapshotSource> {
        Arc::clone(&self.sources.lock().unwrap()[index])
    }

    fn count(&self) -> usize {
        self.sources.lock().unwrap().len()
    }
}

impl TerminalScreenSocketSink for DeferredSnapshotSink {
    fn begin_terminal_stream(&self, _: &SessionId, _: &str) -> bool {
        true
    }

    fn replace_terminal_snapshot(
        &self,
        _: &SessionId,
        _: &str,
        source: Arc<dyn TerminalSnapshotSource>,
    ) -> bool {
        self.sources.lock().unwrap().push(source);
        true
    }

    fn enqueue_terminal_delta(
        &self,
        _: &SessionId,
        _: &str,
        _: &FirehoseFrame,
    ) -> TerminalDeltaOutcome {
        TerminalDeltaOutcome::Queued
    }

    fn drop_terminal_session(&self, _: &SessionId) {}
}

fn same_source(
    left: &Arc<dyn TerminalSnapshotSource>,
    right: &Arc<dyn TerminalSnapshotSource>,
) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(left), Arc::as_ptr(right))
}

fn whole_frame(cursor: &Arc<dyn TerminalSnapshotCursor>) -> PbCellGridFrame {
    assert_eq!(cursor.part_count(), 1);
    match cursor.materialize(0).expect("the part materializes") {
        SharedCellFrame::Full(frame) => frame,
        SharedCellFrame::Chunk(_) => panic!("expected an unchunked snapshot"),
    }
}

fn deferred(
    h: &terminal_screen_hub_support::Harness,
    socket_id: &str,
) -> Arc<DeferredSnapshotSink> {
    let sink = Arc::new(DeferredSnapshotSink::default());
    h.hub.register_socket(
        socket_id,
        Arc::clone(&sink) as Arc<dyn TerminalScreenSocketSink>,
    );
    h.hub.set_watching(socket_id, &session(), true);
    sink
}

// v2 terminal-screen-hub-snapshot.test.ts "defers accepted full and delta
// snapshots until cursor demand", and terminal-screen-frames.test.ts "defers
// snapshot frame production until a cursor is created".
#[test]
fn a_source_is_planned_on_demand_and_shared_by_every_seed_of_one_version() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &["first", "second"]));
    h.frame(delta(1, "updated"));

    let sink = deferred(&h, "demand");
    assert_eq!(sink.count(), 0, "watching alone plans nothing");
    assert!(h.hub.seed_socket("demand", &session()));
    assert!(h.hub.seed_socket("demand", &session()));
    assert_eq!(sink.count(), 2);
    assert!(
        same_source(&sink.source(0), &sink.source(1)),
        "one cache version, one source"
    );

    let first = sink
        .source(0)
        .create_cursor(SNAPSHOT_A)
        .expect("a leased cursor");
    let second = sink
        .source(0)
        .create_cursor(SNAPSHOT_A)
        .expect("a second cursor on the same plan");
    let encoded = whole_frame(&first);
    assert_eq!((encoded.session_id.as_str(), encoded.seq), (SESSION, 2));
    assert_eq!(texts(&encoded), ["first", "updated"]);
    assert_eq!(whole_frame(&second), encoded);
}

// v2 "keeps a leased predecessor cursor immutable after the canonical cache
// advances".
#[test]
fn a_leased_predecessor_cursor_is_unchanged_after_the_cache_advances() {
    let h = harness();
    let predecessor = deferred(&h, "predecessor");
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &["old-a", "old-b"]));
    let predecessor_cursor = predecessor
        .source(0)
        .create_cursor(SNAPSHOT_A)
        .expect("a leased cursor");

    h.frame(delta(1, "new-b"));
    assert_eq!(h.replica(), Some((2, true)));
    let successor = deferred(&h, "successor");
    assert_eq!(successor.count(), 0);
    assert!(h.hub.seed_socket("successor", &session()));
    assert!(!same_source(&successor.source(0), &predecessor.source(0)));
    let successor_cursor = successor
        .source(0)
        .create_cursor(SNAPSHOT_A)
        .expect("a leased cursor");

    let old = whole_frame(&predecessor_cursor);
    assert_eq!(
        (old.seq, texts(&old)),
        (1, vec!["old-a".to_owned(), "old-b".to_owned()])
    );
    let new = whole_frame(&successor_cursor);
    assert_eq!(
        (new.seq, texts(&new)),
        (2, vec!["old-a".to_owned(), "new-b".to_owned()])
    );
}

// v2 "escalates a minted stream whose baseline never arrives".
#[test]
fn a_stream_whose_baseline_never_arrives_climbs_the_repair_ladder() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    assert!(h.requests().is_empty());
    let armed = h.timers.armed();
    assert_eq!(armed.len(), 1);
    assert_eq!(armed[0].1, FIRST_BYTE);

    h.advance(FIRST_BYTE);
    h.timers.fire(armed[0].0);
    assert_eq!(h.requests(), [request(STREAM)]);

    let (first_attempt, _) = h.timers.newest();
    h.advance(FIRST_BYTE);
    h.timers.fire(first_attempt);
    assert_eq!(h.requests().len(), 2);
    assert!(h.fresh_streams().is_empty());

    let (second_attempt, _) = h.timers.newest();
    h.advance(FIRST_BYTE);
    h.timers.fire(second_attempt);
    assert_eq!(h.requests().len(), 2);
    let fresh = h.fresh_streams();
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].1, STREAM);
    assert!(fresh[0].2.contains("timed out"), "{}", fresh[0].2);
    assert!(
        h.timers.armed().is_empty(),
        "the ladder ends with no deadline left"
    );
}

// v2 "asks for nothing when the baseline lands before the deadline".
#[test]
fn a_baseline_that_lands_before_the_deadline_asks_for_nothing() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    assert_eq!(h.timers.armed().len(), 1);

    h.frame(baseline(1, &[]));
    assert_eq!(h.replica(), Some((1, true)));
    h.advance(FIRST_BYTE * 4);
    h.timers.fire_all();
    h.frame(delta(1, "live"));
    assert_eq!(h.replica(), Some((2, true)));
    assert!(h.requests().is_empty());
    assert!(h.fresh_streams().is_empty());
}

// v2 "a re-minted stream deadline replaces the superseded one".
#[test]
fn a_reminted_stream_deadline_replaces_the_superseded_one() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    let (stale, _) = h.timers.newest();
    h.hub.expect_stream(&session(), OTHER_STREAM, 8, 2);
    let (fresh, _) = h.timers.newest();
    assert_ne!(fresh, stale);

    h.advance(FIRST_BYTE);
    h.timers.fire(stale);
    assert!(
        h.requests().is_empty(),
        "the superseded stream's deadline does nothing"
    );
    h.timers.fire(fresh);
    assert_eq!(h.requests(), [request(OTHER_STREAM)]);
}

// v2 "leaves a chunked baseline mid-transfer to the chunk stall deadline".
#[test]
fn a_baseline_mid_transfer_is_left_to_the_chunk_stall_deadline() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    let (watchdog, _) = h.timers.newest();
    let parts = row_chunks(&baseline(1, &[]), SNAPSHOT_A);

    h.advance(FIRST_BYTE / 2);
    h.chunk(&parts[0], 0);
    h.advance(FIRST_BYTE - FIRST_BYTE / 2);
    h.timers.fire(watchdog);
    assert!(h.requests().is_empty());
    let armed = h.timers.armed();
    assert_eq!(armed.len(), 1, "only the stall deadline remains");
    assert_ne!(armed[0].0, watchdog);
    assert_eq!(armed[0].1, CELL_GRID_CHUNK_STALL_MS);

    h.chunk(&parts[1], 0);
    assert_eq!(h.replica(), Some((1, true)));
    assert!(h.requests().is_empty());
}
