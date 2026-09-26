//! The stray reaper, the stillborn detector, and the channel allocator. Each
//! test names the incident or the invariant it pins, because these thresholds
//! are decisions rather than tunables and the reasoning is the only thing
//! stopping someone from "simplifying" them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use roost_worker::strays::{
    Birth, ChannelAllocator, DEAD_BIRTH_LIFETIME, RECENTLY_CLOSED_TTL, STRAY_STRIKES, Stillborn,
    StrayTracker, Verdict,
};

fn tracked(ids: &[u16]) -> HashMap<u16, ()> {
    ids.iter().map(|id| (*id, ())).collect()
}

/// WITHOUT THE REAPER, nothing kills a survivor. A deleted session's PTY stays
/// alive indefinitely and the coordinator's open rows drift below the live PTY
/// count — 12 rows against 88 processes was an observed state.
#[test]
fn an_untracked_channel_is_eventually_reaped() {
    let now = Instant::now();
    let mut tracker = StrayTracker::new();

    let first = tracker.sweep(&[7], &tracked(&[]), now);
    assert_eq!(
        first,
        vec![Verdict::Strike { channel_id: 7 }],
        "the first sweep only warns"
    );

    let second = tracker.sweep(&[7], &tracked(&[]), now + Duration::from_secs(60));
    assert_eq!(
        second,
        vec![Verdict::Reap { channel_id: 7 }],
        "the second sweep reaps it — two minutes untracked is not a live spawn"
    );
}

/// TWO STRIKES, NOT ONE, and the reason is specific: the worker's session table
/// trails the keeper's spawn by a beat, so a just-spawned channel is briefly in
/// the keeper and not yet tracked. One grace interval covers that; one strike
/// would kill a live spawn.
#[test]
fn a_just_spawned_channel_is_never_reaped_on_its_first_sighting() {
    let now = Instant::now();
    let mut tracker = StrayTracker::new();
    tracker.on_spawn(9);

    // The spawn has not yet landed in the worker's table, so the first sweep
    // does not see it as tracked.
    let first = tracker.sweep(&[9], &tracked(&[]), now);
    assert_eq!(
        first,
        vec![Verdict::Strike { channel_id: 9 }],
        "one strike, and the channel survives it"
    );
    assert_eq!(tracker.strikes(9), 1);
}

/// A channel that flaps in and out of the worker's view must never accumulate
/// to a kill. Strikes are reset the moment the worker's view catches up, so
/// only CONSECUTIVE untracked sweeps count.
#[test]
fn strikes_reset_when_the_workers_view_catches_up() {
    let now = Instant::now();
    let mut tracker = StrayTracker::new();

    tracker.sweep(&[4], &tracked(&[]), now);
    assert_eq!(tracker.strikes(4), 1);

    // The worker's view catches up for one sweep.
    tracker.sweep(&[4], &tracked(&[4]), now + Duration::from_secs(60));
    assert_eq!(
        tracker.strikes(4),
        0,
        "a tracked channel has no strikes at all"
    );

    // And losing it again starts from scratch.
    let again = tracker.sweep(&[4], &tracked(&[]), now + Duration::from_secs(120));
    assert_eq!(
        again,
        vec![Verdict::Strike { channel_id: 4 }],
        "it starts over"
    );
}

/// THE RESTART LOOP, proven 2026-06-23. A channel emits a few PTY bytes after
/// its record was deleted — the keeper is a separate process, so in-flight
/// frames arrive after the close. Those are benign, and counting them re-tripped
/// the degraded-keeper detector immediately after a reconcile, and the restart
/// loop SIGTERMed every live PTY on the machine.
#[test]
fn a_recently_closed_channels_tail_emissions_are_benign() {
    let now = Instant::now();
    let mut tracker = StrayTracker::new();
    tracker.on_session_closed(3, now);

    // Inside the window: not even a strike.
    let inside = tracker.sweep(&[3], &tracked(&[]), now);
    assert_eq!(
        inside,
        vec![Verdict::Keep],
        "a tail emission is not a stray"
    );
    assert_eq!(tracker.strikes(3), 0, "and it costs no strike either");

    // Even several sweeps' worth of sweeps inside one window stay benign.
    let mut elapsed = now;
    for _ in 0..10 {
        elapsed += Duration::from_millis(50);
        assert_eq!(
            tracker.sweep(&[3], &tracked(&[]), elapsed),
            vec![Verdict::Keep],
            "still inside the {RECENTLY_CLOSED_TTL:?} window"
        );
    }
}

/// Past the window it is a TRUE orphan — a degraded keeper driving a channel
/// nobody owns — and it does count.
#[test]
fn a_channel_still_emitting_past_the_window_is_a_real_orphan() {
    let now = Instant::now();
    let mut tracker = StrayTracker::new();
    tracker.on_session_closed(3, now);

    let after = now + RECENTLY_CLOSED_TTL + Duration::from_millis(1);
    assert_eq!(
        tracker.sweep(&[3], &tracked(&[]), after),
        vec![Verdict::Strike { channel_id: 3 }],
        "past the window the tail is a real orphan, not a race"
    );
}

/// A tracked channel is never a stray, however long it lives.
#[test]
fn a_tracked_channel_is_always_kept() {
    let now = Instant::now();
    let mut tracker = StrayTracker::new();
    for sweep in 0..20 {
        assert_eq!(
            tracker.sweep(&[1], &tracked(&[1]), now + Duration::from_secs(sweep * 60)),
            vec![Verdict::Keep]
        );
    }
    assert_eq!(tracker.strikes(1), 0);
}

/// Bookkeeping for channels the keeper no longer reports is dropped, so a
/// long-lived worker does not accumulate an entry per channel it ever had.
#[test]
fn bookkeeping_for_a_vanished_channel_is_dropped() {
    let now = Instant::now();
    let mut tracker = StrayTracker::new();
    tracker.sweep(&[5], &tracked(&[]), now);
    assert_eq!(tracker.strikes(5), 1);

    // The keeper no longer reports it, so there is nothing left to reap.
    tracker.sweep(&[], &tracked(&[]), now + Duration::from_secs(60));
    assert_eq!(tracker.strikes(5), 0, "and its bookkeeping went with it");
}

/// THE STILLBORN RULE. A child that exits within the lifetime having produced
/// NOTHING is a degraded keeper birthing a dead PTY.
#[test]
fn a_child_that_exits_at_once_producing_nothing_is_stillborn() {
    let now = Instant::now();
    let birth = Birth::new(now);
    assert_eq!(
        birth.verdict(now + Duration::from_millis(10)),
        Stillborn::Stillborn
    );
    assert!(
        birth
            .verdict(now + Duration::from_millis(10))
            .is_stillborn()
    );
}

/// The discriminator is producing output. A real shell prints a prompt before
/// it exits, so a fast `exit` is NOT stillborn — and counting it would restart
/// a perfectly healthy keeper.
#[test]
fn a_fast_child_that_printed_something_is_not_stillborn() {
    let now = Instant::now();
    let mut birth = Birth::new(now);
    birth.produced(1);
    assert_eq!(
        birth.verdict(now + Duration::from_millis(10)),
        Stillborn::ProducedOutput,
        "one byte of prompt is enough — a real shell prints before it exits"
    );
}

/// A child that lived past the lifetime is an ordinary exit, whatever it
/// printed.
#[test]
fn a_child_that_lived_long_enough_is_an_ordinary_exit() {
    let now = Instant::now();
    let mut birth = Birth::new(now);
    assert_eq!(
        birth.verdict(now + DEAD_BIRTH_LIFETIME),
        Stillborn::LivedLongEnough
    );
    birth.produced(4_096);
    assert_eq!(
        birth.verdict(now + DEAD_BIRTH_LIFETIME + Duration::from_secs(60)),
        Stillborn::LivedLongEnough,
        "and printing a great deal does not make it stillborn"
    );
}

/// The three verdicts are named apart so a log line says WHICH observation
/// applied, rather than only that the answer was "no".
#[test]
fn the_three_verdicts_are_distinguishable() {
    assert_ne!(Stillborn::Stillborn, Stillborn::ProducedOutput);
    assert_ne!(Stillborn::ProducedOutput, Stillborn::LivedLongEnough);
    assert!(!Stillborn::ProducedOutput.is_stillborn());
    assert!(!Stillborn::LivedLongEnough.is_stillborn());
}

/// THE COLLISION RULE. A keeper outlives the worker, so a fresh worker starts
/// its channel counter at one and collides with channels the OLD keeper still
/// holds. The counter must advance past the KEEPER's actual maximum — the
/// coordinator does not know about orphaned PTYs from an earlier generation.
#[test]
fn the_channel_counter_advances_past_what_the_keeper_really_holds() {
    let mut allocator = ChannelAllocator::new();
    assert_eq!(allocator.next(), 1);

    // The coordinator knows about channels 1 and 2. The keeper holds 1, 2 and
    // 88 — an orphan from an earlier generation that the coordinator has
    // forgotten.
    assert!(allocator.advance_past_keeper(&[1, 2, 88]));
    assert_eq!(
        allocator.next(),
        89,
        "a spawn must not pick an id the keeper already has, even one the \\
         coordinator no longer lists"
    );
}

/// The counter never goes BACKWARDS when the keeper reports less than the
/// worker already allocated — otherwise a reconcile would reissue an id that a
/// live session is holding.
#[test]
fn the_channel_counter_never_goes_backwards() {
    let mut allocator = ChannelAllocator::new();
    allocator.advance_past_keeper(&[10]);
    assert_eq!(allocator.next(), 11);

    assert!(
        !allocator.advance_past_keeper(&[3]),
        "a lower maximum does not move it"
    );
    assert_eq!(
        allocator.next(),
        11,
        "and a lower keeper report is not a rewind"
    );
}

/// An empty keeper report is not a reason to move anything.
#[test]
fn an_empty_keeper_report_moves_nothing() {
    let mut allocator = ChannelAllocator::new();
    allocator.advance_past_keeper(&[10]);
    assert!(!allocator.advance_past_keeper(&[]));
    assert_eq!(allocator.next(), 11);
}

/// Allocation hands out consecutive ids and stops rather than wrapping.
#[test]
fn allocation_hands_out_consecutive_ids_and_stops_at_the_ceiling() {
    let mut allocator = ChannelAllocator::new();
    assert_eq!(allocator.take(), Some(1));
    assert_eq!(allocator.take(), Some(2));
    assert_eq!(
        allocator.next(),
        3,
        "and the counter is where the next call starts"
    );

    // The LAST id is usable exactly once: a counter that refused to advance
    // would never hand it out and would silently cost a channel, and one that
    // saturated would hand it out forever.
    let mut exhausted = ChannelAllocator::new();
    exhausted.advance_past_keeper(&[u16::MAX - 1]);
    assert_eq!(exhausted.next(), u16::MAX);
    assert!(!exhausted.is_exhausted());
    assert_eq!(exhausted.take(), Some(u16::MAX), "the last id is usable");
    assert!(exhausted.is_exhausted());
    assert_eq!(
        exhausted.take(),
        None,
        "exactly once, then no id and no wraparound"
    );

    // A keeper holding the final id leaves nothing to hand out at all, and that
    // is said rather than wrapped to zero.
    let mut full = ChannelAllocator::new();
    assert!(full.advance_past_keeper(&[u16::MAX]));
    assert!(full.is_exhausted());
    assert_eq!(full.take(), None);
}

/// The reaper's two strikes are a DECISION, and the sweep interval is what makes
/// them two minutes rather than two seconds.
#[test]
fn the_strike_count_is_a_decision_about_the_spawn_window() {
    assert_eq!(
        STRAY_STRIKES, 2,
        "one grace interval covers the spawn race; two never kills a live spawn"
    );
}
