//! Every row of the delta-fence table in `docs/phase4-client-contract.md` §6.3,
//! one weakened rule at a time.
//!
//! Each case builds a frame that satisfies EVERY OTHER row and breaks exactly
//! one, so a failure names the rule that was relaxed rather than "the delta was
//! refused". Each also asserts the replica is byte-identical afterwards, because
//! "invalidates the cursor" means the canonical frame is left exactly as it was —
//! not partially advanced — so the next full is the first thing that moves it.

mod support;

use roost_client_core::Admission;
use support::{
    EPOCH, NEXT_EPOCH, OTHER_STREAM, SESSION, STREAM, delta, full, replica_with_baseline,
    sync_token,
};

/// Feed one frame to a replica that already holds a complete full, and assert it
/// was refused without moving the grid.
///
/// Every case here is on the RIGHT stream and the RIGHT generation, so each one
/// is a real gap and each must latch exactly one repair — "any gap invalidates
/// the cursor and latches one snapshot request"
/// (`protocol/spec/terminal-stream.md:27`). A refusal that latches nothing is a
/// frame for a replica that does not exist, and that case has its own test.
fn refuse(broken: roost_proto::PbCellGridFrame, why: &str) {
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);
    let before = replica.canonical().cloned();
    let outcome = replica.admit_frame(&broken, false, &token, 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "{why}: a gap on this replica must latch exactly one repair, got {outcome:?}"
    );
    assert!(
        replica.repair_latched(),
        "{why}: the repair should be latched"
    );
    assert_eq!(
        replica.canonical(),
        before.as_ref(),
        "{why}: a refused frame must leave the replica byte-identical"
    );
}

/// The same, but insisting that THIS CRATE's fence is what refused it.
///
/// The delta-fence table has TWO enforcement points, and a test that only asserts
/// "it was refused" cannot tell them apart. `roost_protocol::cell`'s decoder
/// already rejects a duplicate row index, a row index past the grid, a
/// `scrollback_rows` on a delta, and a `seq` that does not follow `base_seq` —
/// those four frames never reach this crate's fold at all. The rows left are the
/// ones only the client's fold can catch, and they report `delta_unfollowed`;
/// `delta_fold_rejected` would mean the shared `apply_delta` caught it instead,
/// which is a different rule and would leave this row untested.
fn refuse_at_the_client_fence(broken: roost_proto::PbCellGridFrame, why: &str) {
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);
    let before = replica.canonical().cloned();
    let outcome = replica.admit_frame(&broken, false, &token, 10);
    match &outcome {
        Admission::Refused {
            reason,
            latched: true,
        } => assert_eq!(
            reason, "delta_unfollowed",
            "{why}: the CLIENT's fence must refuse this, not the shared decoder or helper"
        ),
        other => panic!("{why}: a gap on this replica must latch one repair, got {other:?}"),
    }
    assert_eq!(
        replica.canonical(),
        before.as_ref(),
        "{why}: a refused frame must leave the replica byte-identical"
    );
}

/// A delta that satisfies every rule is admitted, so each refusal below is
/// attributable to the one rule it breaks and not to a broken fixture.
#[test]
fn an_unbroken_delta_is_admitted() {
    let mut replica = replica_with_baseline(4);
    let outcome = replica.admit_frame(&delta(1, 4, 2), false, &sync_token(1, 1), 10);
    assert_eq!(outcome, Admission::DeltaApplied);
    let canonical = replica
        .canonical()
        .expect("the baseline is still installed");
    assert_eq!(
        canonical.seq, 2,
        "the replica advanced exactly one sequence"
    );
}

#[test]
fn a_delta_from_a_previous_grid_epoch_is_refused() {
    // THE EPOCH FENCE. A resize mints a new grid epoch, and a delta from the
    // previous one indexes a grid that no longer exists.
    let mut broken = delta(1, 4, 2);
    broken.grid_epoch = NEXT_EPOCH.to_owned();
    refuse_at_the_client_fence(broken, "grid epoch");
}

/// Refused by the shared decoder in `roost_protocol::cell`, upstream of this
/// crate's fold — which is the point of the row, and why this one uses the
/// stage-agnostic helper. See `refuse_at_the_client_fence`.
#[test]
fn a_delta_whose_base_is_not_the_installed_sequence_is_refused() {
    let mut broken = delta(7, 4, 2);
    broken.seq = 8;
    refuse(broken, "base_seq");
}

/// Refused by the shared decoder in `roost_protocol::cell`, upstream of this
/// crate's fold — which is the point of the row, and why this one uses the
/// stage-agnostic helper. See `refuse_at_the_client_fence`.
#[test]
fn a_delta_that_skips_a_sequence_is_refused() {
    // A gap is not a fast path: `seq` must be the EXACT successor.
    let mut broken = delta(1, 4, 2);
    broken.seq = 5;
    refuse(broken, "non-successor seq");
}

/// A frame for ANOTHER stream does not latch, and is the one refusal on this
/// path that does not.
///
/// It is not a gap in this replica — it is a frame for a replica that does not
/// exist. Latching would ask the authority for a baseline it already sent and
/// this client ignored, which is how one stale frame becomes a request storm.
#[test]
fn a_delta_for_another_stream_is_refused_without_latching() {
    let mut broken = delta(1, 4, 2);
    broken.stream_id = OTHER_STREAM.to_owned();
    let mut replica = replica_with_baseline(4);
    let before = replica.canonical().cloned();
    let outcome = replica.admit_frame(&broken, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: false, .. }),
        "a foreign-stream frame must not latch this replica's repair, got {outcome:?}"
    );
    assert!(!replica.repair_latched());
    assert_eq!(replica.canonical(), before.as_ref());
}

#[test]
fn a_delta_in_a_different_geometry_is_refused() {
    // The PANE's geometry, not merely the sender's: a grid the pane cannot show
    // would then be folded against the wrong row count forever after.
    let mut broken = delta(1, 4, 2);
    broken.rows = 6;
    refuse_at_the_client_fence(broken, "rows");
}

#[test]
fn a_delta_that_changes_alt_screen_is_refused() {
    // Alt-screen occupancy changes the row numbering, so a delta cannot be
    // applied across the transition.
    let mut broken = delta(1, 4, 2);
    broken.alt_screen = true;
    refuse_at_the_client_fence(broken, "alt screen");
}

/// Refused by the shared decoder in `roost_protocol::cell`, upstream of this
/// crate's fold — which is the point of the row, and why this one uses the
/// stage-agnostic helper. See `refuse_at_the_client_fence`.
#[test]
fn a_delta_that_also_restates_history_is_refused() {
    // A delta appends; it does not restate. `scrollback_rows` on a delta is a
    // second description of history the replica already holds.
    let mut broken = delta(1, 4, 2);
    broken.scrollback_rows = vec![support::row(0, "old")];
    refuse(broken, "scrollback_rows on a delta");
}

/// Refused by the shared decoder in `roost_protocol::cell`, upstream of this
/// crate's fold — which is the point of the row, and why this one uses the
/// stage-agnostic helper. See `refuse_at_the_client_fence`.
#[test]
fn a_delta_repeating_a_row_index_is_refused() {
    // Overwrite-by-index is order-dependent when an index appears twice, so the
    // duplicate is refused rather than resolved by last-write-wins.
    let mut broken = delta(1, 4, 2);
    broken.viewport_rows = vec![support::row(1, "a"), support::row(1, "b")];
    refuse(broken, "duplicate row index");
}

/// Refused by the shared decoder in `roost_protocol::cell`, upstream of this
/// crate's fold — which is the point of the row, and why this one uses the
/// stage-agnostic helper. See `refuse_at_the_client_fence`.
#[test]
fn a_delta_indexing_past_the_grid_is_refused() {
    let mut broken = delta(1, 4, 2);
    broken.viewport_rows = vec![support::row(9, "a")];
    refuse(broken, "row index past the grid");
}

#[test]
fn a_delta_while_a_chunked_baseline_is_in_flight_is_refused() {
    // A delta arriving mid-baseline belongs to the frame the partial is
    // rebuilding. Folding it onto the old canonical and then completing the
    // snapshot publishes a grid that is neither the old one nor the new one.
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);
    let before = replica.canonical().cloned();
    // Split the same full into two parts, so the assembler is genuinely mid-flight.
    let source = full(4);
    let first = roost_proto::PbCellGridChunk {
        // The snapshot id is a UUID on the wire, and the assembler refuses
        // anything else before it looks at the rows.
        snapshot_id: "00000000-0000-4000-8000-000000000011".to_owned(),
        chunk_index: 0,
        chunk_count: 2,
        part: roost_proto::buffa::MessageField::some(
            roost_protocol::cell::frame_chunks::create_cell_grid_frame_part(
                &source,
                &source.viewport_rows[..2],
                &[],
            ),
        ),
        ..Default::default()
    };
    assert_eq!(
        replica.admit_chunk(&first, &token, 10),
        Admission::ChunkPending
    );
    // `base_seq` 1 and `seq` 2, matching the INSTALLED canonical exactly, so the
    // chunk-in-flight row is the only one that can refuse this. With a base_seq of
    // 2 the `base_seq` row would refuse it instead and the test would pass even
    // with the chunk rule deleted.
    let outcome = replica.admit_frame(&delta(1, 4, 2), false, &token, 11);
    match &outcome {
        Admission::Refused {
            reason,
            latched: true,
        } => assert_eq!(
            reason, "delta_unfollowed",
            "the CLIENT's chunk-in-flight fence must refuse this, not another row"
        ),
        other => panic!("a delta during a chunked baseline must be refused, got {other:?}"),
    }
    assert_eq!(replica.canonical(), before.as_ref());
}

#[test]
fn a_delta_for_a_retired_generation_is_refused_and_latches_nothing() {
    // The frame is not bad; it belongs to a replica that no longer exists. Latching
    // a repair for THIS one would be repairing the wrong session.
    let mut replica = replica_with_baseline(4);
    let before = replica.canonical().cloned();
    let outcome = replica.admit_frame(&delta(2, 4, 2), false, &sync_token(99, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: false, .. }),
        "got {outcome:?}"
    );
    assert_eq!(replica.canonical(), before.as_ref());
}

#[test]
fn a_frame_whose_session_does_not_match_the_replica_latches_a_repair() {
    // A session mismatch is the one mismatch that IS this replica's problem: the
    // frame claims to be for this session and is not admissible as it stands.
    let mut broken = delta(2, 4, 2);
    broken.session_id = "00000000-0000-4000-8000-0000000000ff".to_owned();
    let mut replica = replica_with_baseline(4);
    let outcome = replica.admit_frame(&broken, false, &sync_token(1, 1), 10);
    assert!(
        matches!(outcome, Admission::Refused { latched: true, .. }),
        "a session mismatch must latch exactly one repair, got {outcome:?}"
    );
}

/// The fence above is only meaningful if the epoch really is on the wire and
/// really is what the replica reads, so this pins the plumbing the fence depends
/// on rather than the fence again.
#[test]
fn the_grid_epoch_is_read_off_the_wire_onto_the_replica() {
    let replica = replica_with_baseline(4);
    let canonical = replica.canonical().expect("a baseline is installed");
    assert_eq!(canonical.grid_epoch, EPOCH);
}

/// The stream-splice case, and the only one that reaches `!baseline_ready`.
///
/// After a stream change the replica keeps the OLD canonical in place but marks
/// itself not-ready, because the renderer is still painting those rows. So
/// `canonical` is `Some` while `baseline_ready` is `false` — the one state where
/// a delta can satisfy EVERY OTHER row of the fence table: the delta names the
/// new expected stream, the canonical is stale, and when the epoch, dimensions,
/// `base_seq` and `seq` all line up, only `!baseline_ready` stands between the
/// client and folding one grid's delta onto another's.
///
/// With that row deleted this delta is applied and `apply_delta` waves it
/// through. That is the splice this crate exists to prevent.
#[test]
fn a_delta_on_a_new_stream_never_folds_onto_the_previous_streams_grid() {
    let next_stream = "00000000-0000-4000-8000-0000000000b1";
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);

    // A new stream is minted for the resize. The replica keeps the old grid up
    // and stops calling itself ready.
    replica.install_expected_stream(next_stream, 8, 4);
    assert!(
        !replica.baseline_ready(),
        "the new stream has no baseline yet"
    );
    assert!(
        replica.canonical().is_some(),
        "the renderer is still painting the previous grid, so it is retained"
    );

    // A delta satisfying every OTHER row: the new expected stream, the same
    // epoch and dimensions, and a base_seq matching the RETAINED canonical.
    let spliced = roost_proto::PbCellGridFrame {
        session_id: SESSION.to_owned(),
        stream_id: next_stream.to_owned(),
        grid_epoch: EPOCH.to_owned(),
        cols: 8,
        rows: 4,
        full: false,
        viewport_rows: vec![support::row(1, "from the other grid")],
        base_seq: 1,
        seq: 2,
        ..Default::default()
    };
    let before = replica.canonical().cloned();
    let outcome = replica.admit_frame(&spliced, false, &token, 10);
    assert_eq!(
        outcome,
        Admission::Refused {
            reason: "delta_unfollowed".to_string(),
            latched: true,
        },
        "a delta for the new stream must not fold onto the previous stream's grid"
    );
    assert_eq!(replica.canonical(), before.as_ref(), "nothing was spliced");
}

/// The ONLY state that reaches `!baseline_ready` on its own, and the reason that
/// row is not redundant.
///
/// `install_expected_stream` recomputes readiness on a GEOMETRY change without
/// clearing the canonical, because the renderer is still painting those rows. So
/// after a resize on the SAME stream, `canonical.stream_id` still equals the
/// expected stream id and only `baseline_ready` is false. A delta carrying the
/// NEW geometry, the same epoch, and a `base_seq` matching the RETAINED
/// canonical then satisfies every other row of the table — including the two
/// stream rows that mask this in the splice case above — and is applied onto a
/// grid that is 12 columns wide while the delta is 12 columns wide and the
/// canonical is 8. `apply_delta` checks `delta.cols != base.cols` and refuses,
/// but only AFTER it has been handed the frame.
///
/// With `!baseline_ready` deleted this delta is folded. The client has accepted a
/// 12-column delta onto an 8-column grid, and every later delta compounds it.
#[test]
fn a_delta_at_the_new_geometry_never_folds_onto_the_old_geometry() {
    let mut replica = replica_with_baseline(4);
    let token = sync_token(1, 1);

    // A resize on the same stream: the expectation moves, the painted grid stays.
    replica.install_expected_stream(STREAM, 12, 4);
    assert!(
        !replica.baseline_ready(),
        "the new geometry has no baseline of its own yet"
    );
    let before = replica.canonical().cloned();
    assert_eq!(
        before.as_ref().map(|frame| frame.cols),
        Some(8),
        "the retained grid is still the old, narrower one"
    );

    let resized = roost_proto::PbCellGridFrame {
        session_id: SESSION.to_owned(),
        stream_id: STREAM.to_owned(),
        grid_epoch: EPOCH.to_owned(),
        cols: 12,
        rows: 4,
        full: false,
        viewport_rows: vec![support::row(1, "twelve columns")],
        base_seq: 1,
        seq: 2,
        ..Default::default()
    };
    let outcome = replica.admit_frame(&resized, false, &token, 10);
    assert_eq!(
        outcome,
        Admission::Refused {
            reason: "delta_unfollowed".to_string(),
            latched: true,
        },
        "a delta at the new geometry must not fold onto the old geometry's grid"
    );
    assert_eq!(replica.canonical(), before.as_ref(), "nothing was reflowed");
}
