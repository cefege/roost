//! Clearing history (`CSI 3J`) reframes. The core counts the cleared lines as
//! evicted, so the monotonic total holds still and a delta would leave the
//! client holding rows that no longer exist; only the eviction-versus-append
//! comparison in `next_cell_frame` sees it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{CellEmitState, RioCore, TerminalCore, next_cell_frame};

#[test]
fn clearing_history_reframes() {
    let mut core = RioCore::new(20, 10);
    for row in 0..30 {
        core.write(format!("line {row}\r\n").as_bytes());
    }
    let state = CellEmitState::new("epoch", "stream");
    let (first, state) = next_cell_frame(&core, &state, false, None).unwrap();
    assert!(first.full);
    assert!(
        core.scrollback_count() > 0,
        "history exists before the clear"
    );
    core.clear_dirty();

    core.write(b"\x1b[3J");
    assert_eq!(core.scrollback_count(), 0);
    let (cleared, _) = next_cell_frame(&core, &state, false, None).unwrap();
    assert!(cleared.full, "a history clear is a full frame");
    assert!(cleared.scrollback_rows.is_empty());
    assert_eq!(cleared.sb_base, cleared.scrollback_total);
}

#[test]
fn ordinary_scrolling_at_the_cap_stays_a_delta() {
    let mut core = RioCore::with_history(20, 5, 10);
    for row in 0..40 {
        core.write(format!("line {row}\r\n").as_bytes());
    }
    let state = CellEmitState::new("epoch", "stream");
    let (_, state) = next_cell_frame(&core, &state, false, None).unwrap();
    core.clear_dirty();

    core.write(b"one more\r\nand another\r\n");
    let (frame, _) = next_cell_frame(&core, &state, false, None).unwrap();
    assert!(!frame.full, "evicting exactly what was appended is a delta");
    assert_eq!(frame.scrollback_append.len(), 2);
}
