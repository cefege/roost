//! Inline image placements on the cell frame: they survive the wire, the
//! chunked snapshot path and the single fold, and a decoder refuses a set a
//! renderer could not place.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use roost_protocol::cell::delta_batch::fold_cell_delta_batch;
use roost_protocol::cell::diff_grid::apply_delta;
use roost_protocol::cell::frame_chunk_assembler::{CellGridChunkAssembler, CellGridChunkAssembly};
use roost_protocol::cell::frame_chunks::chunk_cell_grid_frame;
use roost_protocol::cell::types::{ImagePlacement, ImagePlacements};
use roost_protocol::cell::{cell_frame_to_proto, proto_to_cell_frame};

use support::{SNAPSHOT, frame, full_frame, next_delta, text_row};

fn placement(image_key: u64, row: u64) -> ImagePlacement {
    ImagePlacement {
        image_key,
        row,
        col: 5,
        columns: 4,
        rows: 2,
        source_x: 0,
        source_y: 0,
        source_width: 32,
        source_height: 32,
        image_width: 32,
        image_height: 32,
        offset_x_px: 1,
        offset_y_px: 2,
        z_index: -1,
    }
}

fn two_placements() -> ImagePlacements {
    Arc::from(vec![placement(7, 0), placement(9, 1)])
}

#[test]
fn placements_round_trip_through_the_wire() {
    let mut source = full_frame(&["zero", "one", "two"]);
    source.image_placements = Some(two_placements());
    let wire = cell_frame_to_proto(&source, "session").unwrap();
    assert!(wire.image_placements_present);
    let decoded = proto_to_cell_frame(&wire).unwrap();
    assert_eq!(decoded.image_placements, Some(two_placements()));
}

#[test]
fn a_delta_without_a_set_decodes_as_unchanged_and_a_full_as_empty() {
    let base = full_frame(&["zero", "one"]);
    let mut delta = next_delta(&base, vec![text_row(0, "ZERO")], Vec::new());
    delta.image_placements = None;
    let wire = cell_frame_to_proto(&delta, "session").unwrap();
    assert_eq!(proto_to_cell_frame(&wire).unwrap().image_placements, None);

    let mut full = cell_frame_to_proto(&base, "session").unwrap();
    full.image_placements_present = false;
    let decoded = proto_to_cell_frame(&full).unwrap();
    assert_eq!(decoded.image_placements.as_deref(), Some(&[][..]));
}

#[test]
fn placements_survive_a_chunked_snapshot() {
    let mut source = frame(4);
    let placements = cell_frame_to_proto(
        &{
            let mut held = full_frame(&["x"]);
            held.image_placements = Some(two_placements());
            held
        },
        "s",
    )
    .unwrap();
    source.image_placements = placements.image_placements;
    source.image_placements_present = true;
    let chunks = chunk_cell_grid_frame(&source, SNAPSHOT).unwrap();
    let mut assembler = CellGridChunkAssembler::new();
    let mut assembled = None;
    for chunk in &chunks {
        if let CellGridChunkAssembly::Complete { frame, .. } = assembler.push(chunk, 0).unwrap() {
            assembled = Some(frame);
        }
    }
    let assembled = assembled.expect("the snapshot completes");
    assert!(assembled.image_placements_present);
    assert_eq!(assembled.image_placements, source.image_placements);
}

#[test]
fn a_decoder_refuses_what_cannot_be_placed() {
    let mut source = full_frame(&["zero"]);
    source.image_placements = Some(Arc::from(vec![placement(1, 0); 257]));
    let wire = cell_frame_to_proto(&source, "session").unwrap();
    assert!(proto_to_cell_frame(&wire).is_err(), "257 placements");

    let mut empty_span = placement(1, 0);
    empty_span.columns = 0;
    source.image_placements = Some(Arc::from(vec![empty_span]));
    let wire = cell_frame_to_proto(&source, "session").unwrap();
    assert!(proto_to_cell_frame(&wire).is_err(), "a zero-column span");
}

#[test]
fn a_delta_replaces_the_set_only_when_it_carries_one() {
    let mut base = full_frame(&["zero", "one"]);
    base.image_placements = Some(two_placements());

    let mut unchanged = next_delta(&base, vec![text_row(0, "ZERO")], Vec::new());
    unchanged.image_placements = None;
    let mut held = base.clone();
    apply_delta(&mut held, &unchanged).unwrap();
    assert_eq!(held.image_placements, Some(two_placements()));

    let mut cleared = next_delta(&held, Vec::new(), Vec::new());
    cleared.image_placements = Some(Arc::from([]));
    apply_delta(&mut held, &cleared).unwrap();
    assert_eq!(held.image_placements.as_deref(), Some(&[][..]));

    let mut replaced = next_delta(&held, Vec::new(), Vec::new());
    replaced.image_placements = Some(Arc::from(vec![placement(3, 1)]));
    apply_delta(&mut held, &replaced).unwrap();
    assert_eq!(
        held.image_placements.as_deref(),
        Some(&[placement(3, 1)][..])
    );
}

#[test]
fn a_folded_batch_keeps_the_newest_set() {
    let base = full_frame(&["zero", "one"]);
    let mut first = next_delta(&base, Vec::new(), Vec::new());
    first.image_placements = Some(Arc::from(vec![placement(1, 0)]));
    let mut second = next_delta(&first, vec![text_row(1, "ONE")], Vec::new());
    second.image_placements = None;
    let batch = fold_cell_delta_batch(&base, &[first, second]).unwrap();
    assert_eq!(
        batch.frame.image_placements.as_deref(),
        Some(&[placement(1, 0)][..])
    );
}
