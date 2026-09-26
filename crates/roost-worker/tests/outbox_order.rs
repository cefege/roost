//! The outbox's ordering and its caps. The ordering is the contract; the caps
//! are what stop a backlog becoming a memory leak.
//!
//! The clock is a parameter rather than a sleep, so the raw-metadata promotion
//! rule is tested exactly rather than approximately.

use std::time::{Duration, Instant};

use roost_worker::outbox::{
    AdmitError, Admitted, Lane, Outbox, PENDING_BYTES_CAP, PENDING_CAP, RAW_METADATA_MAX_AGE,
};

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

    let drained = outbox.drain_all(now());
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

    let drained = outbox.drain_all(now());
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
    let drained = outbox.drain_all(now());
    let labels: Vec<&str> = drained.iter().map(|frame| frame.label.as_str()).collect();
    assert_eq!(labels, vec!["opened", "state", "exited"]);
}

/// A raw-metadata frame that has waited past its age bound is PROMOTED ahead of
/// the cells, so a chatty terminal cannot starve the coordinator-side scanners
/// indefinitely. The clock is passed in, so this is exact rather than a race.
#[test]
fn a_stale_raw_frame_is_promoted_ahead_of_the_cells() {
    let mut outbox = Outbox::default();
    let start = now();
    outbox
        .admit(Lane::RawMetadata, vec![0u8; 4], "raw-old", start)
        .expect("queued");
    outbox
        .admit(Lane::Terminal, vec![0u8; 4], "cells", start)
        .expect("queued");

    // Before the age bound, the lane order stands and cells go first.
    let early = outbox
        .drain_one(start + RAW_METADATA_MAX_AGE / 2)
        .expect("a frame");
    assert_eq!(
        early.label, "cells",
        "a fresh raw frame does not jump the queue"
    );

    // After it, the raw frame is promoted even though its lane is behind
    // Terminal in the drain order.
    outbox
        .admit(Lane::RawMetadata, vec![0u8; 4], "raw-old", start)
        .expect("queued");
    outbox
        .admit(Lane::Terminal, vec![0u8; 4], "cells-2", start)
        .expect("queued");
    let promoted = outbox
        .drain_one(start + RAW_METADATA_MAX_AGE + Duration::from_millis(1))
        .expect("a frame");
    assert_eq!(
        promoted.label, "raw-old",
        "a stale raw frame is promoted over the cells"
    );
}

/// A raw frame younger than the bound is NOT promoted. Without this the
/// promotion would fire on every frame the moment it was queued, and the lane
/// order would be meaningless.
#[test]
fn a_fresh_raw_frame_does_not_jump_the_queue() {
    let mut outbox = Outbox::default();
    admit(&mut outbox, Lane::RawMetadata, "raw", 4).expect("queued");
    admit(&mut outbox, Lane::Terminal, "cells", 4).expect("queued");
    let first = outbox.drain_one(now()).expect("a frame");
    assert_eq!(first.label, "cells");
}

/// The frame cap refuses a lane that may not be dropped, and drops one that
/// may. A caller whose DURABLE event did not fit has to learn that now.
#[test]
fn the_frame_cap_refuses_a_durable_lane_and_sheds_a_droppable_one() {
    let mut outbox = Outbox::new(2, PENDING_BYTES_CAP);
    admit(&mut outbox, Lane::Durable, "one", 4).expect("queued");
    admit(&mut outbox, Lane::Durable, "two", 4).expect("queued");

    let refused = admit(&mut outbox, Lane::Durable, "three", 4);
    assert!(
        matches!(refused, Err(AdmitError::Full { pending: 2, cap: 2 })),
        "a durable frame that does not fit is refused, not dropped: {refused:?}"
    );

    let mut outbox = Outbox::new(2, PENDING_BYTES_CAP);
    admit(&mut outbox, Lane::RawMetadata, "raw-one", 4).expect("queued");
    admit(&mut outbox, Lane::RawMetadata, "raw-two", 4).expect("queued");
    assert!(matches!(
        admit(&mut outbox, Lane::RawMetadata, "raw-three", 4),
        Ok(Admitted::Queued)
    ));
    assert_eq!(
        outbox.lane_len(Lane::RawMetadata),
        2,
        "the cap holds for a droppable lane"
    );
}

/// A droppable lane sheds its OLDEST frame. The new frame is the one the caller
/// holds a reference to, and dropping it would make the caller's own
/// accounting a lie.
#[test]
fn a_shed_frame_is_the_oldest_not_the_newest() {
    let mut outbox = Outbox::new(2, PENDING_BYTES_CAP);
    admit(&mut outbox, Lane::RawMetadata, "oldest", 4).expect("queued");
    admit(&mut outbox, Lane::RawMetadata, "middle", 4).expect("queued");
    admit(&mut outbox, Lane::RawMetadata, "newest", 4).expect("queued");

    let drained = outbox.drain_all(now());
    let labels: Vec<&str> = drained.iter().map(|frame| frame.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["middle", "newest"],
        "the oldest went, not the one just offered"
    );
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

/// ONLY raw metadata may be dropped. Dropping a durable event loses a fact
/// about what happened; dropping a terminal frame loses the only description
/// of the screen.
#[test]
fn only_raw_metadata_is_droppable() {
    assert!(Lane::RawMetadata.is_droppable());
    assert!(!Lane::Durable.is_droppable());
    assert!(!Lane::Control.is_droppable());
    assert!(!Lane::Terminal.is_droppable());
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
    assert_eq!(outbox.drain_all(now())[0].label, "opened");
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
    let drained = outbox.drain_all(now());
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
    assert_eq!(outbox.drain_one(now()), None);
    assert!(outbox.drain_all(now()).is_empty());
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
    assert_eq!(position(Lane::Durable), 0, "durable first");
    assert!(
        position(Lane::Control) < position(Lane::Terminal),
        "control gates what the link does next, so it precedes the cells"
    );
    assert!(
        position(Lane::Durable) < position(Lane::Terminal),
        "an opened event precedes the first cell for its session"
    );
}

/// A frame's age is measured from admission, so promotion is a function of how
/// long it waited rather than of when the queue was last touched.
#[test]
fn a_frames_age_is_its_wait_not_the_lane_s() {
    let mut outbox = Outbox::default();
    let start = now();
    outbox
        .admit(Lane::RawMetadata, vec![0u8; 4], "raw", start)
        .expect("queued");
    let frame = outbox.drain_one(start).expect("a frame");
    assert_eq!(frame.age(start), Duration::ZERO);
    assert!(frame.age(start + Duration::from_secs(1)) >= Duration::from_secs(1));
}
