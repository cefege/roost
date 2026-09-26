//! The announced-channel barrier and the worker's durable-event window, with an
//! injected clock so nothing sleeps and nothing flakes.
//!
//! Covers: every refusal the barrier can produce and what each one costs, the
//! socket-wide retention budget it shares with the ordered frame queue, and the
//! socket-wide retention budget it shares with the ordered frame queue. The Sync
//! ACK window and the worker's 600-per-minute rate are in
//! `transport_windows_ack.rs`.
//!
//! These are rules a live socket cannot be asked about reliably -- a frame held
//! for 3 s needs a deterministic clock to test at all -- which is why they are
//! pure functions over `now_ms` rather than timers.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use roost_coord::worker_link::announced_barrier::AnnouncedBarrier;
use roost_coord::worker_link::announced_types::{
    ChannelDrop, ChannelPhase, CommitOutcome, DropReason, EnqueueOutcome, FrameLane, MAX_FRAMES,
    MAX_WAIT_MS, RetainedFrame, RetainedWorkBudget,
};

fn budget() -> RetainedWorkBudget {
    RetainedWorkBudget::new(256, 16 * 1024 * 1024)
}

fn cell(seq: u64, bytes: u64) -> RetainedFrame {
    RetainedFrame {
        lane: FrameLane::Cell { full: false, seq },
        encoded_bytes: bytes,
    }
}

fn full(seq: u64, bytes: u64) -> RetainedFrame {
    RetainedFrame {
        lane: FrameLane::Cell { full: true, seq },
        encoded_bytes: bytes,
    }
}

fn metadata(bytes: u64) -> RetainedFrame {
    RetainedFrame {
        lane: FrameLane::Metadata,
        encoded_bytes: bytes,
    }
}

/// Collects the drops a barrier reports, so a test can assert which channel died
/// and why without a terminal view hub.
///
/// The `Rc` is load-bearing and was a real bug: `RefCell<Vec<_>>` implements
/// `Clone` by cloning the *vector*, so a plain `drops.clone()` hands the callback
/// and the assertions two independent cells and every drop assertion reads an
/// empty list.
fn recorder() -> (Rc<RefCell<Vec<ChannelDrop>>>, impl FnMut(ChannelDrop)) {
    let drops: Rc<RefCell<Vec<ChannelDrop>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&drops);
    (drops, move |drop| {
        sink.borrow_mut().push(drop);
    })
}

// ── the announced-channel barrier ──────────────────────────────────────────

#[test]
fn a_frame_for_an_unannounced_channel_flows_the_ordinary_way() {
    let mut barrier = AnnouncedBarrier::new(budget());
    let mut report = |_drop: ChannelDrop| unreachable!("nothing is announced yet");
    assert_eq!(
        barrier.enqueue(7, cell(1, 100), &mut report),
        EnqueueOutcome::NotAnnounced
    );
}

#[test]
fn a_announced_channel_holds_its_frames_in_arrival_order_until_the_route_commits() {
    let mut barrier = AnnouncedBarrier::new(budget());
    let mut report = |_drop: ChannelDrop| unreachable!("nothing should drop");
    barrier.announce(7, "s-1", 0, &mut report);

    assert!(barrier.is_announced(7));
    assert_eq!(
        barrier.enqueue(7, full(1, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    assert_eq!(
        barrier.enqueue(7, cell(2, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    assert_eq!(
        barrier.enqueue(7, cell(3, 100), &mut report),
        EnqueueOutcome::Buffered
    );

    let stats = barrier.stats();
    assert_eq!(stats.channels, 1);
    assert_eq!(stats.pending, 1);
    assert_eq!(stats.draining, 0);
    assert_eq!(stats.frames, 3);
    assert_eq!(stats.bytes, 300);

    assert_eq!(
        barrier.commit(7, "s-1", true, &mut report),
        CommitOutcome::Drained { frames: 3 }
    );
    assert_eq!(
        barrier.stats().frames,
        0,
        "the budget is released on commit"
    );
    assert!(!barrier.is_announced(7));
}

#[test]
fn a_cell_sequence_gap_drops_the_channel_because_a_lost_frame_is_unrecoverable() {
    // The recipient's baseline is already wrong; waiting cannot fix it, and the
    // recipient's status frames and partial chunks never establish a baseline
    // (`protocol/spec/terminal-stream.md:27`).
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-1", 0, &mut report);
    assert_eq!(
        barrier.enqueue(7, full(1, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    // seq 3 skips 2.
    assert_eq!(
        barrier.enqueue(7, cell(3, 100), &mut report),
        EnqueueOutcome::Dropped
    );

    let recorded = drops.borrow();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].reason, DropReason::OutOfOrder);
    assert_eq!(recorded[0].channel_id, 7);
    assert_eq!(recorded[0].session_id, "s-1");
    assert_eq!(recorded[0].phase, ChannelPhase::Pending);
    assert!(
        !barrier.is_announced(7),
        "the channel is gone with its frames"
    );
}

#[test]
fn a_delta_before_any_full_is_a_gap_because_there_is_nothing_to_continue() {
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-1", 0, &mut report);
    assert_eq!(
        barrier.enqueue(7, cell(1, 100), &mut report),
        EnqueueOutcome::Dropped
    );
    assert_eq!(drops.borrow()[0].reason, DropReason::OutOfOrder);
}

#[test]
fn a_full_resets_the_run_so_the_next_delta_is_its_successor() {
    // A full after a gap is precisely the repair that makes a channel usable
    // again. Refusing it would leave the recipient permanently stuck rather than
    // momentarily wrong.
    let mut barrier = AnnouncedBarrier::new(budget());
    let (_drops, mut report) = recorder();
    barrier.announce(7, "s-1", 0, &mut report);
    assert_eq!(
        barrier.enqueue(7, full(900, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    assert_eq!(
        barrier.enqueue(7, cell(901, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    assert_eq!(
        barrier.enqueue(7, cell(903, 100), &mut report),
        EnqueueOutcome::Dropped
    );
}

#[test]
fn the_frame_cap_drops_the_channel_and_names_the_bound() {
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-1", 0, &mut report);
    for seq in 1..=MAX_FRAMES as u64 {
        assert_eq!(
            barrier.enqueue(7, full(seq, 1), &mut report),
            EnqueueOutcome::Buffered,
            "frame {seq}"
        );
    }
    assert_eq!(
        barrier.enqueue(7, full(MAX_FRAMES as u64 + 1, 1), &mut report),
        EnqueueOutcome::Dropped
    );
    assert_eq!(drops.borrow()[0].reason, DropReason::Overflow);
    assert_eq!(drops.borrow()[0].cell_frames, MAX_FRAMES);
}

#[test]
fn a_socket_wide_budget_refusal_drops_the_channel_rather_than_exceeding_the_budget() {
    // The budget is the SOCKET's, shared with the ordered frame queue, so one
    // worker opening many channels exhausts one budget rather than one each
    // (`apps/coord/src/workers/worker-ws-upgrade.ts:148`).
    let mut barrier = AnnouncedBarrier::new(RetainedWorkBudget::new(4, 16 * 1024 * 1024));
    let (drops, mut report) = recorder();
    barrier.announce(1, "s-1", 0, &mut report);
    barrier.announce(2, "s-2", 0, &mut report);
    for seq in 1..=2 {
        assert_eq!(
            barrier.enqueue(1, full(seq, 10), &mut report),
            EnqueueOutcome::Buffered
        );
    }
    for seq in 1..=2 {
        assert_eq!(
            barrier.enqueue(2, full(seq, 10), &mut report),
            EnqueueOutcome::Buffered
        );
    }
    // The fifth frame has nowhere to go.
    barrier.announce(3, "s-3", 0, &mut report);
    assert_eq!(
        barrier.enqueue(3, full(1, 10), &mut report),
        EnqueueOutcome::Dropped
    );
    assert_eq!(drops.borrow()[0].reason, DropReason::Overflow);
}

#[test]
fn a_re_announcement_supersedes_rather_than_merges() {
    // Merging would deliver the old channel's cells to a session that no longer
    // owns the channel.
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-old", 0, &mut report);
    assert_eq!(
        barrier.enqueue(7, full(1, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    barrier.announce(7, "s-new", 0, &mut report);
    assert_eq!(drops.borrow()[0].reason, DropReason::Superseded);
    assert_eq!(drops.borrow()[0].session_id, "s-old");
    // The new announcement starts empty.
    assert_eq!(barrier.stats().frames, 0);
    assert_eq!(
        barrier.commit(7, "s-new", true, &mut report),
        CommitOutcome::Drained { frames: 0 }
    );
}

#[test]
fn a_commit_naming_a_different_session_is_refused_without_dropping() {
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-1", 0, &mut report);
    assert_eq!(
        barrier.enqueue(7, full(1, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    assert_eq!(
        barrier.commit(7, "s-2", true, &mut report),
        CommitOutcome::SessionMismatch
    );
    assert!(
        drops.borrow().is_empty(),
        "a mismatch is not a drop; the channel is still open"
    );
    assert!(barrier.is_announced(7));
}

#[test]
fn a_durable_index_that_bound_a_different_session_drops_the_channel() {
    // This is the one refusal that means the durable state and the announcement
    // DISAGREE, which is exactly the case where delivering would bind cells to
    // the wrong session.
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-1", 0, &mut report);
    assert_eq!(
        barrier.enqueue(7, full(1, 100), &mut report),
        EnqueueOutcome::Buffered
    );
    assert_eq!(
        barrier.commit(7, "s-1", false, &mut report),
        CommitOutcome::MappingMismatch
    );
    assert_eq!(drops.borrow()[0].reason, DropReason::MappingMismatch);
}

#[test]
fn a_channel_that_waits_past_the_bound_is_reported_as_expired() {
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-1", 1_000, &mut report);
    assert!(barrier.expired(1_000 + MAX_WAIT_MS - 1).is_empty());
    let expired = barrier.expired(1_000 + MAX_WAIT_MS);
    assert_eq!(expired, vec![(7, "s-1".to_string())]);
    barrier.fail(7, DropReason::Timeout, &mut report);
    assert_eq!(drops.borrow()[0].reason, DropReason::Timeout);
    assert!(
        barrier.expired(1_000 + MAX_WAIT_MS * 10).is_empty(),
        "the channel is gone"
    );
}

#[test]
fn a_draining_channel_is_not_reported_as_expired() {
    // The deadline is about waiting for the route, and a committed channel is
    // already past it.
    let mut barrier = AnnouncedBarrier::new(budget());
    let mut report = |_drop: ChannelDrop| unreachable!();
    barrier.announce(7, "s-1", 0, &mut report);
    barrier.commit(7, "s-1", true, &mut report);
    assert!(barrier.expired(MAX_WAIT_MS * 2).is_empty());
}

#[test]
fn clearing_releases_every_channel_and_its_budget() {
    let mut budget = RetainedWorkBudget::new(256, 16 * 1024 * 1024);
    let mut barrier = AnnouncedBarrier::new(budget);
    budget = RetainedWorkBudget::new(256, 16 * 1024 * 1024);
    let _ = budget;
    let mut report = |_drop: ChannelDrop| unreachable!();
    for channel in 0..4u32 {
        barrier.announce(channel, "s", 0, &mut report);
        barrier.enqueue(channel, full(1, 1000), &mut report);
    }
    assert_eq!(barrier.stats().bytes, 4000);
    barrier.clear();
    assert_eq!(barrier.stats().channels, 0);
    assert_eq!(barrier.stats().bytes, 0);
}

#[test]
fn a_closed_budget_retains_nothing_further() {
    let mut budget = RetainedWorkBudget::new(4, 1024);
    budget.close();
    let mut barrier = AnnouncedBarrier::new(budget);
    let (drops, mut report) = recorder();
    barrier.announce(1, "s-1", 0, &mut report);
    assert_eq!(
        barrier.enqueue(1, full(1, 10), &mut report),
        EnqueueOutcome::Dropped
    );
    assert_eq!(drops.borrow()[0].reason, DropReason::Overflow);
}

#[test]
fn metadata_and_binary_frames_are_counted_separately_so_a_drop_says_what_was_lost() {
    let mut barrier = AnnouncedBarrier::new(budget());
    let (drops, mut report) = recorder();
    barrier.announce(7, "s-1", 0, &mut report);
    barrier.enqueue(
        7,
        RetainedFrame {
            lane: FrameLane::Binary { bytes: 4096 },
            encoded_bytes: 4096,
        },
        &mut report,
    );
    barrier.enqueue(7, metadata(64), &mut report);
    barrier.fail(7, DropReason::AppendFailed, &mut report);
    let recorded = &drops.borrow()[0];
    assert_eq!(recorded.binary_frames, 1);
    assert_eq!(recorded.binary_bytes, 4096);
    assert_eq!(recorded.metadata_frames, 1);
    assert_eq!(recorded.cell_frames, 0);
    assert_eq!(recorded.reason, DropReason::AppendFailed);
}
