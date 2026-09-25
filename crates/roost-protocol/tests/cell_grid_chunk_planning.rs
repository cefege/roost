//! Planning a chunked cell snapshot: where the part boundaries fall, that the
//! measured bytes agree with the real encoder, and that a planned snapshot
//! survives the round trip back into one frame.
//!
//! The fixtures that need a real encoder live here rather than in the
//! assembler suite, because the byte arithmetic IS the subject: an estimator
//! that drifts by one byte is a fleet that stalls instead of attaching. The
//! assembler's state machine is `cell_grid_chunks.rs`, and the caps the plan
//! is filled against are `cell_grid_chunk_limits.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_proto::buffa::Message;
use roost_proto::{PbCellGridChunk, PbCellRow, PbCellSpan};
use roost_protocol::cell::frame_chunk_assembler::{CellGridChunkAssembler, CellGridChunkAssembly};
use roost_protocol::cell::frame_chunk_validation::{
    CELL_GRID_PART_MAX_BYTES, CellGridChunkError, CellGridChunkErrorCode,
};
use roost_protocol::cell::frame_chunks::{
    CellGridSnapshotPart, chunk_cell_grid_frame, create_cell_grid_frame_part,
    create_cell_grid_snapshot_source, decode_cell_grid_chunk, encode_cell_grid_chunk,
    encoded_cell_grid_chunk_size, encoded_cell_grid_frame_size,
};

use support::{SNAPSHOT, accepted, chunk_with, frame, row};

#[test]
fn a_forced_small_full_reassembles_to_the_original_frame() {
    let source = frame(4);
    let chunks = chunk_cell_grid_frame(&source, SNAPSHOT).expect("the source is valid");
    assert_eq!(
        chunks.len(),
        1,
        "a small full is one chunk even when chunking is forced"
    );

    let encoded = encode_cell_grid_chunk(&chunks[0]).expect("a small chunk encodes");
    let decoded = decode_cell_grid_chunk(&encoded).expect("it decodes");
    assert_eq!(accepted(&decoded), source);
}

#[test]
fn an_authoritative_history_bridge_survives_the_round_trip() {
    let mut source = frame(2);
    source.sb_base = 700;
    source.scrollback_total = 703;
    source.scrollback_rows = (700..703)
        .map(|index| row(index, &format!("h{index}")))
        .collect();

    let parts = [
        (source.scrollback_rows.clone(), Vec::new()),
        (Vec::new(), vec![source.viewport_rows[0].clone()]),
        (Vec::new(), source.viewport_rows[1..].to_vec()),
    ];
    let mut assembler = CellGridChunkAssembler::new();
    let mut assembled = None;
    for (index, (scrollback, viewport)) in parts.iter().enumerate() {
        let carried = viewport.clone();
        let chunk = chunk_with(
            &source,
            carried,
            scrollback.clone(),
            index as u32,
            3,
            SNAPSHOT,
        );
        let outcome = assembler
            .push(&chunk, 1)
            .unwrap_or_else(|error| panic!("part {index} was refused: {error}"));
        if let CellGridChunkAssembly::Complete { frame, .. } = outcome {
            assembled = Some(frame);
        }
    }
    assert_eq!(assembled.as_ref(), Some(&source));
}

#[test]
fn encoded_sizes_agree_with_the_real_encoder() {
    let mut source = frame(3);
    source.cols = 6;
    source.cursor_keys_app = true;
    source.mouse_sgr = true;
    source.viewport_rows[0].spans[0] = PbCellSpan {
        text: "hi".to_owned(),
        fg: 7,
        bg: 8,
        flags: 3,
        fg_rgb: Some(0x00ff00),
        bg_rgb: Some(0x101010),
        columns: 2,
        link_key: Some("k-0".to_owned()),
        link_uri: Some("https://example.test/green".to_owned()),
        ..Default::default()
    };
    source.viewport_rows[1].spans[0] = PbCellSpan {
        text: "中".to_owned(),
        columns: 2,
        link_key: Some("k-1".to_owned()),
        link_uri: Some("https://example.test/wide".to_owned()),
        ..Default::default()
    };
    source.sb_base = 12;
    source.scrollback_total = 13;
    source.scrollback_rows = vec![row(12, "history")];

    let part = create_cell_grid_frame_part(&source, &source.viewport_rows, &source.scrollback_rows);
    assert_eq!(
        encoded_cell_grid_frame_size(&part) as usize,
        part.encode_to_vec().len()
    );
    assert_eq!(
        encoded_cell_grid_frame_size(&source) as usize,
        source.encode_to_vec().len()
    );

    // The chunk carries the frame's history too: a snapshot that declares a
    // scrollback total never completes on viewport rows alone, and the encoder
    // has to account for those bytes to agree with the estimator.
    let chunk = chunk_with(
        &source,
        source.viewport_rows.clone(),
        source.scrollback_rows.clone(),
        0,
        1,
        SNAPSHOT,
    );
    assert_eq!(
        encoded_cell_grid_chunk_size(&chunk) as usize,
        chunk.encode_to_vec().len()
    );
    let encoded = encode_cell_grid_chunk(&chunk).expect("a small chunk encodes");
    assert_eq!(
        accepted(&decode_cell_grid_chunk(&encoded).expect("it decodes")),
        source
    );
}

#[test]
fn a_frame_over_one_mib_is_split_on_whole_row_boundaries() {
    let mut source = frame(8);
    source.cols = 1;
    let text = "x".repeat(300 * 1_024);
    source.viewport_rows = (0..8).map(|index| row(index, &text)).collect();
    source.sb_base = 40;
    source.scrollback_total = 42;
    source.scrollback_rows = (40..42).map(|index| row(index, &text)).collect();

    assert!(encoded_cell_grid_frame_size(&source) > CELL_GRID_PART_MAX_BYTES);
    let chunks = chunk_cell_grid_frame(&source, SNAPSHOT).expect("the source is valid");
    assert!(chunks.len() > 1, "an oversized full must be split");

    let mut carried: Vec<PbCellRow> = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.chunk_index, index as u32);
        assert_eq!(chunk.chunk_count, chunks.len() as u32);
        assert_eq!(chunk.snapshot_id, SNAPSHOT);
        let bytes = encoded_cell_grid_chunk_size(chunk);
        assert!(
            bytes <= CELL_GRID_PART_MAX_BYTES,
            "part {index} is {bytes} bytes"
        );
        assert_eq!(bytes as usize, chunk.encode_to_vec().len());

        let part = chunk.part.as_option().expect("the part is set");
        carried.extend(part.scrollback_rows.iter().cloned());
        carried.extend(part.viewport_rows.iter().cloned());
    }
    let mut expected = source.scrollback_rows.clone();
    expected.extend(source.viewport_rows.iter().cloned());
    assert_eq!(
        carried, expected,
        "history leads, then the viewport, one whole row each"
    );
}

#[test]
fn every_planned_part_is_full_to_the_budget() {
    let mut source = frame(6);
    source.cols = 1;
    let text = "x".repeat(400 * 1_024);
    source.viewport_rows = (0..6).map(|index| row(index, &text)).collect();

    let planner = create_cell_grid_snapshot_source(&source, true).expect("the source is valid");
    let planned = planner
        .create_cursor(SNAPSHOT)
        .expect("a UUID is a valid snapshot id");
    assert_eq!(
        planned.part_count(),
        3,
        "two 400 KiB rows fit in a 1 MiB part"
    );

    let mut unplanned: Vec<PbCellRow> = source.viewport_rows.clone();
    let mut previous: Option<PbCellGridChunk> = None;
    for index in 0..planned.part_count() {
        let chunk = match planned.materialize(index).expect("the index is in range") {
            CellGridSnapshotPart::Chunk(chunk) => chunk,
            CellGridSnapshotPart::Frame(_) => panic!("a forced source yields chunks"),
        };
        if let Some(previous) = previous
            && let Some(next) = unplanned.first().cloned()
        {
            // Cloned, not removed: the probe asks "would the NEXT row still
            // have fitted?", and the next row still has to be there for the
            // part after this one to claim.
            let mut fuller = previous.clone();
            let part = fuller.part.as_option_mut().expect("the part is set");
            part.viewport_rows.push(next);
            assert!(
                encoded_cell_grid_chunk_size(&fuller) > CELL_GRID_PART_MAX_BYTES,
                "part {index} left room for a row it should have taken"
            );
        }
        let taken = chunk
            .part
            .as_option()
            .expect("the part is set")
            .viewport_rows
            .len();
        unplanned.drain(..taken);
        previous = Some(chunk);
    }
    assert!(unplanned.is_empty(), "every row was planned into some part");
}

#[test]
fn a_small_full_is_still_sent_as_one_frame() {
    let source = frame(3);
    let planner = create_cell_grid_snapshot_source(&source, false).expect("the source is valid");
    let unplanned = planner
        .create_cursor(SNAPSHOT)
        .expect("a UUID is a valid snapshot id");
    assert_eq!(unplanned.part_count(), 1);
    match unplanned.materialize(0).expect("index 0 exists") {
        CellGridSnapshotPart::Frame(frame) => assert_eq!(frame, source),
        CellGridSnapshotPart::Chunk(_) => panic!("a small full is not chunked"),
    }
    assert!(unplanned.materialize(1).is_err(), "there is no second part");
}

#[test]
fn a_row_too_big_for_any_part_is_refused_rather_than_split() {
    let mut source = frame(2);
    source.cols = 1;
    source.viewport_rows[0].spans[0].text = "x".repeat(CELL_GRID_PART_MAX_BYTES as usize);
    let refused = chunk_cell_grid_frame(&source, SNAPSHOT).expect_err("no part can hold it");
    assert_eq!(refused.code, CellGridChunkErrorCode::SingleRowOversize);
    assert!(refused.reason.contains("cannot fit"));
}

#[test]
fn a_cursor_refuses_a_snapshot_id_that_is_not_a_uuid() {
    let source = frame(2);
    let planned = create_cell_grid_snapshot_source(&source, true).expect("planning is fine");
    let refused: CellGridChunkError = planned
        .create_cursor("nope")
        .expect_err("a UUID is required");
    assert_eq!(refused.code, CellGridChunkErrorCode::InvalidSnapshotId);
}

#[test]
fn an_invalid_source_never_reaches_the_planner() {
    let mut source = frame(2);
    source.viewport_rows.pop();
    let refused = create_cell_grid_snapshot_source(&source, true).expect_err("one row is missing");
    assert_eq!(refused.code, CellGridChunkErrorCode::MissingRow);
}
