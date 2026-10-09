//! The live-delta row cap, which is the one decision in the emitter that looks
//! optional and is not.
//!
//! A delta's history append starts at the ring's retained floor. A client that
//! fell further behind than the ring's capacity would therefore receive a frame
//! with a hole in its history and no way to notice it. `next_cell_frame` caps
//! the live append and escalates to a viewport-only full instead, so no client
//! is ever more than `LIVE_DELTA_SCROLLBACK_ROWS_CAP` lines behind.
//!
//! This is the named test wave-gate row W2 mutates, and it is written as two
//! tests rather than one on purpose: the escalation must be bounded in BOTH
//! directions. A cap that never fired would splice a hole, and a cap that
//! always fired would turn every live emit into a full frame.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{
    CellEmitState, LIVE_DELTA_SCROLLBACK_ROWS_CAP, RioCore, TerminalCore, next_cell_frame,
};

/// Scroll `lines` fresh history rows into the core.
///
/// Each feed is a CR-LF pair so the caret returns to column zero between lines;
/// the count that reaches history is the newlines past the viewport height, and
/// the tests below measure what actually landed rather than trusting this.
fn scroll_lines(core: &mut RioCore, lines: usize) {
    for row in 0..lines {
        core.write(format!("history row {row}\r\n").as_bytes());
    }
}

/// A live emit that has run past the row cap is a FULL frame, and a
/// viewport-only one.
///
/// Viewport-only is the half that is easy to get wrong in the fix. The
/// escalation exists to stop a client falling behind, so a full frame that
/// carried the history it was escalating away from would defeat the cap while
/// still passing an `is_full` assertion.
#[test]
fn a_delta_past_the_row_cap_becomes_a_viewport_only_full() {
    let mut core = RioCore::new(80, 24);

    // The baseline the delta path is allowed to start from. Forced, because
    // the first frame of any stream is a full whatever the cap says.
    let (_, baseline) = next_cell_frame(
        &core,
        &CellEmitState::new("epoch-1", "stream-1"),
        true,
        None,
    )
    .expect("a forced first frame is always built");
    assert!(
        baseline.sent_full,
        "the forced frame established a baseline"
    );
    core.clear_dirty();

    // The viewport height is added so the caret's walk down the first screen
    // does not eat into the overflow the test is trying to create.
    let overflow = LIVE_DELTA_SCROLLBACK_ROWS_CAP as usize + 64;
    scroll_lines(&mut core, overflow + 24);

    // Stated as a precondition rather than assumed, so a change in how the
    // emulator accounts for scrolls cannot quietly turn this test into one
    // that never reaches the cap and therefore never asserts anything.
    let growth = core.scrollback_count() as u64 - baseline.last_scrollback_total;
    assert!(
        growth > LIVE_DELTA_SCROLLBACK_ROWS_CAP,
        "the feed must actually exceed the cap: grew {growth} rows against a cap of {LIVE_DELTA_SCROLLBACK_ROWS_CAP}"
    );

    // A large history budget is offered deliberately: the cap has to override
    // the caller's tail, or a caller that asks for generous history silently
    // walks straight past it.
    let (frame, next) = next_cell_frame(&core, &baseline, false, Some(4_096))
        .expect("the emitter answers past the cap");

    assert!(
        frame.full,
        "a live append past the cap of {LIVE_DELTA_SCROLLBACK_ROWS_CAP} rows must escalate to a full frame, \
         not a delta whose append starts at the retained floor and leaves a hole in the client's history"
    );
    assert_eq!(
        frame.base_seq, 0,
        "a full frame is a baseline, not an extension of the frame before it"
    );

    assert!(
        frame.scrollback_rows.is_empty(),
        "the capped full must be viewport-only: it carried {} history rows, which is exactly the append the cap exists to prevent",
        frame.scrollback_rows.len()
    );
    assert_eq!(
        frame.sb_base, frame.scrollback_total,
        "a viewport-only full anchors its base at the total, so there is no window to normalise and no rows to send"
    );
    assert!(
        frame.scrollback_append.is_empty(),
        "a full frame is not also an append"
    );
    assert_eq!(
        next.last_scrollback_total, frame.scrollback_total,
        "the client's cursor is re-anchored to the total, so the NEXT frame is a delta from here and not a second catch-up"
    );
}

/// A live emit inside the cap stays a delta.
///
/// The other direction, and the one a mutation that deletes the cap entirely
/// would not catch on its own: a grid that escalates on every emit is not a
/// grid with a cap, it is a grid that stopped doing deltas.
#[test]
fn a_delta_within_the_row_cap_stays_a_delta() {
    let mut core = RioCore::new(80, 24);

    let (_, baseline) = next_cell_frame(
        &core,
        &CellEmitState::new("epoch-1", "stream-1"),
        true,
        None,
    )
    .expect("a forced first frame is always built");
    core.clear_dirty();

    let growth = LIVE_DELTA_SCROLLBACK_ROWS_CAP as usize - 64;
    scroll_lines(&mut core, growth + 24);

    let appended = core.scrollback_count() as u64 - baseline.last_scrollback_total;
    assert!(
        appended > 0 && appended <= LIVE_DELTA_SCROLLBACK_ROWS_CAP,
        "the feed must land strictly inside the cap: appended {appended} rows against a cap of {LIVE_DELTA_SCROLLBACK_ROWS_CAP}"
    );

    let (frame, next) = next_cell_frame(&core, &baseline, false, Some(4_096))
        .expect("the emitter answers inside the cap");

    assert!(
        !frame.full,
        "a live append of {appended} rows is inside the cap of {LIVE_DELTA_SCROLLBACK_ROWS_CAP} and must stay a delta"
    );
    assert_eq!(
        frame.base_seq, baseline.seq,
        "a delta declares the frame it was computed against"
    );
    assert_eq!(
        frame.scrollback_append.len() as u64,
        appended,
        "the delta appends every row the client is missing, which is the fast path the cap is protecting"
    );
    assert_eq!(
        next.seq,
        baseline.seq + 1,
        "an exact successor, never a gap"
    );
}
