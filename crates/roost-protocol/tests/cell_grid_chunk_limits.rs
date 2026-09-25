//! The caps one chunked cell snapshot is held to: the part byte limit, the span
//! cap, the interned link runs, and the whole-snapshot byte ceiling.
//!
//! Every limit here IS a canonical protobuf byte count or an exact count of
//! rows, so each case is pinned at the limit and one entry past it. The
//! assembler's state machine is `cell_grid_chunks.rs`, and the planner that
//! fills parts to those limits is `cell_grid_chunk_planning.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::{HashMap, HashSet};

use roost_proto::{PbCellRow, PbCellSpan};
use roost_protocol::cell::frame_chunk_assembler::CellGridChunkAssembler;
use roost_protocol::cell::frame_chunk_validation::{
    CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS,
    CELL_GRID_SNAPSHOT_MAX_SPANS, CellGridChunkErrorCode, add_snapshot_rows,
    assert_cell_grid_snapshot,
};
use roost_protocol::cell::frame_chunks::{decode_cell_grid_chunk, encoded_cell_grid_chunk_size};

use support::{chunk_of, frame, pushed_code, row};

#[test]
fn an_oversized_part_is_refused_on_its_encoded_size() {
    let mut source = frame(1);
    source.cols = 1;
    let mut wide = source.viewport_rows[0].clone();
    wide.spans[0].text = "x".repeat(CELL_GRID_PART_MAX_BYTES as usize);
    let chunk = chunk_of(&source, vec![wide], 0, 1);
    assert!(encoded_cell_grid_chunk_size(&chunk) > CELL_GRID_PART_MAX_BYTES);
    assert_eq!(
        pushed_code(&mut CellGridChunkAssembler::new(), &chunk, 0),
        CellGridChunkErrorCode::ChunkSize
    );

    let oversized = vec![0u8; CELL_GRID_PART_MAX_BYTES as usize + 1];
    let refused = decode_cell_grid_chunk(&oversized).expect_err("an oversized buffer");
    assert!(refused.to_string().starts_with("chunk-size:"));
}

#[test]
fn the_span_cap_is_exact() {
    let source = frame(4);
    let mut seen = HashSet::new();
    let mut links = HashMap::new();
    let at_cap = add_snapshot_rows(
        &source,
        &source.viewport_rows,
        &mut seen,
        &mut links,
        CELL_GRID_SNAPSHOT_MAX_SPANS - 4,
    );
    assert_eq!(at_cap, Ok(CELL_GRID_SNAPSHOT_MAX_SPANS));
    assert_eq!(seen.len(), 4);

    let mut seen = HashSet::new();
    let mut links = HashMap::new();
    let over_cap = add_snapshot_rows(
        &source,
        &source.viewport_rows,
        &mut seen,
        &mut links,
        CELL_GRID_SNAPSHOT_MAX_SPANS - 3,
    );
    assert_eq!(
        over_cap.err().map(|error| error.code),
        Some(CellGridChunkErrorCode::SpanLimit)
    );
}

#[test]
fn link_mappings_are_interned_and_a_conflicting_key_is_refused() {
    assert_eq!(CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS, 1_024);
    let mut linked = frame(5);
    linked.cols = 256;
    linked.viewport_rows = (0..5)
        .map(|row_index| PbCellRow {
            index: row_index,
            spans: (0..256)
                .map(|span_index| PbCellSpan {
                    text: "x".to_owned(),
                    fg: 256,
                    bg: 256,
                    columns: 1,
                    link_key: Some(format!("key-{row_index}-{span_index}")),
                    link_uri: Some(format!("https://example.test/{row_index}/{span_index}")),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .collect();
    assert_eq!(
        assert_cell_grid_snapshot(&linked)
            .err()
            .map(|error| error.code),
        Some(CellGridChunkErrorCode::LinkLimit)
    );

    let mut conflicting = frame(2);
    for (index, uri) in ["https://a", "https://b"].iter().enumerate() {
        let mut linked_row = conflicting.viewport_rows[index].clone();
        linked_row.spans[0].link_key = Some("k".to_owned());
        linked_row.spans[0].link_uri = Some((*uri).to_owned());
        conflicting.viewport_rows[index] = linked_row;
    }
    let rest = conflicting.viewport_rows[1..].to_vec();
    let mut assembler = CellGridChunkAssembler::new();
    assembler
        .push(
            &chunk_of(
                &conflicting,
                vec![conflicting.viewport_rows[0].clone()],
                0,
                2,
            ),
            0,
        )
        .expect("the first part is valid");
    assert_eq!(
        pushed_code(&mut assembler, &chunk_of(&conflicting, rest, 1, 2), 1),
        CellGridChunkErrorCode::LinkConflict
    );
}

#[test]
fn a_snapshot_over_sixty_four_mib_is_refused_part_by_part() {
    assert_eq!(CELL_GRID_SNAPSHOT_MAX_BYTES, 64 * 1_048_576);
    let mut source = frame(65);
    source.cols = 1;
    let payload = "x".repeat(CELL_GRID_PART_MAX_BYTES as usize - 512);
    source.viewport_rows = (0..65).map(|index| row(index, &payload)).collect();

    let mut assembler = CellGridChunkAssembler::new();
    let mut failure = None;
    for index in 0..65u32 {
        let carried = source.viewport_rows[index as usize].clone();
        let chunk = chunk_of(&source, vec![carried], index, 65);
        if let Err(error) = assembler.push(&chunk, u64::from(index)) {
            failure = Some(error.code);
            break;
        }
    }
    assert_eq!(failure, Some(CellGridChunkErrorCode::SnapshotSize));
}
