//! Semantic retention behind the announced-channel barrier: a fact parked after
//! cell loss belongs to exactly one session and lives at most 30 s, an early
//! fact at most 3 s, and every retention owner charges the one socket budget.
//!
//! Ports `apps/coord/tests/events/announced-channel-recovery-order.test.ts`
//! ("a pre-commit replacement recovery remains for its replacement session");
//! its three other cases interleave a recovery delivery with later facts, which
//! a synchronous drain on the socket's one task cannot do. Also ports the pure
//! halves of `apps/coord/tests/announce-barrier-handler.test.ts` ("a stale
//! recovery cannot absorb metadata after exact route replacement", and the two
//! socket-cap cases) over `FrameQueue`, whose budget the barrier shares.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod announced_support;

use announced_support::{Barrier, CHANNEL, SESSION, binary, commit_labels, metadata, now};
use roost_coord::worker_link::announced_types::{
    ANNOUNCED_CHANNEL_MAX_BYTES, EnqueueOutcome, SEMANTIC_METADATA_MAX_CHANNELS,
    SEMANTIC_METADATA_PREANNOUNCE_MAX, SEMANTIC_METADATA_RECOVERY_MAX,
};
use roost_coord::worker_link::frame_queue::{
    FrameQueue, QueueRefusal, Queued, QueuedFrame, WORKER_FRAME_QUEUE_MAX_BYTES,
    WORKER_FRAME_QUEUE_MAX_FRAMES,
};
use roost_coord::worker_link::retained_budget::BudgetOverflow;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use tokio::time::Instant;

const REPLACEMENT: &str = "00000000-0000-4000-8000-000000000719";

/// Park `title` as `session`'s recovery by overflowing its announced channel.
fn park(b: &mut Barrier, session: &str, title: &str) -> Instant {
    let parked_at = now();
    b.announce(session);
    b.enqueue(metadata(title, true, true, 1), 40);
    let flood = binary(1, "overflow");
    assert_eq!(
        b.enqueue(flood, ANNOUNCED_CHANNEL_MAX_BYTES),
        EnqueueOutcome::Dropped
    );
    parked_at
}

#[test]
fn a_pre_commit_replacement_recovery_remains_for_its_replacement_session() {
    // v2: "a pre-commit replacement recovery remains for its replacement session"
    let mut b = Barrier::new();
    park(&mut b, SESSION, "older");
    park(&mut b, REPLACEMENT, "replacement");

    let (committed, delivered) = commit_labels(&mut b, SESSION, true);
    assert!(!committed, "the replaced session's recovery is gone");
    assert!(delivered.is_empty());
    let (committed, delivered) = commit_labels(&mut b, REPLACEMENT, true);
    assert!(committed);
    assert_eq!(delivered, ["metadata:replacement"]);
}

#[test]
fn a_stale_recovery_cannot_absorb_metadata_after_exact_route_replacement() {
    // v2 (announce-barrier-handler): "a stale recovery cannot absorb metadata
    // after exact route replacement" — the durable index now maps the channel
    // to another session, so the old recovery is discarded and the new fact
    // is published rather than retained.
    let mut b = Barrier::new();
    park(&mut b, SESSION, "old-title");
    assert_eq!(b.barrier.stats().recovery_metadata, 1);

    let behind_recovery =
        b.barrier
            .reconcile_retained_metadata(CHANNEL, REPLACEMENT, &mut b.budget);

    assert!(!behind_recovery, "the new fact takes the ordinary path");
    assert_eq!(b.barrier.stats().recovery_metadata, 0);
    assert_eq!(
        b.budget.stats().frames,
        0,
        "the discarded fact gave its charge back"
    );
    assert!(
        !b.barrier
            .reconcile_retained_metadata(CHANNEL, SESSION, &mut b.budget),
        "nothing of the old session survives to absorb a later fact"
    );
}

#[test]
fn a_parked_recovery_lives_thirty_seconds_and_then_gives_its_charge_back() {
    // The 30 s recovery owner (`ANNOUNCED_SEMANTIC_METADATA_RECOVERY_MAX_MS`).
    let mut b = Barrier::new();
    let parked_at = park(&mut b, SESSION, "parked");
    let just_before =
        parked_at + SEMANTIC_METADATA_RECOVERY_MAX - std::time::Duration::from_millis(1);
    b.barrier.expire(just_before, &mut b.budget);
    assert_eq!(
        b.barrier.stats().recovery_metadata,
        1,
        "still inside its wait"
    );

    b.barrier
        .expire(now() + SEMANTIC_METADATA_RECOVERY_MAX, &mut b.budget);

    assert_eq!(b.barrier.stats().recovery_metadata, 0);
    assert_eq!(b.budget.stats().frames, 0);
    let (committed, delivered) = commit_labels(&mut b, SESSION, true);
    assert!(
        !committed && delivered.is_empty(),
        "an expired fact is never delivered"
    );
}

#[test]
fn an_early_fact_waits_three_seconds_for_its_announcement() {
    let mut b = Barrier::new();
    let retained_at = now();
    assert!(b.retain_early(metadata("early", true, false, 1), 40));
    assert!(b.barrier.next_deadline().is_some());
    b.barrier.expire(
        retained_at + SEMANTIC_METADATA_PREANNOUNCE_MAX - std::time::Duration::from_millis(1),
        &mut b.budget,
    );
    assert_eq!(b.barrier.stats().pre_announced_metadata, 1);

    b.barrier
        .expire(now() + SEMANTIC_METADATA_PREANNOUNCE_MAX, &mut b.budget);

    assert_eq!(b.barrier.stats().pre_announced_metadata, 0);
    b.announce(SESSION);
    let (_, delivered) = commit_labels(&mut b, SESSION, true);
    assert!(
        delivered.is_empty(),
        "the expired fact did not move into the announcement"
    );
}

#[test]
fn retention_holds_at_most_sixty_four_channels_and_refuses_a_non_compact_fact() {
    let mut b = Barrier::new();
    for channel in 0..SEMANTIC_METADATA_MAX_CHANNELS as u32 {
        let CoordWorkerUpstream::TerminalMetadata(fact) = metadata("t", true, false, 1) else {
            unreachable!();
        };
        assert!(b.barrier.retain_unannounced_metadata(
            100 + channel,
            &fact,
            40,
            now(),
            &mut b.budget
        ));
    }
    assert!(!b.retain_early(metadata("one too many", true, false, 1), 40));
    assert!(!b.retain_early(metadata("oversized", true, false, 1), 4 * 1024 + 1));
    assert_eq!(
        b.barrier.stats().pre_announced_metadata,
        SEMANTIC_METADATA_MAX_CHANNELS
    );
}

#[test]
fn the_socket_frame_cap_counts_the_in_flight_append_and_every_announced_channel() {
    // v2: "socket frame cap includes an in-flight append and every announced channel"
    let mut queue = FrameQueue::new();
    assert!(matches!(
        queue.push(QueuedFrame::new(vec![1; 8], 0)),
        Queued::Admitted { .. }
    ));
    let in_flight = queue.take_front().unwrap();
    let mut b = Barrier::new();
    for index in 0..WORKER_FRAME_QUEUE_MAX_FRAMES as u32 - 1 {
        let channel = 1_000 + index;
        b.barrier
            .announce(channel, SESSION, now(), queue.budget_mut());
        let frame = binary(u64::from(index) + 1, "x");
        let held = b
            .barrier
            .enqueue(channel, frame, 1, now(), queue.budget_mut());
        assert_eq!(held, EnqueueOutcome::Buffered);
    }
    assert_eq!(queue.budget().stats().frames, WORKER_FRAME_QUEUE_MAX_FRAMES);

    b.barrier
        .announce(2_000, SESSION, now(), queue.budget_mut());
    let refused = b
        .barrier
        .enqueue(2_000, binary(1, "x"), 1, now(), queue.budget_mut());

    assert_eq!(refused, EnqueueOutcome::Dropped);
    let overflow = BudgetOverflow {
        frames: WORKER_FRAME_QUEUE_MAX_FRAMES,
        bytes: 8 + WORKER_FRAME_QUEUE_MAX_FRAMES as u64 - 1,
        rejected_bytes: 1,
    };
    assert_eq!(
        queue.budget().overflow(),
        Some(overflow),
        "the 1009 decision"
    );
    assert_eq!(
        queue.push(QueuedFrame::new(vec![1], 0)),
        Queued::Refused(QueueRefusal::Latched),
        "the overflow latched every owner closed"
    );
    queue.release(&in_flight);
    assert_eq!(
        queue.budget().stats().frames,
        WORKER_FRAME_QUEUE_MAX_FRAMES - 1
    );
}

#[test]
fn the_socket_byte_cap_counts_the_in_flight_append_through_the_first_excess_byte() {
    // v2: "socket byte cap retains an in-flight append through the first excess byte"
    let mut queue = FrameQueue::new();
    let append_bytes = 64_u64;
    queue.push(QueuedFrame::new(vec![0; append_bytes as usize], 0));
    let mut b = Barrier::new();
    let mut remaining = WORKER_FRAME_QUEUE_MAX_BYTES as u64 - append_bytes;
    let mut channel = 3_000;
    while remaining > 0 {
        let bytes = remaining.min(ANNOUNCED_CHANNEL_MAX_BYTES);
        b.barrier
            .announce(channel, SESSION, now(), queue.budget_mut());
        let held = b
            .barrier
            .enqueue(channel, binary(1, "x"), bytes, now(), queue.budget_mut());
        assert_eq!(held, EnqueueOutcome::Buffered);
        remaining -= bytes;
        channel += 1;
    }
    assert_eq!(
        queue.budget().stats().bytes,
        WORKER_FRAME_QUEUE_MAX_BYTES as u64
    );

    b.barrier
        .announce(4_000, SESSION, now(), queue.budget_mut());
    let refused = b
        .barrier
        .enqueue(4_000, binary(1, "x"), 1, now(), queue.budget_mut());

    assert_eq!(refused, EnqueueOutcome::Dropped);
    assert!(queue.budget().overflow().is_some());
    assert_eq!(
        queue.budget().stats().bytes,
        WORKER_FRAME_QUEUE_MAX_BYTES as u64
    );
}
