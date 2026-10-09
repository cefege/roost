//! Terminal image decoding, placement frames, and retained PNG bytes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{CellEmitState, RioCore, TerminalCore, next_cell_frame};

const KITTY_IMAGE: &[u8] = b"\x1b_Ga=T,f=32,s=2,v=2,c=4,r=2;/wAA//8AAP//AAD//wAA//==\x1b\\";

#[test]
fn kitty_transmission_produces_placement_and_png() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(KITTY_IMAGE);
    let placements = core.image_placements();
    assert_eq!(placements.len(), 1);
    let placement = &placements[0];
    assert_eq!(
        (placement.col, placement.columns, placement.rows),
        (0, 4, 2)
    );
    assert_eq!((placement.image_width, placement.image_height), (2, 2));
    assert_eq!(placement.viewport_row, 0);
    let png = core.image_png(placement.image_key).expect("PNG exists");
    let decoded = image::load_from_memory(&png).expect("PNG decodes");
    assert_eq!((decoded.width(), decoded.height()), (2, 2));
}

#[test]
fn kitty_delete_removes_placements() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(KITTY_IMAGE);
    assert_eq!(core.image_placements().len(), 1);
    core.write_raw(b"\x1b_Ga=d,d=A\x1b\\");
    assert!(core.image_placements().is_empty());
}

#[test]
fn sixel_emits_atlas_placement() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1bPq#0;2;100;0;0#0~~\x1b\\");
    let placements = core.image_placements();
    assert_eq!(placements.len(), 1);
    assert!(placements[0].columns >= 1);
}

#[test]
fn placement_frame_is_stable_when_scrolled_into_history() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(KITTY_IMAGE);
    let (first, _) =
        next_cell_frame(&core, &CellEmitState::new("epoch", "stream"), true, None).unwrap();
    let first_row = first.image_placements.as_ref().unwrap()[0].row;
    for _ in 0..30 {
        core.write_raw(b"line\r\n");
    }
    let (second, _) =
        next_cell_frame(&core, &CellEmitState::new("epoch", "stream"), true, None).unwrap();
    let second_row = second.image_placements.as_ref().unwrap()[0].row;
    assert_eq!(first_row, second_row);
}

#[test]
fn placement_only_delta_is_reported_once() {
    let mut core = RioCore::new(80, 24);
    let state = CellEmitState::new("epoch", "stream");
    let (_, state) = next_cell_frame(&core, &state, true, None).unwrap();
    core.write_raw(KITTY_IMAGE);
    let (frame, state) = next_cell_frame(&core, &state, false, None).unwrap();
    assert!(frame.image_placements.is_some());
    let (frame, _) = next_cell_frame(&core, &state, false, None).unwrap();
    assert!(frame.image_placements.is_none());
}
