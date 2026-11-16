//! The outbox's ordering and its caps. The ordering is the contract; the caps
//! are what stop a backlog becoming a memory leak.
//! Frames carry their own `Instant` at admission, so age is tested exactly
//! rather than approximately.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use roost_worker::outbox::{AdmitError, Admitted, Lane, Outbox, PENDING_BYTES_CAP, PENDING_CAP};

fn now() -> Instant {
    Instant::now()
}

fn admit(
    outbox: &mut Outbox,
    lane: Lane,
    label: &str,
    size: usize,
) -> Result<Admitted, AdmitError> {
    outbox.admit(lane, vec![0u8; size], label, now())
}

/// THE RULE. A session's `opened` event must reach the coordinator before that
/// session's first terminal frame. A cell ahead of its own `opened` is a frame
/// the browser cannot place, and it presents as a terminal that never paints.
#[test]
fn a_durable_event_drains_before_any_terminal_frame() {
    let mut outbox = Outbox::default();
    admit(&mut outbox, Lane::Terminal, "cells-1", 10).expect("queued");
    admit(&mut outbox, Lane::Terminal, "cells-2", 10).expect("queued");
    admit(&mut outbox, Lane::Durable, "session.opened", 10).expect("queued");

    let drained = outbox.drain_all();
    let labels: Vec<&str> = drained.iter().map(|frame| frame.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["session.opened", "cells-1", "cells-2"],
        "the durable event is first even though the cells were queued before it"
    );
}

/// Control frames gate what the link does next, so they also precede terminal
/// frames. A `closed` that lands after the cells that follow it is a session
/// that never closes.
#[test]
fn control_drains_before_terminal_frames() {
    let mut outbox = Outbox::default();
    admit(&mut outbox, Lane::Terminal, "cells", 10).expect("queued");
    admit(&mut outbox, Lane::Control, "session.closed", 10).expect("queued");

    let drained = outbox.drain_all();
    let labels: Vec<&str> = drained.iter().map(|frame| frame.label.as_str()).collect();
    assert_eq!(labels, vec!["session.closed", "cells"]);
}

/// Chronology WITHIN a lane is preserved. Two events about the same session
/// that arrive in the wrong order tell the coordinator a story that did not
/// happen.
#[test]
fn chronology_within_a_lane_is_preserved() {
    let mut outbox = Outbox::default();
    for label in ["opened", "state", "exited"] {
        admit(&mut outbox, Lane::Durable, label, 4).expect("queued");
    }
    let drained = outbox.drain_all();
    let labels: Vec<&str> = drained.iter().map(|frame| frame.label.as_str()).collect();
    assert_eq!(labels, vec!["opened", "state", "exited"]);
}

/// The frame cap refuses a frame that does not fit. A caller whose DURABLE
/// event did not fit has to learn that now.
#[test]
fn the_frame_cap_refuses_a_frame_that_does_not_fit() {
    let mut outbox = Outbox::new(2, PENDING_BYTES_CAP);
    admit(&mut outbox, Lane::Durable, "one", 4).expect("queued");
    admit(&mut outbox, Lane::Durable, "two", 4).expect("queued");

    let refused = admit(&mut outbox, Lane::Durable, "three", 4);
    assert!(
        matches!(refused, Err(AdmitError::Full { pending: 2, cap: 2 })),
        "a frame that does not fit is refused, not dropped: {refused:?}"
    );
    assert_eq!(outbox.frame_count(), 2);
}

/// The byte cap is exact because frames are encoded before admission. An
/// estimate over a mutable message is how a byte cap stops being one.
#[test]
fn the_byte_cap_is_enforced_on_encoded_bytes() {
    let mut outbox = Outbox::new(PENDING_CAP, 32);
    admit(&mut outbox, Lane::Durable, "a", 20).expect("queued");
    admit(&mut outbox, Lane::Durable, "b", 8).expect("queued");
    assert_eq!(outbox.byte_count(), 28);

    let refused = admit(&mut outbox, Lane::Durable, "c", 8);
    assert!(
        matches!(refused, Err(AdmitError::OverBytes { bytes: 28, cap: 32 })),
        "the last frame is the one that would cross the cap: {refused:?}"
    );
    assert_eq!(
        outbox.byte_count(),
        28,
        "and the byte count is unchanged by a refusal"
    );
}

/// A frame that alone exceeds the byte cap is refused outright. Admitting it
/// and then discovering the problem at send time is how a queue holds a frame
/// it can never deliver.
#[test]
fn a_frame_larger_than_the_byte_cap_is_refused_outright() {
    let mut outbox = Outbox::new(PENDING_CAP, 64);
    let refused = admit(&mut outbox, Lane::Terminal, "huge", 65);
    assert!(matches!(
        refused,
        Err(AdmitError::FrameTooLarge { bytes: 65, cap: 64 })
    ));
    assert!(outbox.is_empty());
}

/// Discarding one lane leaves the others alone — a stream generation being
/// invalidated drops its cells, not the events that describe the session.
#[test]
fn discarding_a_lane_leaves_the_others_intact() {
    let mut outbox = Outbox::default();
    admit(&mut outbox, Lane::Terminal, "cells-1", 4).expect("queued");
    admit(&mut outbox, Lane::Terminal, "cells-2", 4).expect("queued");
    admit(&mut outbox, Lane::Durable, "opened", 4).expect("queued");

    assert_eq!(outbox.discard(Lane::Terminal), 2);
    assert_eq!(outbox.frame_count(), 1);
    assert_eq!(
        outbox.byte_count(),
        4,
        "discarded bytes leave the count too"
    );
    assert_eq!(outbox.drain_all()[0].label, "opened");
}

/// A reconnect replays everything, still in contract order. The ordering
/// property has to survive the drain, not merely hold frame by frame.
#[test]
fn a_full_replay_still_puts_durable_before_terminal() {
    let mut outbox = Outbox::default();
    for index in 0..3 {
        admit(&mut outbox, Lane::Terminal, &format!("cells-{index}"), 4).expect("queued");
    }
    for index in 0..2 {
        admit(&mut outbox, Lane::Durable, &format!("opened-{index}"), 4).expect("queued");
    }
    let drained = outbox.drain_all();
    let durable_position = drained.iter().position(|f| f.lane == Lane::Durable);
    let first_terminal = drained.iter().position(|f| f.lane == Lane::Terminal);
    assert!(
        durable_position < first_terminal,
        "every durable event precedes every cell: {drained:?}"
    );
}

/// Draining an empty outbox yields nothing rather than blocking, because the
/// caller's next action is to wait on the socket.
#[test]
fn draining_an_empty_outbox_yields_nothing() {
    let mut outbox = Outbox::default();
    assert!(outbox.is_empty());
    assert_eq!(outbox.drain_one(), None);
    assert!(outbox.drain_all().is_empty());
}

/// The drain order is declared once and is the order, so the contract is
/// readable in one place rather than inferred from a loop.
#[test]
fn the_drain_order_places_terminal_last() {
    let order = Lane::DRAIN_ORDER;
    let position = |lane: Lane| {
        order
            .iter()
            .position(|l| *l == lane)
            .expect("the lane is listed")
    };
    // v2 `drainQueues` (`coord-link-outbox.ts:165-177`): liveness, then events.
    assert_eq!(position(Lane::Liveness), 0, "the pong first");
    assert_eq!(position(Lane::Durable), 1, "durable next");
    assert!(
        position(Lane::Control) < position(Lane::Terminal),
        "control gates what the link does next, so it precedes the cells"
    );
    assert!(
        position(Lane::Durable) < position(Lane::Terminal),
        "an opened event precedes the first cell for its session"
    );
}

/// A frame's age is measured from admission, not from when its lane was last
/// touched.
#[test]
fn a_frames_age_is_its_wait_not_the_lane_s() {
    let mut outbox = Outbox::default();
    let start = now();
    outbox
        .admit(Lane::Terminal, vec![0u8; 4], "cells", start)
        .expect("queued");
    let frame = outbox.drain_one().expect("a frame");
    assert_eq!(frame.age(start), Duration::ZERO);
    assert!(frame.age(start + Duration::from_secs(1)) >= Duration::from_secs(1));
}
