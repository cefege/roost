//! Pins bounded viewport checkpoints across baseline and delta transitions.
//! PTY transcript and coordinator folds share this reconstruction primitive.
//! Ordinary terminal history retention remains covered by apply_delta tests.

use crate::cell::{CellGridFrame, CellRow, CellSpan, MouseTracking, apply_viewport_frame};
use std::sync::Arc;

#[test]
fn checkpoint_keeps_only_viewport_through_full_replacement_and_deltas() {
    let mut grid: Option<CellGridFrame> = None;
    assert_eq!(apply_viewport_frame(&mut grid, &frame(false, 1, 2)), None);
    assert!(grid.is_none(), "a delta cannot invent its missing baseline");
    let mut full = frame(true, 0, 1);
    full.viewport_rows[0] = row(0, "initial");
    full.scrollback_rows = vec![row(0, "initial-history")];
    full.scrollback_total = 1;
    assert_eq!(apply_viewport_frame(&mut grid, &full), Some(()));
    assert_viewport_checkpoint(grid.as_ref().unwrap(), 1, "initial");

    let mut replacement = frame(true, 0, 2);
    replacement.viewport_rows[0] = row(0, "replacement");
    replacement.scrollback_rows = vec![row(0, "replacement-history")];
    replacement.scrollback_total = 2;
    assert_eq!(apply_viewport_frame(&mut grid, &replacement), Some(()));
    assert_viewport_checkpoint(grid.as_ref().unwrap(), 2, "replacement");

    let before_invalid = grid.clone();
    assert_eq!(
        apply_viewport_frame(&mut grid, &frame(false, 99, 100)),
        None
    );
    assert_eq!(
        grid, before_invalid,
        "a broken chain cannot alter a checkpoint"
    );

    for seq in 3..103 {
        let mut delta = frame(false, seq - 1, seq);
        delta.viewport_rows = vec![row(0, &format!("viewport-{seq}"))];
        delta.scrollback_append = vec![row(seq as u32, &format!("history-{seq}"))];
        delta.scrollback_total = seq - 1;
        assert_eq!(apply_viewport_frame(&mut grid, &delta), Some(()));
        assert_viewport_checkpoint(grid.as_ref().unwrap(), seq, &format!("viewport-{seq}"));
    }
}

fn assert_viewport_checkpoint(frame: &CellGridFrame, seq: u64, text: &str) {
    assert_eq!(frame.seq, seq);
    assert_eq!(frame.viewport_rows[0].spans[0].text, text);
    assert!(frame.scrollback_rows.is_empty());
    assert!(frame.scrollback_append.is_empty());
}

fn row(index: u32, text: &str) -> CellRow {
    CellRow {
        index,
        spans: Arc::from([CellSpan {
            text: text.to_owned(),
            fg: 0,
            bg: 0,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: text.len() as u32,
            link_uri: None,
            link_key: None,
        }]),
        mark: 0,
    }
}

fn frame(full: bool, base_seq: u64, seq: u64) -> CellGridFrame {
    let row = |index| CellRow {
        index,
        spans: Arc::from([]),
        mark: 0,
    };
    CellGridFrame {
        stream_id: "tool-stream".into(),
        grid_epoch: "tool-epoch".into(),
        cols: 80,
        rows: 24,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        kitty_keyboard_flags: 0,
        full,
        viewport_rows: if full {
            (0..24).map(row).collect()
        } else {
            vec![row(0)]
        },
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq,
        seq,
        image_placements: None,
    }
}
