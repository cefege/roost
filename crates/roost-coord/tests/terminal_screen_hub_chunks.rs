//! Chunked baselines and the repair ladder around them: a bad chunk latches one
//! request, a stalled transfer re-requests at the exact deadline, two silent
//! requests escalate to a fresh stream, and a landed full or a new stream
//! leaves every older deadline with nothing to do.
//!
//! Ports the "bounded chunk assembly" cases of
//! `apps/coord/tests/terminal/screen/terminal-screen-hub-chunks.test.ts`; its
//! delta-hold cases are `terminal_screen_hub_hold.rs`. A deadline is never
//! cancelled here, so v2's "the timer was cleared" is asserted as "firing it
//! does nothing".
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;

use roost_coord::terminal_screen::snapshot_controller::TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
use roost_proto::PbCellGridChunk;
use roost_proto::buffa::MessageField;
use roost_protocol::cell::{
    CELL_GRID_CHUNK_STALL_MS, CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_CHUNKS,
};
use terminal_screen_hub_support::{
    OTHER_STREAM, SNAPSHOT_A, STREAM, baseline, chunks, delta, full_frame, harness, request,
    row_chunks, session,
};

const FIRST_BYTE: u64 = TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;

// v2 "latches out-of-order, missing-row, and chunk-count failures once".
#[test]
fn a_malformed_chunk_run_latches_exactly_one_request_and_installs_nothing() {
    let source = baseline(1, &[]);
    let out_of_order = row_chunks(&source, SNAPSHOT_A).remove(1);
    let missing_row = chunks(
        &source,
        &[vec![source.viewport_rows[0].clone()]],
        SNAPSHOT_A,
    )
    .remove(0);
    let mut part = source.clone();
    part.viewport_rows = vec![source.viewport_rows[0].clone()];
    let over_cap = PbCellGridChunk {
        snapshot_id: SNAPSHOT_A.to_owned(),
        chunk_index: 0,
        chunk_count: CELL_GRID_SNAPSHOT_MAX_CHUNKS + 1,
        part: MessageField::some(part),
        ..Default::default()
    };
    for (case, chunk) in [
        ("out of order", out_of_order),
        ("missing row", missing_row),
        ("over cap", over_cap),
    ] {
        let h = harness();
        h.hub.expect_stream(&session(), STREAM, 8, 2);
        h.chunk(&chunk, 0);
        h.chunk(&chunk, 0);
        assert_eq!(h.requests(), [request(STREAM)], "{case}");
        assert_eq!(h.replica(), None, "{case}");
    }
}

// v2 "rejects an oversized unchunked baseline without exposing a cache".
#[test]
fn an_unchunked_full_larger_than_one_part_is_refused() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 1, 1);
    let oversized = "x".repeat(CELL_GRID_PART_MAX_BYTES as usize);
    h.frame(full_frame(STREAM, 1, 1, 1, &[oversized.as_str()]));
    assert_eq!(h.requests(), [request(STREAM)]);
    assert_eq!(h.replica(), None);
}

// v2 "redrives a latched request when a partial stalls at the exact boundary".
#[test]
fn a_partial_that_stalls_at_the_exact_boundary_redrives_the_request() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(delta(1, "changed"));
    assert_eq!(h.requests(), [request(STREAM)]);
    let (request_deadline, delay) = h.timers.newest();
    assert_eq!(delay, FIRST_BYTE);
    let partial = row_chunks(&baseline(1, &[]), SNAPSHOT_A);

    h.chunk(&partial[0], 0);
    assert_eq!(h.replica(), None);
    let (stall_deadline, delay) = h.timers.newest();
    assert_eq!(delay, CELL_GRID_CHUNK_STALL_MS);
    assert_ne!(stall_deadline, request_deadline);
    h.timers.fire(request_deadline);
    assert_eq!(
        h.requests(),
        [request(STREAM)],
        "the first chunk retired the request deadline"
    );

    h.advance(CELL_GRID_CHUNK_STALL_MS);
    h.timers.fire(stall_deadline);
    assert_eq!(h.requests(), [request(STREAM), request(STREAM)]);
    assert_eq!(h.replica(), None);

    h.chunk(&partial[1], 0);
    assert_eq!(
        h.requests().len(),
        2,
        "the stalled half does not open a third request"
    );
    assert_eq!(h.replica(), None);
}

// v2 "retries no-first-byte repair once then requests a fresh stream".
#[test]
fn two_silent_snapshot_requests_escalate_to_a_fresh_stream() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(delta(1, "changed"));
    assert_eq!(h.requests(), [request(STREAM)]);

    let (first, _) = h.timers.newest();
    h.advance(FIRST_BYTE);
    h.timers.fire(first);
    assert_eq!(h.requests(), [request(STREAM), request(STREAM)]);
    assert!(h.fresh_streams().is_empty());

    let (second, _) = h.timers.newest();
    assert_ne!(second, first);
    h.advance(FIRST_BYTE);
    h.timers.fire(second);
    assert_eq!(h.requests().len(), 2);
    let fresh = h.fresh_streams();
    assert_eq!(fresh.len(), 1);
    assert_eq!(
        (fresh[0].0.as_str(), fresh[0].1.as_str()),
        (request(STREAM).0.as_str(), STREAM)
    );
    assert!(fresh[0].2.contains("timed out"), "{}", fresh[0].2);

    h.timers.fire_all();
    assert_eq!(
        (h.requests().len(), h.fresh_streams().len()),
        (2, 1),
        "no deadline outlives the ladder"
    );
}

// v2 "valid full and stream supersession cancel request-time repair timers".
#[test]
fn a_landed_full_or_a_new_stream_leaves_the_request_deadline_nothing_to_do() {
    let full = harness();
    full.hub.expect_stream(&session(), STREAM, 8, 2);
    full.frame(delta(1, "changed"));
    full.frame(baseline(1, &[]));
    assert_eq!(full.replica(), Some((1, true)));
    full.timers.fire_all();
    assert_eq!(full.requests(), [request(STREAM)]);
    assert!(full.fresh_streams().is_empty());

    let superseded = harness();
    superseded.hub.expect_stream(&session(), STREAM, 8, 2);
    superseded.frame(delta(1, "changed"));
    let before: Vec<u64> = superseded
        .timers
        .armed()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    superseded.hub.expect_stream(&session(), OTHER_STREAM, 8, 2);
    for id in before {
        superseded.timers.fire(id);
    }
    assert_eq!(superseded.requests(), [request(STREAM)]);
    assert!(superseded.fresh_streams().is_empty());
}

// v2 "post-first-byte chunk failure starts a new bounded request".
#[test]
fn a_chunk_failure_after_the_first_byte_starts_a_new_bounded_request() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(delta(1, "changed"));
    let partial = row_chunks(&baseline(1, &[]), SNAPSHOT_A);
    h.chunk(&partial[0], 0);
    assert_eq!(h.requests(), [request(STREAM)]);
    let (stall_deadline, _) = h.timers.newest();

    h.chunk(&partial[0], 0);
    assert_eq!(h.requests(), [request(STREAM), request(STREAM)]);
    let (request_deadline, delay) = h.timers.newest();
    assert_eq!(delay, FIRST_BYTE);
    h.advance(CELL_GRID_CHUNK_STALL_MS);
    h.timers.fire(stall_deadline);
    assert_eq!(
        h.requests().len(),
        2,
        "the failed transfer's stall deadline is retired"
    );
    h.timers.fire(request_deadline);
    assert_eq!(
        h.requests().len(),
        3,
        "the new request escalates on its own deadline"
    );
}
