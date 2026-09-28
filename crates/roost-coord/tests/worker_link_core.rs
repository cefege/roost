//! Two properties the worker socket's core exists to hold: the close-code
//! table is a promise to the peer, and the queue preserves order and refuses at
//! its budget.
//!
//! Each of these was a real failure mode in the source transport, and both are
//! pure values here so neither needs a live socket to observe.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::worker_link::conn_types::{
    CLOSE_POLICY_VIOLATION, CLOSE_QUEUE_OVERFLOW, CLOSE_REAUTH_REQUIRED, CLOSE_REVOKED,
    SocketClose, SocketIdentity,
};
use roost_coord::worker_link::frame_queue::{
    FrameQueue, QueueRefusal, Queued, QueuedFrame, WORKER_FRAME_QUEUE_MAX_BYTES,
    WORKER_FRAME_QUEUE_MAX_FRAMES,
};

#[test]
fn every_close_the_transport_can_emit_carries_the_code_the_worker_reacts_to() {
    // The table is a promise: the worker backs off on 1009 and 1008, and
    // re-authenticates on 4001 and 4003. Sending the wrong code tells it to do
    // the wrong thing, which is why each row is asserted rather than trusted.
    assert_eq!(
        SocketClose::QueueOverflow.code(),
        Some(CLOSE_QUEUE_OVERFLOW)
    );
    assert_eq!(
        SocketClose::QueueOverflow.into_frame(),
        Some((1009, "worker queue overflow"))
    );
    assert_eq!(
        SocketClose::EventRateExceeded.code(),
        Some(CLOSE_POLICY_VIOLATION)
    );
    assert_eq!(
        SocketClose::EventRateExceeded.into_frame(),
        Some((1008, "worker event rate exceeded"))
    );
    assert_eq!(
        SocketClose::DedupeMismatch.code(),
        Some(CLOSE_POLICY_VIOLATION)
    );
    assert_eq!(SocketClose::Revoked.code(), Some(CLOSE_REVOKED));
    assert_eq!(SocketClose::Revoked.into_frame(), Some((4001, "revoked")));
    assert_eq!(
        SocketClose::ReauthRequired.code(),
        Some(CLOSE_REAUTH_REQUIRED)
    );
    assert_eq!(
        SocketClose::ReauthRequired.into_frame(),
        Some((4003, "reauth required"))
    );
}

#[test]
fn a_socket_that_merely_died_closes_with_no_code_at_all() {
    // The load-bearing half of the table. A durable append that threw, or a
    // route that vanished, is NOT a policy violation: the worker's correct
    // response is to reconnect and replay what was never acknowledged, and any
    // code here would tell it to back off or re-authenticate instead.
    assert_eq!(SocketClose::Default.code(), None);
    assert_eq!(SocketClose::Default.into_frame(), None);
    assert_ne!(SocketClose::Default.reason(), "");
}

#[test]
fn the_queue_hands_frames_back_in_arrival_order() {
    let mut queue = FrameQueue::with_bounds(8, 1024);
    for index in 0..5_u8 {
        assert!(matches!(
            queue.push(QueuedFrame::new(vec![index], 1)),
            Queued::Admitted { .. }
        ));
    }
    // The runtime dispatches in order and does not await its handlers, so this
    // is the only thing standing between two frames of one channel delta and
    // the out-of-order drop that discards the channel.
    let mut seen = Vec::new();
    while let Some(frame) = queue.take_front() {
        seen.push(frame.payload[0]);
        queue.release(&frame);
    }
    assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    assert_eq!(queue.charged_bytes(), 0, "every frame was released");
}

#[test]
fn the_byte_budget_stays_charged_until_the_handler_settles() {
    let mut queue = FrameQueue::with_bounds(8, 10);
    queue.push(QueuedFrame::new(vec![0; 6], 0));
    let taken = queue.take_front().expect("the frame was admitted");
    // Dequeued but not released: the handler is still running, and the socket is
    // still paying for it. A queue that forgot work on dequeue would admit
    // twice its bound while the first batch was in flight.
    assert_eq!(queue.charged_bytes(), 6);
    assert_eq!(
        queue.push(QueuedFrame::new(vec![0; 6], 0)),
        Queued::Refused(QueueRefusal::Overflow),
        "a dequeued-but-unsettled frame is still charged"
    );
    queue.release(&taken);
    assert_eq!(queue.charged_bytes(), 0, "the settled frame stops costing");
    // This queue is now LATCHED and stays that way — an overflow closes the
    // socket rather than letting it recover — so the "it fits again" half is
    // asked on a fresh queue, which is the only way a real socket ever gets
    // there.
    let mut fresh = FrameQueue::with_bounds(8, 10);
    fresh.push(QueuedFrame::new(vec![0; 6], 0));
    let settling = fresh.take_front().expect("the frame was admitted");
    assert_eq!(
        fresh.push(QueuedFrame::new(vec![0; 6], 0)),
        Queued::Refused(QueueRefusal::Overflow)
    );
    let mut after = FrameQueue::with_bounds(8, 10);
    after.push(QueuedFrame::new(vec![0; 6], 0));
    let done = after.take_front().expect("the frame was admitted");
    after.release(&done);
    assert!(matches!(
        after.push(QueuedFrame::new(vec![0; 6], 0)),
        Queued::Admitted { .. }
    ));
    drop(settling);
}

#[test]
fn an_overflow_latches_before_it_refuses_anything_else() {
    let mut queue = FrameQueue::with_bounds(2, 1024);
    queue.push(QueuedFrame::new(vec![1], 0));
    queue.push(QueuedFrame::new(vec![2], 0));
    assert_eq!(
        queue.push(QueuedFrame::new(vec![3], 0)),
        Queued::Refused(QueueRefusal::Overflow)
    );
    // Latched, and the latch is reported ONCE. A worker that keeps sending
    // during the close must not be able to keep charging, and the caller must
    // not log a line per refused frame — an overflow that floods the log hides
    // the one line that mattered.
    assert!(queue.is_latched());
    assert!(!queue.latch(), "the second latch reports nothing new");
    for _ in 0..10 {
        assert_eq!(
            queue.push(QueuedFrame::new(vec![4], 0)),
            Queued::Refused(QueueRefusal::Latched)
        );
    }
    assert_eq!(queue.depth(), 2, "nothing was charged after the latch");
}

#[test]
fn a_frame_that_would_charge_nothing_is_refused_rather_than_admitted() {
    let mut queue = FrameQueue::with_bounds(4, 1024);
    // An empty frame occupies a slot forever and charges nothing, so a peer
    // sending them could hold every slot without ever reaching the byte bound.
    assert_eq!(
        queue.push(QueuedFrame::new(Vec::new(), 0)),
        Queued::Refused(QueueRefusal::Overflow)
    );
    assert_eq!(queue.depth(), 0);
}

#[test]
fn the_contract_bounds_are_the_ones_the_queue_ships_with() {
    assert_eq!(WORKER_FRAME_QUEUE_MAX_FRAMES, 256);
    assert_eq!(WORKER_FRAME_QUEUE_MAX_BYTES, 16 * 1024 * 1024);
    let queue = FrameQueue::new();
    assert_eq!(queue.depth(), 0);
    assert!(!queue.is_latched());
    assert_eq!(queue.charged_bytes(), 0);
}

#[test]
fn a_socket_identity_carries_the_generation_it_was_verified_at() {
    // The generation is per-SOCKET, not per-worker: it is what a later frame is
    // checked against, and a socket admitted under a moved generation closes
    // rather than being served.
    let identity = SocketIdentity::new("ab12cd34".to_owned(), 7, "a machine".to_owned());
    assert_eq!(identity.key_generation, 7);
    assert_eq!(identity.fingerprint, "ab12cd34");
    assert_eq!(identity.label, "a machine");
}
