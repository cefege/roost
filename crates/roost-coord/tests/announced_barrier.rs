//! The announced-channel barrier: terminal frames that arrive before their
//! `opened`/`respawned` route commits drain in their original channel order
//! after the binding, and every loss is reported with what it cost.
//!
//! Ports `apps/coord/tests/events/announced-channel-barrier.test.ts`; each
//! test names its v2 case. Not ported: "frames arriving during the drain join
//! the tail" — delivery here is a synchronous callback on the socket's one
//! task, so nothing can arrive mid-drain; its closing assertion (a committed
//! channel is open again) is the last test below.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod announced_support;

use announced_support::{
    Barrier, SESSION, binary, cell, chunk, commit_labels, label, metadata, now,
};
use roost_coord::worker_link::announced_types::{
    ANNOUNCED_CHANNEL_MAX_BYTES, ANNOUNCED_CHANNEL_MAX_FRAMES, ANNOUNCED_CHANNEL_MAX_WAIT,
    BarrierStats, ChannelPhase, DropReason, EnqueueOutcome,
};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

const MAX_BYTES: u64 = ANNOUNCED_CHANNEL_MAX_BYTES;

#[test]
fn a_respawns_binary_frames_and_its_cells_drain_in_arrival_order() {
    // v2: "a respawn's metadata binary frame and its cell frames drain in arrival order"
    let mut b = Barrier::new();
    assert_eq!(b.enqueue(cell(1, true), 100), EnqueueOutcome::NotAnnounced);
    b.announce(SESSION);
    assert_eq!(
        b.enqueue(binary(1, "\u{1b}]0;fresh-title\u{7}"), 60),
        EnqueueOutcome::Buffered
    );
    assert_eq!(b.enqueue(cell(10, true), 100), EnqueueOutcome::Buffered);
    assert_eq!(
        b.enqueue(binary(2, "prompt$ "), 40),
        EnqueueOutcome::Buffered
    );
    assert_eq!(b.enqueue(cell(11, false), 100), EnqueueOutcome::Buffered);
    let stats = b.barrier.stats();
    assert_eq!((stats.channels, stats.frames, stats.bytes), (1, 4, 300));
    assert_eq!((stats.pending, stats.draining), (1, 0));

    let (committed, delivered) = commit_labels(&mut b, SESSION, true);

    assert!(committed);
    assert_eq!(
        delivered,
        [
            "binary:\u{1b}]0;fresh-title\u{7}",
            "cell:10:full",
            "binary:prompt$ ",
            "cell:11:delta"
        ]
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
    // v2: "semantic metadata waits for the announced channel route"
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
    // v2: "early title and activity facts merge before the route announces"
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
    // v2: "a pre-bind snapshot chunk establishes the barrier baseline"
    let mut b = Barrier::new();
    b.announce(SESSION);
    assert_eq!(b.enqueue(chunk(10), 100), EnqueueOutcome::Buffered);
    assert_eq!(b.enqueue(cell(11, false), 100), EnqueueOutcome::Buffered);
    let (_, delivered) = commit_labels(&mut b, SESSION, true);
    assert_eq!(delivered, ["chunk:10", "cell:11:delta"]);
}

#[test]
fn coalesced_metadata_retains_its_original_channel_order() {
    // v2: "coalesced metadata retains its original channel order"
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
fn frame_count_overflow_reports_the_pending_loss_of_both_lanes() {
    // v2: "frame-count overflow reports the pending loss of both lanes"
    let mut b = Barrier::new();
    b.announce(SESSION);
    assert_eq!(b.enqueue(cell(10, true), 100), EnqueueOutcome::Buffered);
    for seq in 1..ANNOUNCED_CHANNEL_MAX_FRAMES as u64 {
        assert_eq!(b.enqueue(binary(seq, "ab"), 10), EnqueueOutcome::Buffered);
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
    assert_eq!(drop.cell_frames, 2);
    assert_eq!(drop.binary_frames, ANNOUNCED_CHANNEL_MAX_FRAMES - 1);
    assert_eq!(
        drop.binary_bytes,
        (ANNOUNCED_CHANNEL_MAX_FRAMES as u64 - 1) * 2
    );
    assert_eq!(b.budget.stats().frames, 0, "a drop releases what it held");
}

#[test]
fn byte_cap_overflow_drops_the_buffer_and_reports_the_dropped_pty_bytes() {
    // v2: "byte-cap overflow drops the buffer and reports the dropped PTY bytes"
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    b.enqueue(binary(1, "title-bytes"), 64);
    assert_eq!(
        b.enqueue(binary(2, "flood"), MAX_BYTES),
        EnqueueOutcome::Dropped
    );
    let drops = b.drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].reason, DropReason::Overflow);
    assert_eq!((drops[0].cell_frames, drops[0].binary_frames), (1, 2));
    assert_eq!(
        drops[0].binary_bytes,
        ("title-bytes".len() + "flood".len()) as u64
    );
}

#[test]
fn metadata_survives_an_overflowed_cell_barrier_until_the_route_commits() {
    // v2: "metadata survives an overflowed cell barrier until the route commits"
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(metadata("survives-overflow", true, true, 1), 40);
    b.enqueue(cell(10, true), 100);
    assert_eq!(
        b.enqueue(binary(1, "flood"), MAX_BYTES),
        EnqueueOutcome::Dropped
    );
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
    // v2: "matching recovery survives a same-session reannouncement"
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(metadata("reannounced", true, true, 1), 40);
    assert_eq!(
        b.enqueue(binary(1, "overflow"), MAX_BYTES),
        EnqueueOutcome::Dropped
    );
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
    // v2: "a timed-out cell barrier retains latest metadata through route binding"
    let mut b = Barrier::new();
    let announced_at = now();
    b.barrier.announce(7, SESSION, announced_at, &mut b.budget);
    b.enqueue(metadata("survives-timeout", true, true, 1), 40);
    b.enqueue(cell(10, true), 100);
    b.enqueue(binary(1, "osc8-link"), 32);

    b.barrier
        .expire(announced_at + ANNOUNCED_CHANNEL_MAX_WAIT, &mut b.budget);

    let drops = b.drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(
        (drops[0].reason, drops[0].phase),
        (DropReason::Timeout, ChannelPhase::Pending)
    );
    assert_eq!((drops[0].cell_frames, drops[0].binary_frames), (1, 1));
    assert_eq!(drops[0].binary_bytes, "osc8-link".len() as u64);
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
    // v2: "a delta before the channel's first full grid is an ordering loss"
    let mut b = Barrier::new();
    b.announce(SESSION);
    // A binary frame first is legitimate; a cell DELTA first is not.
    assert_eq!(b.enqueue(binary(1, "bytes"), 20), EnqueueOutcome::Buffered);
    assert_eq!(b.enqueue(cell(2, false), 100), EnqueueOutcome::Dropped);
    let drops = b.drops();
    assert_eq!(drops[0].reason, DropReason::OutOfOrder);
    assert_eq!((drops[0].cell_frames, drops[0].binary_frames), (1, 1));

    b.announce(SESSION);
    assert_eq!(b.enqueue(cell(10, true), 100), EnqueueOutcome::Buffered);
    assert_eq!(b.enqueue(cell(12, false), 100), EnqueueOutcome::Dropped);
    assert_eq!(b.drops()[1].reason, DropReason::OutOfOrder);
}

#[test]
fn commit_without_the_exact_binding_delivers_nothing() {
    // v2: "commit without the exact binding delivers nothing"
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    b.enqueue(binary(1, "bytes"), 20);
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
    // v2: "a replacement announcement reports the buffer it can never bind"
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
    // v2: the closing assertion of "frames arriving during the drain join the tail"
    let mut b = Barrier::new();
    b.announce(SESSION);
    b.enqueue(cell(10, true), 100);
    let (committed, _) = commit_labels(&mut b, SESSION, true);
    assert!(committed);
    assert_eq!(b.barrier.stats().channels, 0);
    assert_eq!(b.enqueue(cell(12, false), 40), EnqueueOutcome::NotAnnounced);
}
