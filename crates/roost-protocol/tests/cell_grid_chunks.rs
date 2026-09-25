//! The chunked cell-snapshot assembler's state machine: what it accepts, the
//! ORDER its rules run in, and that a refusal leaves no partial behind.
//!
//! The fixtures are hand-built protobuf messages rather than an encoder's
//! output, so each rule can be violated in isolation and the code that reports
//! it identified by name. The caps a part and a snapshot are held to are
//! `cell_grid_chunk_limits.rs`, and the planner that fills parts is
//! `cell_grid_chunk_planning.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::cell::frame_chunk_assembler::{
    CellGridChunkAssembler, CellGridChunkAssembly, CellGridSnapshotProgress,
};
use roost_protocol::cell::frame_chunk_validation::{
    CELL_GRID_CHUNK_STALL_MS, CellGridChunkErrorCode,
};
use roost_protocol::cell::frame_chunks::create_cell_grid_frame_part;

use support::{
    OTHER_STREAM, REPLACEMENT, SNAPSHOT, chunk_of, chunk_with, frame, part_code, pushed_code, row,
};

#[test]
fn a_two_chunk_snapshot_reports_pending_then_complete_and_rebuilds_the_frame() {
    let source = frame(4);
    let parts = [
        vec![source.viewport_rows[0].clone()],
        source.viewport_rows[1..].to_vec(),
    ];
    let mut assembler = CellGridChunkAssembler::new();
    let mut assembled = None;

    for (index, rows) in parts.iter().enumerate() {
        let chunk = chunk_of(&source, rows.clone(), index as u32, 2);
        let outcome = assembler
            .push(&chunk, 1)
            .unwrap_or_else(|error| panic!("part {index} was refused: {error}"));
        match outcome {
            CellGridChunkAssembly::Pending {
                snapshot_id,
                next_chunk_index,
            } => {
                assert_eq!(snapshot_id, SNAPSHOT);
                assert_eq!(next_chunk_index, index as u32 + 1);
                assert!(assembled.is_none(), "a pending snapshot is not a frame yet");
            }
            CellGridChunkAssembly::Complete { snapshot_id, frame } => {
                assert_eq!(snapshot_id, SNAPSHOT);
                assembled = Some(frame);
            }
        }
    }
    assert_eq!(
        assembled.as_ref(),
        Some(&source),
        "the assembled frame is the source"
    );
    assert_eq!(
        assembler.snapshot_progress(),
        None,
        "a completed snapshot is idle again"
    );
}

#[test]
fn the_header_checks_run_before_the_part_is_read() {
    // Every header fault here is stacked on the ones after it: a non-UUID
    // snapshot id over an impossible chunk_count, an out-of-range index and an
    // absent part. The earliest rule is the one that must be reported.
    let mut everything_wrong = PbCellGridChunk {
        snapshot_id: "nope".to_owned(),
        chunk_index: 0,
        chunk_count: 0,
        ..Default::default()
    };
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &everything_wrong, 0),
        CellGridChunkErrorCode::InvalidSnapshotId
    );

    everything_wrong.snapshot_id = SNAPSHOT.to_owned();
    everything_wrong.chunk_count = 0;
    everything_wrong.chunk_index = 3;
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &everything_wrong, 0),
        CellGridChunkErrorCode::ChunkCount
    );

    everything_wrong.chunk_count = 2;
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &everything_wrong, 0),
        CellGridChunkErrorCode::ChunkIndex
    );

    everything_wrong.chunk_index = 0;
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &everything_wrong, 0),
        CellGridChunkErrorCode::MissingPart
    );
}

#[test]
fn the_scalar_part_checks_run_in_contract_order() {
    let source = frame(2);
    let valid = create_cell_grid_frame_part(&source, &source.viewport_rows[..1], &[]);

    let mut part = valid.clone();
    part.full = false;
    part.stream_id = "not-a-uuid".to_owned();
    part.cols = 0;
    part.seq = 0;
    assert_eq!(part_code(part), CellGridChunkErrorCode::InvalidFull);

    let mut part = valid.clone();
    part.stream_id = "not-a-uuid".to_owned();
    part.cols = 0;
    part.seq = 0;
    assert_eq!(part_code(part), CellGridChunkErrorCode::InvalidStreamId);

    let mut part = valid.clone();
    part.cols = 0;
    part.seq = 0;
    assert_eq!(part_code(part), CellGridChunkErrorCode::InvalidGeometry);

    let mut part = valid.clone();
    part.seq = 0;
    assert_eq!(part_code(part), CellGridChunkErrorCode::InvalidSequence);

    let mut part = valid.clone();
    part.scrollback_append.push(row(0, "appended"));
    assert_eq!(part_code(part), CellGridChunkErrorCode::InvalidFull);

    let mut part = valid.clone();
    part.grid_epoch = String::new();
    assert_eq!(part_code(part), CellGridChunkErrorCode::InvalidFull);

    let mut part = valid;
    part.sb_base = 5;
    part.scrollback_total = 2;
    assert_eq!(part_code(part), CellGridChunkErrorCode::InvalidFull);
}

#[test]
fn a_part_with_no_row_at_all_is_refused_before_its_size_is_measured() {
    let source = frame(2);
    let empty = chunk_with(&source, Vec::new(), Vec::new(), 0, 1, SNAPSHOT);
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &empty, 0),
        CellGridChunkErrorCode::MissingRow
    );
}

#[test]
fn a_stalled_partial_reports_the_stall_before_anything_about_the_new_part() {
    let source = frame(2);
    let first = chunk_of(&source, vec![source.viewport_rows[0].clone()], 0, 2);
    let mut assembler = CellGridChunkAssembler::new();
    assert!(matches!(
        assembler.push(&first, 1_000),
        Ok(CellGridChunkAssembly::Pending { .. })
    ));
    assert_eq!(
        assembler.snapshot_progress(),
        Some(CellGridSnapshotProgress {
            snapshot_id: SNAPSHOT.to_owned(),
            received_chunks: 1,
            total_chunks: 2,
        })
    );

    // A replacement that would also break the order rules still reports the
    // stall: the gap is the earlier fault.
    let late = chunk_of(&source, source.viewport_rows[1..].to_vec(), 1, 2);
    assert_eq!(
        pushed_code(&mut assembler, &late, 1_000 + CELL_GRID_CHUNK_STALL_MS),
        CellGridChunkErrorCode::SnapshotStalled
    );
}

#[test]
fn expire_answers_inside_the_window_and_resets_at_the_boundary() {
    let source = frame(2);
    let first = chunk_of(&source, vec![source.viewport_rows[0].clone()], 0, 2);
    let mut assembler = CellGridChunkAssembler::new();
    assert!(
        !assembler.expire(1_000),
        "an idle assembler has nothing to expire"
    );

    assembler
        .push(&first, 1_000)
        .expect("the first part is valid");
    assert!(!assembler.expire(1_000 + CELL_GRID_CHUNK_STALL_MS - 1));
    assert_eq!(
        assembler
            .snapshot_progress()
            .map(|progress| progress.received_chunks),
        Some(1)
    );
    assert!(assembler.expire(1_000 + CELL_GRID_CHUNK_STALL_MS));
    assert_eq!(assembler.snapshot_progress(), None);
    assert!(
        !assembler.expire(1_000 + 10 * CELL_GRID_CHUNK_STALL_MS),
        "it is idle again"
    );
}

#[test]
fn a_replacement_snapshot_must_start_at_zero_inside_the_same_stream() {
    let source = frame(2);
    let other = PbCellGridFrame {
        stream_id: OTHER_STREAM.to_owned(),
        ..frame(2)
    };
    let first = chunk_of(&source, vec![source.viewport_rows[0].clone()], 0, 2);

    let mut out_of_order = CellGridChunkAssembler::new();
    out_of_order
        .push(&first, 0)
        .expect("the first part is valid");
    let rows = vec![other.viewport_rows[0].clone()];
    let replacement = chunk_with(&other, rows, Vec::new(), 1, 2, REPLACEMENT);
    assert_eq!(
        pushed_code(&mut out_of_order, &replacement, 1),
        CellGridChunkErrorCode::ChunkOrder
    );

    let mut wrong_stream = CellGridChunkAssembler::new();
    wrong_stream
        .push(&first, 0)
        .expect("the first part is valid");
    let rows = vec![other.viewport_rows[0].clone()];
    let replacement = chunk_with(&other, rows, Vec::new(), 0, 2, REPLACEMENT);
    assert_eq!(
        pushed_code(&mut wrong_stream, &replacement, 1),
        CellGridChunkErrorCode::MetadataMismatch
    );
}

#[test]
fn a_partial_snapshot_refuses_a_changed_count_metadata_or_order() {
    let source = frame(3);
    let rest = source.viewport_rows[1..].to_vec();
    let first = chunk_of(&source, vec![source.viewport_rows[0].clone()], 0, 2);

    let mut count_changed = CellGridChunkAssembler::new();
    count_changed
        .push(&first, 0)
        .expect("the first part is valid");
    let resplit = chunk_of(&source, rest.clone(), 1, 3);
    assert_eq!(
        pushed_code(&mut count_changed, &resplit, 1),
        CellGridChunkErrorCode::ChunkCount
    );

    let mut moved_cursor = CellGridChunkAssembler::new();
    moved_cursor
        .push(&first, 0)
        .expect("the first part is valid");
    let mut shifted = chunk_of(&source, rest.clone(), 1, 2);
    shifted
        .part
        .as_option_mut()
        .expect("the part is set")
        .cursor_col = 1;
    assert_eq!(
        pushed_code(&mut moved_cursor, &shifted, 1),
        CellGridChunkErrorCode::MetadataMismatch
    );

    let mut repeated = CellGridChunkAssembler::new();
    repeated.push(&first, 0).expect("the first part is valid");
    let again = chunk_of(&source, vec![source.viewport_rows[0].clone()], 0, 2);
    assert_eq!(
        pushed_code(&mut repeated, &again, 1),
        CellGridChunkErrorCode::ChunkOrder
    );
}

#[test]
fn a_rejected_part_leaves_the_assembler_with_no_partial() {
    let source = frame(4);
    let first = chunk_of(&source, vec![source.viewport_rows[0].clone()], 0, 2);
    let mut assembler = CellGridChunkAssembler::new();
    assembler.push(&first, 0).expect("the first part is valid");

    let duplicate = chunk_of(&source, vec![source.viewport_rows[0].clone()], 1, 2);
    assert_eq!(
        pushed_code(&mut assembler, &duplicate, 1),
        CellGridChunkErrorCode::DuplicateRow
    );
    assert_eq!(
        assembler.snapshot_progress(),
        None,
        "a rejection drops the partial"
    );

    // The same assembler takes a correct first part again, which only works if
    // the refused one left nothing behind.
    assert!(matches!(
        assembler.push(&first, 2),
        Ok(CellGridChunkAssembly::Pending { .. })
    ));

    let gapped = chunk_of(
        &source,
        vec![
            source.viewport_rows[2].clone(),
            source.viewport_rows[3].clone(),
        ],
        1,
        2,
    );
    assert_eq!(
        pushed_code(&mut assembler, &gapped, 3),
        CellGridChunkErrorCode::MissingRow,
        "row 1 never arrived, so the snapshot never completes"
    );
    assert_eq!(assembler.snapshot_progress(), None);
}

#[test]
fn a_row_index_outside_the_snapshot_is_refused_wherever_it_sits() {
    let source = frame(4);
    let mut out_of_range = source.viewport_rows[0].clone();
    out_of_range.index = 7;
    let chunk = chunk_of(&source, vec![out_of_range], 0, 1);
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &chunk, 0),
        CellGridChunkErrorCode::RowIndex
    );

    let mut with_history = source.clone();
    with_history.sb_base = 5;
    with_history.scrollback_total = 6;
    let chunk = chunk_with(
        &with_history,
        vec![source.viewport_rows[0].clone()],
        vec![row(9, "h9")],
        0,
        1,
        SNAPSHOT,
    );
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &chunk, 0),
        CellGridChunkErrorCode::RowIndex,
        "history must continue at sb_base and stop below scrollback_total"
    );
}
