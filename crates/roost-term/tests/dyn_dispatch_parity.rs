//! The emitter must not care HOW it is handed a core.
//!
//! `roost-term`'s free functions take `&dyn TerminalCore` so that the worker's
//! `SessionRecord`, which holds a `Box<dyn TerminalCore + Send>`, can reach
//! them at all. That was a widening of the parameter type, and a widening is
//! only safe if it changes nothing a caller can observe: the frame built from
//! a boxed core has to be byte-identical to the frame built from the concrete
//! one, or every delta and every full frame on the wire is at the mercy of
//! dispatch.
//!
//! This is the only test guarding that property, and it exists because the
//! widening was done in `f8ae1b44` to unblock a slice, at a moment when the
//! crate could not be compiled. It was written before the gate and run in it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{AlacrittyCore, CellEmitState, TerminalCore, next_cell_frame};

/// The feed a core is given, so both paths see the same bytes in the same order
/// and any difference in the frame is dispatch rather than input.
const FEED: &[u8] = b"roost dispatch parity\r\n$ echo one\r\none\r\n$ ";

fn concrete() -> AlacrittyCore {
    AlacrittyCore::new(80, 24)
}

fn boxed() -> Box<dyn TerminalCore + Send> {
    Box::new(AlacrittyCore::new(80, 24))
}

/// A frame built through `&dyn` equals the same frame built through the
/// concrete type.
///
/// The first frame is forced so both sides take the FULL branch rather than the
/// delta branch: this test is about the dispatch, and a forced full exercises
/// every cell the core holds. The delta path is covered by the same equality
/// on the second call below.
#[test]
fn a_frame_from_a_boxed_core_matches_one_from_the_concrete_core() {
    let mut concrete_core = concrete();
    let mut boxed_core = boxed();
    concrete_core.write(FEED);
    boxed_core.write(FEED);

    let (concrete_frame, _) = next_cell_frame(
        &concrete_core,
        &CellEmitState::new("epoch-1", "stream-1"),
        true,
        None,
    )
    .expect("the concrete core builds a full frame");
    let (boxed_frame, _) =
        next_cell_frame(&*boxed_core, &CellEmitState::new("epoch-1", "stream-1"), true, None)
            .expect("the boxed core builds the same full frame");

    assert_eq!(
        concrete_frame, boxed_frame,
        "dispatch changed the frame the wire would carry"
    );

    // And the delta branch, after the baseline, which is the path a live
    // session spends all its time on.
    concrete_core.write(b"more output\r\n");
    boxed_core.write(b"more output\r\n");
    let (concrete_delta, _) = next_cell_frame(
        &concrete_core,
        &CellEmitState::new("epoch-1", "stream-1"),
        true,
        None,
    )
    .expect("the concrete core builds a second full");
    let (boxed_delta, _) = next_cell_frame(
        &*boxed_core,
        &CellEmitState::new("epoch-1", "stream-1"),
        true,
        None,
    )
    .expect("the boxed core builds the same second full");

    assert_eq!(
        concrete_delta, boxed_delta,
        "dispatch changed the second frame the wire would carry"
    );
}

/// The advance must match too, not only the frame.
///
/// A box that produced the right bytes from the wrong state would pass the
/// comparison above on the first call and diverge on the second, so both sides
/// are advanced identically and asserted on the state as well as the frame.
#[test]
fn the_emit_state_advances_identically_through_a_box() {
    let mut concrete_core = concrete();
    let mut boxed_core = boxed();
    concrete_core.write(FEED);
    boxed_core.write(FEED);

    let start = CellEmitState::new("epoch-1", "stream-1");
    let (frame_a, state_a) =
        next_cell_frame(&concrete_core, &start, true, None).expect("concrete full");
    let (frame_b, state_b) = next_cell_frame(&*boxed_core, &start, true, None).expect("boxed full");

    assert_eq!(frame_a, frame_b);
    assert_eq!(
        state_a, state_b,
        "the same bytes advanced the emitter state differently through a box"
    );
}
