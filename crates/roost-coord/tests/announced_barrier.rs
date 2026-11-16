//! The announced-channel barrier: terminal frames that arrive before their
//! `opened`/`respawned` route commits drain in their original channel order
//! after the binding, and every loss is reported with what it cost.
//!
//! Delivery here is a synchronous callback on the socket's one task, so no
//! frame can arrive mid-drain; the last test below pins the closing condition
//! (a committed channel is open again).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod announced_support;

use announced_support::{Barrier, SESSION, cell, chunk, commit_labels, label, metadata, now};
use roost_coord::worker_link::announced_types::{
    ANNOUNCED_CHANNEL_MAX_BYTES, ANNOUNCED_CHANNEL_MAX_FRAMES, ANNOUNCED_CHANNEL_MAX_WAIT,
    BarrierStats, ChannelPhase, DropReason, EnqueueOutcome,
};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

const MAX_BYTES: u64 = ANNOUNCED_CHANNEL_MAX_BYTES;

#[test]
fn a_respawns_metadata_and_its_cells_drain_in_arrival_order() {
    let mut b = Barrier::new();
    assert_eq!(b.enqueue(cell(1, true), 100), EnqueueOutcome::NotAnnounced);
    b.announce(SESSION);
    assert_eq!(
        b.enqueue(metadata("fresh-title", true, false, 1), 40),
        EnqueueOutcome::Buffered
    );
    assert_eq!(b.enqueue(cell(10, true), 100), EnqueueOutcome::Buffered);
    assert_eq!(b.enqueue(cell(11, false), 100), EnqueueOutcome::Buffered);
    let stats = b.barrier.stats();
    assert_eq!((stats.channels, stats.frames, stats.bytes), (1, 3, 240));
    assert_eq!((stats.pending, stats.draining), (1, 0));

    let (committed, delivered) = commit_labels(&mut b, SESSION, true);

    assert!(committed);
    assert_eq!(
        delivered,
        ["metadata:fresh-title", "cell:10:full", "cell:11:delta"]
    );
    assert_eq!(b.barrier.stats().channels, 0);
    assert_eq!(
        b.budget.stats().frames,
        0,
        "every held frame gave its charge back"
    );
    assert!(b.drops().is_empty());
}

#[test]
fn semantic_metadata_waits_for_the_announced_channel_route() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    assert_eq!(
        b.enqueue(metadata("fresh-title", true, true, 1), 40),
        EnqueueOutcome::Buffered
    );
    assert_eq!(b.enqueue(cell(10, true), 100), EnqueueOutcome::Buffered);
    let (_, delivered) = commit_labels(&mut b, SESSION, true);
    assert_eq!(delivered, ["metadata:fresh-title", "cell:10:full"]);
    assert!(b.drops().is_empty());
}

#[test]
fn early_title_and_activity_facts_merge_before_the_route_announces() {
    let mut b = Barrier::new();
    assert!(b.retain_early(metadata("early title", true, false, 11), 40));
    assert!(b.retain_early(metadata("", false, true, 22), 40));
    b.announce(SESSION);
    let mut delivered = Vec::new();
    let committed = b.commit(SESSION, true, &mut |frame| delivered.push(frame));
    assert!(committed);
    let [CoordWorkerUpstream::TerminalMetadata(merged)] = delivered.as_slice() else {
        panic!("exactly one merged metadata frame, got {delivered:?}");
    };
    assert_eq!(merged.title, "early title");
    assert!(merged.title_changed && merged.activity_changed);
    assert_eq!(merged.activity_ts_ms, 22);
}

#[test]
fn a_pre_bind_snapshot_chunk_establishes_the_barrier_baseline() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    assert_eq!(b.enqueue(chunk(10), 100), EnqueueOutcome::Buffered);
    assert_eq!(b.enqueue(cell(11, false), 100), EnqueueOutcome::Buffered);
    let (_, delivered) = commit_labels(&mut b, SESSION, true);
    assert_eq!(delivered, ["chunk:10", "cell:11:delta"]);
}

#[test]
fn coalesced_metadata_retains_its_original_channel_order() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(metadata("retained-title", true, false, 1), 40);
    b.enqueue(cell(10, true), 100);
    b.enqueue(metadata("", false, true, 1), 40);
    let mut delivered = Vec::new();
    b.commit(SESSION, true, &mut |frame| delivered.push(frame));
    let labels: Vec<String> = delivered.iter().map(label).collect();
    assert_eq!(labels, ["metadata:retained-title", "cell:10:full"]);
    let CoordWorkerUpstream::TerminalMetadata(merged) = &delivered[0] else {
        panic!("the first frame is the coalesced metadata");
    };
    assert!(merged.title_changed && merged.activity_changed);
    assert_eq!(merged.title, "retained-title");
}

#[test]
fn frame_count_overflow_reports_the_pending_loss() {
    // Frame-count overflow reports the pending loss of both lanes (one lane remains).
    let mut b = Barrier::new();
    b.announce(SESSION);
    assert_eq!(b.enqueue(cell(10, true), 100), EnqueueOutcome::Buffered);
    for seq in 1..ANNOUNCED_CHANNEL_MAX_FRAMES as u64 {
        assert_eq!(b.enqueue(chunk(seq), 10), EnqueueOutcome::Buffered);
    }
    assert_eq!(b.enqueue(cell(11, false), 10), EnqueueOutcome::Dropped);

    assert_eq!(b.barrier.stats().channels, 0);
    let drops = b.drops();
    let [drop] = drops.as_slice() else {
        panic!("one drop, got {drops:?}");
    };
    assert_eq!(
        (drop.reason, drop.phase),
        (DropReason::Overflow, ChannelPhase::Pending)
    );
    assert_eq!((drop.channel_id, drop.session_id.as_str()), (7, SESSION));
    // The refused frame is lost too, so it is counted with the buffer.
    assert_eq!(drop.cell_frames, ANNOUNCED_CHANNEL_MAX_FRAMES + 1);
    assert_eq!(b.budget.stats().frames, 0, "a drop releases what it held");
}

#[test]
fn byte_cap_overflow_drops_the_buffer_and_reports_the_dropped_frames() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    b.enqueue(chunk(11), 64);
    assert_eq!(b.enqueue(chunk(12), MAX_BYTES), EnqueueOutcome::Dropped);
    let drops = b.drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].reason, DropReason::Overflow);
    assert_eq!(drops[0].cell_frames, 3);
}

#[test]
fn metadata_survives_an_overflowed_cell_barrier_until_the_route_commits() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(metadata("survives-overflow", true, true, 1), 40);
    b.enqueue(cell(10, true), 100);
    assert_eq!(b.enqueue(chunk(11), MAX_BYTES), EnqueueOutcome::Dropped);
    let stats = b.barrier.stats();
    assert_eq!(
        (stats.channels, stats.frames, stats.recovery_metadata),
        (0, 1, 1)
    );

    let (committed, delivered) = commit_labels(&mut b, SESSION, true);
    assert!(committed);
    assert_eq!(delivered, ["metadata:survives-overflow"]);
    assert_eq!(b.barrier.stats().frames, 0);
    assert_eq!(b.budget.stats().frames, 0);
}

#[test]
fn matching_recovery_survives_a_same_session_reannouncement() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(metadata("reannounced", true, true, 1), 40);
    assert_eq!(b.enqueue(chunk(11), MAX_BYTES), EnqueueOutcome::Dropped);
    b.announce(SESSION);
    let stats = b.barrier.stats();
    assert_eq!(
        (stats.channels, stats.frames, stats.recovery_metadata),
        (1, 1, 0)
    );
    let (committed, delivered) = commit_labels(&mut b, SESSION, true);
    assert!(committed);
    assert_eq!(delivered, ["metadata:reannounced"]);
}

#[test]
fn a_timed_out_cell_barrier_retains_latest_metadata_through_route_binding() {
    let mut b = Barrier::new();
    let announced_at = now();
    b.barrier.announce(7, SESSION, announced_at, &mut b.budget);
    b.enqueue(metadata("survives-timeout", true, true, 1), 40);
    b.enqueue(cell(10, true), 100);
    b.enqueue(chunk(11), 32);

    b.barrier
        .expire(announced_at + ANNOUNCED_CHANNEL_MAX_WAIT, &mut b.budget);

    let drops = b.drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(
        (drops[0].reason, drops[0].phase),
        (DropReason::Timeout, ChannelPhase::Pending)
    );
    assert_eq!(drops[0].cell_frames, 2);
    let stats = b.barrier.stats();
    assert_eq!(
        (stats.channels, stats.frames, stats.recovery_metadata),
        (0, 1, 1)
    );
    let (committed, delivered) = commit_labels(&mut b, SESSION, true);
    assert!(committed);
    assert_eq!(delivered, ["metadata:survives-timeout"]);
}

#[test]
fn a_delta_before_the_channels_first_full_grid_is_an_ordering_loss() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    // A metadata frame first is legitimate; a cell DELTA first is not.
    assert_eq!(
        b.enqueue(metadata("title", true, false, 1), 20),
        EnqueueOutcome::Buffered
    );
    assert_eq!(b.enqueue(cell(2, false), 100), EnqueueOutcome::Dropped);
    let drops = b.drops();
    assert_eq!(drops[0].reason, DropReason::OutOfOrder);
    assert_eq!((drops[0].cell_frames, drops[0].metadata_frames), (1, 1));

    b.announce(SESSION);
    assert_eq!(b.enqueue(cell(10, true), 100), EnqueueOutcome::Buffered);
    assert_eq!(b.enqueue(cell(12, false), 100), EnqueueOutcome::Dropped);
    assert_eq!(b.drops()[1].reason, DropReason::OutOfOrder);
}

#[test]
fn commit_without_the_exact_binding_delivers_nothing() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    b.enqueue(chunk(11), 20);
    let (committed, delivered) = commit_labels(&mut b, SESSION, false);
    assert!(!committed);
    assert!(delivered.is_empty());
    assert_eq!(b.drops()[0].reason, DropReason::MappingMismatch);
    assert_eq!(b.barrier.stats().channels, 0);

    // A commit for another session never touches this channel's buffer.
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    let (committed, delivered) = commit_labels(&mut b, "other-session", true);
    assert!(!committed);
    assert!(delivered.is_empty());
    assert_eq!(b.barrier.stats().channels, 1);
}

#[test]
fn a_replacement_announcement_reports_the_buffer_it_can_never_bind() {
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    b.announce(SESSION);
    let drops = b.drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(
        (drops[0].reason, drops[0].cell_frames),
        (DropReason::Superseded, 1)
    );
    let expected = BarrierStats {
        channels: 1,
        pending: 1,
        ..BarrierStats::default()
    };
    assert_eq!(b.barrier.stats(), expected);
}

#[test]
fn a_committed_channel_is_open_and_its_next_frame_takes_the_fast_path() {
    // A committed channel is open again once its barrier has drained.
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    let (committed, _) = commit_labels(&mut b, SESSION, true);
    assert!(committed);
    assert_eq!(b.barrier.stats().channels, 0);
    assert_eq!(b.enqueue(cell(12, false), 40), EnqueueOutcome::NotAnnounced);
}
