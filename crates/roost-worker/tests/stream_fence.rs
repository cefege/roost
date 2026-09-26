//! Stream generation fencing and the synchronized-output hold.
//!
//! The fence is about a terminal painting cells from a generation that no
//! longer exists. The hold is about a terminal going permanently dark because a
//! stream opened a synchronized frame and never closed it. Both failures are
//! silent, which is why they get explicit tests rather than being left to the
//! integration suite.

use std::time::{Duration, Instant};

use roost_worker::stream_fence::{
    Fence, Generation, HoldAction, SYNC_OUTPUT_MAX_PENDING_ROWS, SYNC_OUTPUT_MAX_SILENT, Scheduled,
    SyncOutputHold,
};

fn work(channel_id: u16, generation: u64) -> Scheduled {
    Scheduled {
        channel_id,
        generation: Generation(generation),
        payload: b"cells".to_vec(),
        queued_at: Instant::now(),
    }
}

/// A channel's first stream is generation one.
#[test]
fn a_channel_starts_at_its_first_generation() {
    let mut fence = Fence::new();
    let first = fence.open(5);
    assert_eq!(
        fence.current(5),
        Some(first),
        "a channel's first generation is recorded"
    );
}

/// THE INVARIANT. A replaced stream's work must not be delivered: a terminal
/// that paints cells from a generation which no longer exists is showing output
/// that was superseded, and nothing downstream can tell it from real output.
#[test]
fn a_replaced_generations_work_is_dropped() {
    let mut fence = Fence::new();
    fence.open(1);
    let stale = work(1, 1);

    fence.replace(1);

    let accepted = fence.take_current(vec![stale]);
    assert!(
        accepted.is_empty(),
        "the old generation's cells are never delivered"
    );
    assert_eq!(fence.dropped_superseded(), 1, "and the drop is counted");
}

/// The replacement's own work IS delivered. A fence that dropped everything
/// would blank every terminal on every resize.
#[test]
fn the_replacement_generations_work_is_delivered() {
    let mut fence = Fence::new();
    fence.open(1);
    let replacement = fence.replace(1);

    let accepted = fence.take_current(vec![work(1, replacement.0)]);
    assert_eq!(accepted.len(), 1, "the new generation's cells go out");
    assert_eq!(
        fence.dropped_superseded(),
        0,
        "and nothing was dropped to deliver it"
    );
}

/// Generations advance monotonically, so "newer" is a comparison and not a
/// guess. Two replacements must not land on the same number.
#[test]
fn generations_advance_and_never_repeat() {
    let mut fence = Fence::new();
    fence.open(2);
    let mut seen = vec![fence.current(2).expect("opened")];
    for _ in 0..10 {
        seen.push(fence.replace(2));
    }
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        seen.len(),
        "every generation is distinct: {seen:?}"
    );
    assert!(
        seen.windows(2).all(|pair| pair[0] < pair[1]),
        "and they only go up"
    );
}

/// Per-channel independence: replacing one channel's stream must not fence
/// another's. A fence keyed on the worker rather than the channel would blank
/// every terminal whenever any one of them resized.
#[test]
fn generations_are_per_channel() {
    let mut fence = Fence::new();
    fence.open(1);
    let untouched = fence.open(2);
    let replacement = fence.replace(1);

    // Neither is stale: channel 1's replacement IS generation 2, and channel 2
    // was never touched and is still generation 1. A fence keyed on the worker
    // rather than the channel would drop both and blank every terminal.
    let accepted = fence.take_current(vec![work(1, replacement.0), work(2, untouched.0)]);
    assert_eq!(
        accepted.len(),
        2,
        "both are current: replacing one channel fences only that channel"
    );
    assert_eq!(fence.dropped_superseded(), 0, "and neither was dropped");
}

/// A closed channel's work is not wanted either. "No current generation" and
/// "stale generation" are different situations with the same answer, and both
/// are counted apart so a diagnostic can tell them.
#[test]
fn a_closed_channels_work_is_dropped() {
    let mut fence = Fence::new();
    let first = fence.open(3);
    let queued = work(3, first.0);

    fence.close(3);
    assert!(
        fence.take_current(vec![queued]).is_empty(),
        "a closed channel delivers nothing"
    );
    assert_eq!(fence.dropped_unknown_channel(), 1);
    assert_eq!(
        fence.dropped_superseded(),
        0,
        "and it is counted as an unknown channel, not as a superseded generation"
    );
}

/// Work is fenced at DELIVERY, not at scheduling. A caller that queued work
/// before the replacement still has it dropped rather than delivered late.
#[test]
fn the_fence_applies_at_delivery_not_at_scheduling() {
    let mut fence = Fence::new();
    let first = fence.open(1);
    let queued = vec![work(1, first.0)];
    // The replacement happens with the work already queued.
    fence.replace(1);
    assert!(fence.take_current(queued).is_empty());
}

/// A channel that is closed and REOPENED starts a fresh generation, and the
/// old one's work does not come back with it.
#[test]
fn a_reopened_channel_starts_a_new_generation() {
    let mut fence = Fence::new();
    let first = fence.open(1);
    let stale = work(1, first.0);
    fence.close(1);
    let reopened = fence.open(1);

    assert!(
        reopened > first,
        "a reopened channel does NOT restart where it left off, or work still queued from before the close becomes current again"
    );
    assert!(
        fence.take_current(vec![stale]).is_empty(),
        "and the work from before the close stays dead"
    );
    assert_eq!(
        fence.take_current(vec![work(1, reopened.0)]).len(),
        1,
        "while the reopened generation delivers"
    );
}

/// A mixed queue is filtered, not short-circuited: one stale item must not
/// discard the valid ones beside it.
#[test]
fn a_mixed_queue_delivers_only_what_is_current() {
    let mut fence = Fence::new();
    let first_one = fence.open(1);
    let first_two = fence.open(2);
    let replacement = fence.replace(1);

    // Channel 1 is at the replacement, so its original work is stale; channel 2
    // is untouched. One stale item must not discard the valid ones beside it.
    let accepted = fence.take_current(vec![
        work(1, first_one.0),
        work(2, first_two.0),
        work(1, replacement.0),
    ]);
    assert_eq!(accepted.len(), 2, "the current generations survive");
    assert!(
        accepted
            .iter()
            .any(|w| w.channel_id == 1 && w.generation == replacement)
    );
    assert!(accepted.iter().any(|w| w.channel_id == 2));
    assert_eq!(
        fence.dropped_superseded(),
        1,
        "and exactly the stale one was dropped"
    );
}

// -- the synchronized-output hold -----------------------------------------

/// No hold is open, so nothing is withheld.
#[test]
fn no_hold_means_no_withholding() {
    let hold = SyncOutputHold::new();
    assert!(!hold.is_open());
    assert_eq!(hold.action(Instant::now()), HoldAction::Release);
}

/// A synchronized frame withholds output while it is open. The application is
/// telling the renderer not to paint a half-drawn grid, and the renderer obeys.
#[test]
fn an_open_synchronized_frame_withholds_output() {
    let now = Instant::now();
    let mut hold = SyncOutputHold::new();
    hold.open(now);
    assert!(hold.is_open());
    assert_eq!(
        hold.action(now),
        HoldAction::Withhold,
        "inside the ceiling, withhold"
    );
    hold.note_pending(10);
    assert_eq!(
        hold.action(now),
        HoldAction::Withhold,
        "and a little pending work changes nothing"
    );
}

/// THE STUCK SHAPE, SILENT. A stream that opens a synchronized frame and never
/// closes it — a TUI killed mid-repaint, a truncated recording, a `printf` that
/// emitted only the opener — would otherwise withhold forever and the browser
/// goes dark while the core keeps parsing. Nothing re-evaluates such a hold,
/// because a silent hold produces no further chunks, so only the timer rescues
/// it.
#[test]
fn a_silent_stuck_frame_is_released_by_the_wall_ceiling() {
    let now = Instant::now();
    let mut hold = SyncOutputHold::new();
    hold.open(now);

    let just_inside = now + SYNC_OUTPUT_MAX_SILENT - Duration::from_millis(1);
    assert_eq!(
        hold.action(just_inside),
        HoldAction::Withhold,
        "just inside the ceiling"
    );

    let past = now + SYNC_OUTPUT_MAX_SILENT;
    assert_eq!(
        hold.action(past),
        HoldAction::Release,
        "past it the withheld frame ships and the stuck generation is bypassed"
    );
}

/// THE OTHER STUCK SHAPE, FLOODING. A hold that keeps taking rows is caught by
/// the work ceiling well before the wall clock fires, because a flood is
/// actively re-evaluating the hold and the timer is not the thing at risk.
#[test]
fn a_flooding_stuck_frame_is_released_by_the_work_ceiling() {
    let now = Instant::now();
    let mut hold = SyncOutputHold::new();
    hold.open(now);
    hold.note_pending(SYNC_OUTPUT_MAX_PENDING_ROWS - 1);
    assert_eq!(
        hold.action(now),
        HoldAction::Withhold,
        "just under the row ceiling"
    );

    hold.note_pending(1);
    assert_eq!(
        hold.action(now),
        HoldAction::Release,
        "at it, the hold is released even though not a millisecond has passed"
    );
}

/// The two ceilings are INDEPENDENT, which is the whole reason there are two.
/// Neither catches the shape the other exists for.
#[test]
fn the_two_ceilings_catch_different_shapes() {
    let now = Instant::now();

    // Silent: past the wall ceiling, but nowhere near the row ceiling.
    let mut silent = SyncOutputHold::new();
    silent.open(now);
    silent.note_pending(1);
    assert_eq!(
        silent.action(now + SYNC_OUTPUT_MAX_SILENT),
        HoldAction::Release
    );

    // Flooding: at the row ceiling, but nowhere near the wall ceiling.
    let mut flooding = SyncOutputHold::new();
    flooding.open(now);
    flooding.note_pending(SYNC_OUTPUT_MAX_PENDING_ROWS);
    assert_eq!(flooding.action(now), HoldAction::Release);
}

/// Closing the frame resumes immediately and clears the accounting, so a later
/// frame is measured from its own start rather than inheriting the last one's
/// backlog.
#[test]
fn closing_a_frame_resumes_and_clears_the_accounting() {
    let now = Instant::now();
    let mut hold = SyncOutputHold::new();
    hold.open(now);
    hold.note_pending(SYNC_OUTPUT_MAX_PENDING_ROWS - 1);
    hold.close();

    assert!(!hold.is_open());
    assert_eq!(hold.action(now), HoldAction::Release, "nothing is withheld");

    // The next frame starts clean.
    hold.open(now);
    assert_eq!(
        hold.action(now),
        HoldAction::Withhold,
        "and is measured from its own start"
    );
}

/// Output noted while no hold is open is not counted against one. A channel
// that was never synchronized must not open a frame already over its ceiling.
#[test]
fn output_noted_while_no_hold_is_open_is_not_counted() {
    let now = Instant::now();
    let mut hold = SyncOutputHold::new();
    hold.note_pending(SYNC_OUTPUT_MAX_PENDING_ROWS * 4);
    assert_eq!(
        hold.action(now),
        HoldAction::Release,
        "there is nothing to release"
    );

    hold.open(now);
    assert_eq!(
        hold.action(now),
        HoldAction::Withhold,
        "and a frame opened afterwards starts from zero, not from the backlog"
    );
}
