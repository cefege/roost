//! The screen hub's canonical replica: fulls and deltas folded once, a socket
//! that refuses a delta seeded from the fold, history kept on live deltas but
//! never in a recovery full, and one repair latched per failure.
//!
//! Ports the canonical-cache cases of
//! `apps/coord/tests/terminal/screen/terminal-screen-hub.test.ts`; its two
//! watcher-lifecycle cases are `terminal_screen_hub_lifecycle.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;

use roost_coord::sync_ws::terminal::TerminalDeltaOutcome;
use terminal_screen_hub_support::{
    EPOCH, OTHER_STREAM, SESSION, SNAPSHOT_A, SNAPSHOT_B, STREAM, TestSink, baseline, delta,
    delta_frame, full_frame, harness, request, row, row_chunks, session, texts, watch,
};

// v2 "folds deltas once and falls back to the folded baseline when a socket
// cursor rejects".
#[test]
fn a_delta_is_folded_once_and_a_socket_that_refuses_it_is_seeded_from_the_fold() {
    let h = harness();
    let incremental = TestSink::queuing();
    let needs_baseline = TestSink::new(TerminalDeltaOutcome::NeedsSnapshot);
    let handled = TestSink::new(TerminalDeltaOutcome::Handled);
    watch(&h.hub, &incremental, "incremental");
    watch(&h.hub, &needs_baseline, "needs-baseline");
    watch(&h.hub, &handled, "handled");
    h.hub.expect_stream(&session(), STREAM, 8, 2);

    h.frame(baseline(1, &["old-a", "old-b"]));
    for sink in [&incremental, &needs_baseline, &handled] {
        assert_eq!(sink.snapshots().len(), 1);
    }

    h.frame(delta(1, "new-b"));
    assert_eq!(
        (
            incremental.deltas.lock().unwrap().len(),
            incremental.snapshots().len()
        ),
        (1, 1)
    );
    assert_eq!(
        (
            needs_baseline.deltas.lock().unwrap().len(),
            needs_baseline.snapshots().len()
        ),
        (1, 2),
        "a socket that dropped the delta is seeded from the fold it produced"
    );
    assert_eq!(
        (
            handled.deltas.lock().unwrap().len(),
            handled.snapshots().len()
        ),
        (1, 1),
        "a socket that handled the drop itself is owed nothing"
    );

    let folded = needs_baseline.last_seeded();
    assert_eq!(
        (
            folded.session_id.as_str(),
            folded.stream_id.as_str(),
            folded.grid_epoch.as_str()
        ),
        (SESSION, STREAM, EPOCH)
    );
    assert_eq!((folded.full, folded.seq, folded.base_seq), (true, 2, 0));
    assert_eq!(
        (folded.cursor_row, folded.cursor_col, folded.cursor_visible),
        (1, 2, false)
    );
    assert_eq!(
        (
            folded.cursor_keys_app,
            folded.bracketed_paste,
            folded.mouse_tracking
        ),
        (true, true, 1000)
    );
    assert_eq!((folded.mouse_sgr, folded.focus_events), (true, true));
    assert_eq!(texts(&folded), ["old-a", "new-b"]);
    assert_eq!((folded.cols, folded.rows), (8, 2));
    assert_eq!(h.replica(), Some((2, true)));
    assert_eq!(
        h.hub.expected_stream_id(&session()).as_deref(),
        Some(STREAM)
    );

    let late = TestSink::queuing();
    watch(&h.hub, &late, "late");
    assert_eq!(late.begins(), [(SESSION.to_owned(), STREAM.to_owned())]);
    assert!(late.snapshots().is_empty(), "watching alone does not seed");
    assert!(h.hub.seed_socket("late", &session()));
    assert_eq!(texts(&late.last_seeded()), ["old-a", "new-b"]);
}

// v2 "validates legacy full history before storing a viewport-only cache".
#[test]
fn a_legacy_history_full_is_validated_then_stored_viewport_only() {
    let h = harness();
    let sink = TestSink::queuing();
    watch(&h.hub, &sink, "socket-a");
    h.hub.expect_stream(&session(), STREAM, 8, 2);

    let mut malformed = baseline(1, &[]);
    malformed.scrollback_total = 1;
    malformed.sb_base = 0;
    h.frame(malformed);
    assert_eq!(h.replica(), None);
    assert_eq!(h.requests(), [request(STREAM)]);

    let mut legacy = baseline(1, &[]);
    legacy.scrollback_rows = vec![row(0, "old")];
    legacy.scrollback_total = 1;
    legacy.sb_base = 0;
    h.frame(legacy);

    let canonical = sink.last_seeded();
    assert_eq!((canonical.full, canonical.base_seq), (true, 0));
    assert_eq!((canonical.scrollback_total, canonical.sb_base), (1, 1));
    assert!(canonical.scrollback_rows.is_empty() && canonical.scrollback_append.is_empty());
}

// v2 "keeps history on live deltas while recovery snapshots stay viewport-only".
#[test]
fn a_live_delta_keeps_its_history_but_a_recovery_full_is_viewport_only() {
    let h = harness();
    let live = TestSink::queuing();
    watch(&h.hub, &live, "socket-a");
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &[]));

    let mut scrolled = delta(1, "changed");
    scrolled.scrollback_append = vec![row(0, "scrolled")];
    scrolled.scrollback_total = 1;
    h.frame(scrolled);

    let delivered = live.deltas.lock().unwrap()[0].2.clone();
    assert_eq!(
        delivered
            .scrollback_append
            .iter()
            .map(|row| row.spans[0].text.clone())
            .collect::<Vec<_>>(),
        ["scrolled"]
    );

    let late = TestSink::queuing();
    watch(&h.hub, &late, "late");
    assert!(h.hub.seed_socket("late", &session()));
    let seeded = late.last_seeded();
    assert_eq!((seeded.scrollback_total, seeded.sb_base), (1, 1));
    assert!(seeded.scrollback_rows.is_empty() && seeded.scrollback_append.is_empty());
}

// v2 "publishes activation before cells and drops hidden socket state".
#[test]
fn a_stream_begins_before_its_cells_and_an_unwatched_socket_is_dropped() {
    let h = harness();
    let sink = TestSink::queuing();
    watch(&h.hub, &sink, "socket-a");
    assert!(sink.begins().is_empty(), "nothing is expected yet");

    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &[]));
    assert_eq!(
        sink.events(),
        [format!("begin:{STREAM}"), format!("snapshot:{STREAM}")]
    );

    h.hub.set_watching("socket-a", &session(), false);
    assert_eq!(sink.drops(), [SESSION]);
    h.hub.expect_stream(&session(), OTHER_STREAM, 8, 2);
    h.frame(full_frame(OTHER_STREAM, 3, 8, 2, &[]));
    assert_eq!((sink.begins().len(), sink.snapshots().len()), (1, 1));

    h.hub.set_watching("socket-a", &session(), true);
    assert_eq!(
        sink.begins().last(),
        Some(&(SESSION.to_owned(), OTHER_STREAM.to_owned()))
    );
    assert!(h.hub.seed_socket("socket-a", &session()));
    assert_eq!(sink.last_seeded().stream_id, OTHER_STREAM);
}

// v2 "ignores stale streams and latches one repair across wrong base and epoch".
#[test]
fn a_stale_stream_is_ignored_and_a_broken_delta_run_latches_one_repair() {
    let h = harness();
    let sink = TestSink::queuing();
    watch(&h.hub, &sink, "socket-a");
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &[]));

    h.frame(delta_frame(OTHER_STREAM, 1, 1, "changed"));
    assert!(h.requests().is_empty());
    assert_eq!(h.replica(), Some((1, true)));

    let mut wrong_base = delta(9, "changed");
    wrong_base.seq = 10;
    h.frame(wrong_base);
    let mut wrong_epoch = delta(1, "changed");
    wrong_epoch.grid_epoch = "wrong-epoch".to_owned();
    h.frame(wrong_epoch);
    assert_eq!(h.requests(), [request(STREAM)], "one repair for the run");
    assert_eq!(h.replica(), Some((1, false)));

    h.frame(baseline(10, &[]));
    assert_eq!(h.replica(), Some((10, true)));
    let mut wrong_again = delta(10, "changed");
    wrong_again.grid_epoch = "wrong-again".to_owned();
    h.frame(wrong_again);
    assert_eq!(
        h.requests(),
        [request(STREAM), request(STREAM)],
        "a repaired replica latches anew"
    );
}

// v2 "keeps the old baseline visible until a complete replacement assembles".
#[test]
fn the_old_baseline_stays_served_until_a_replacement_assembles_completely() {
    let h = harness();
    let sink = TestSink::queuing();
    watch(&h.hub, &sink, "socket-a");
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &["old-0", "old-1"]));

    let replacement = baseline(2, &["new-0", "new-1"]);
    let first_attempt = row_chunks(&replacement, SNAPSHOT_A);
    h.chunk(&first_attempt[0], 0);
    assert_eq!(h.replica(), Some((1, true)));
    assert_eq!(sink.snapshots().len(), 1);

    h.chunk(&first_attempt[0], 0);
    assert_eq!(h.requests(), [request(STREAM)]);
    assert_eq!(h.hub.current_seq(&session()), Some(1));
    assert_eq!(texts(&sink.last_seeded()), ["old-0", "old-1"]);
    let late = TestSink::queuing();
    watch(&h.hub, &late, "late");
    assert!(
        h.hub.seed_socket("late", &session()),
        "the old baseline is still served"
    );
    assert_eq!(texts(&late.last_seeded()), ["old-0", "old-1"]);

    let complete = row_chunks(&replacement, SNAPSHOT_B);
    h.chunk(&complete[0], 0);
    assert_eq!(h.hub.current_seq(&session()), Some(1));
    h.chunk(&complete[1], 0);
    assert_eq!(h.replica(), Some((2, true)));
    assert_eq!(sink.snapshots().len(), 2);
    assert_eq!(texts(&sink.last_seeded()), ["new-0", "new-1"]);
    assert_eq!(texts(&late.last_seeded()), ["new-0", "new-1"]);
}
