//! The Sync cumulative ACK window, and the two native-buffer decisions that sit
//! beside it.
//!
//! Covers: all three bounds and the close each produces, the acknowledgement
//! rules including the one that closes with 1008, the re-arming of the age
//! deadline, and the worker's durable-event rate window.
//!
//! The split from the announcement barrier is by file size, not by transport: the
//! two share the injected-clock discipline and nothing else.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::sync_ws::ack_window::{
    ACK_TIMEOUT_MS, AckWindow, BackpressureReason, INVALID_ACK_CLOSE_CODE, INVALID_ACK_REASON,
    MAX_UNACKED_BYTES, MAX_UNACKED_FRAMES, WindowClose, buffered_over_high_water,
    should_arm_recovery_timer,
};
use roost_coord::worker_link::rate_window::{
    DURABLE_EVENT_LIMIT, DURABLE_EVENT_WINDOW_MS, DurableEventWindow,
};

#[test]
fn a_disabled_window_sequences_nothing_and_can_never_close() {
    let mut window = AckWindow::new(false);
    for _ in 0..1000 {
        window
            .may_send(1024 * 1024, 0)
            .expect("a disabled window never refuses");
    }
    let sequence = window.record_sent(10, 0);
    assert_eq!(
        sequence, 0,
        "an unnegotiated socket carries no application sequence"
    );
    assert_eq!(window.stats(0).unacked_frames, 0);
}

#[test]
fn a_negotiated_window_numbers_from_one_and_tracks_what_is_unacknowledged() {
    let mut window = AckWindow::new(true);
    assert_eq!(window.next_sequence(), 1);
    assert_eq!(window.record_sent(100, 0), 1);
    assert_eq!(window.record_sent(200, 0), 2);
    let stats = window.stats(0);
    assert_eq!(stats.unacked_frames, 2);
    assert_eq!(stats.unacked_bytes, 300);
    assert_eq!(stats.oldest_age_ms, 0);
}

#[test]
fn the_frame_limit_closes_with_1013_at_the_frame_after_the_bound() {
    let mut window = AckWindow::new(true);
    for index in 0..MAX_UNACKED_FRAMES {
        window
            .may_send(1, 0)
            .unwrap_or_else(|error| panic!("frame {index} should fit: {error:?}"));
        window.record_sent(1, 0);
    }
    match window.may_send(1, 0) {
        Err(WindowClose::Backpressure(reason, _)) => {
            assert_eq!(reason, BackpressureReason::FrameLimit)
        }
        other => panic!("expected a frame-limit close, got {other:?}"),
    }
}

#[test]
fn the_byte_limit_closes_with_1013() {
    let mut window = AckWindow::new(true);
    let half = MAX_UNACKED_BYTES / 2;
    window.may_send(half, 0).expect("half fits");
    window.record_sent(half, 0);
    window.may_send(half, 0).expect("the other half still fits");
    window.record_sent(half, 0);
    match window.may_send(1, 0) {
        Err(WindowClose::Backpressure(reason, _)) => {
            assert_eq!(reason, BackpressureReason::ByteLimit)
        }
        other => panic!("expected a byte-limit close, got {other:?}"),
    }
}

#[test]
fn the_oldest_frame_waiting_past_the_deadline_closes_with_1013() {
    let mut window = AckWindow::new(true);
    window.record_sent(10, 1_000);
    assert!(!window.age_deadline_passed(1_000 + ACK_TIMEOUT_MS - 1));
    assert!(window.may_send(10, 1_000 + ACK_TIMEOUT_MS - 1).is_ok());
    assert!(window.age_deadline_passed(1_000 + ACK_TIMEOUT_MS));
    match window.may_send(10, 1_000 + ACK_TIMEOUT_MS) {
        Err(WindowClose::Backpressure(reason, _)) => {
            assert_eq!(reason, BackpressureReason::AgeLimit)
        }
        other => panic!("expected an age-limit close, got {other:?}"),
    }
}

#[test]
fn an_acknowledgement_above_the_last_sent_sequence_closes_with_1008() {
    // A client cannot have processed something never sent, and continuing would
    // mean the window's accounting no longer describes reality.
    let mut window = AckWindow::new(true);
    window.record_sent(10, 0);
    assert_eq!(window.apply_ack(99, 0), Err(WindowClose::InvalidAck));
    assert_eq!(INVALID_ACK_CLOSE_CODE, 1008);
    assert_eq!(INVALID_ACK_REASON, "invalid sync ack");
}

#[test]
fn a_stale_or_repeated_acknowledgement_is_harmless() {
    // A reordered or duplicated ack is normal on a socket that also carries
    // controls; closing on it would kill healthy clients.
    let mut window = AckWindow::new(true);
    window.record_sent(10, 0);
    window.record_sent(20, 0);
    assert_eq!(window.apply_ack(1, 0), Ok(1));
    assert_eq!(window.apply_ack(1, 0), Ok(0), "a repeat releases nothing");
    assert_eq!(
        window.apply_ack(0, 0),
        Ok(0),
        "a stale ack releases nothing"
    );
    assert_eq!(window.stats(0).unacked_frames, 1);
    assert_eq!(window.apply_ack(2, 0), Ok(1));
    assert_eq!(window.stats(0).unacked_frames, 0);
    assert_eq!(window.stats(0).unacked_bytes, 0);
}

#[test]
fn the_age_deadline_is_re_measured_from_the_new_oldest_after_an_acknowledgement() {
    // The window measures the CURRENT oldest, not the first one ever sent.
    let mut window = AckWindow::new(true);
    window.record_sent(10, 0);
    window.record_sent(10, ACK_TIMEOUT_MS / 2);
    assert!(window.age_deadline_passed(ACK_TIMEOUT_MS + 1));
    let _ = window.apply_ack(1, ACK_TIMEOUT_MS + 1);
    assert!(
        !window.age_deadline_passed(ACK_TIMEOUT_MS + 1),
        "after releasing the oldest, the next one has its own full window"
    );
}

#[test]
fn clearing_releases_the_whole_window_for_a_socket_teardown() {
    let mut window = AckWindow::new(true);
    window.record_sent(10, 0);
    window.record_sent(10, 0);
    window.clear();
    assert_eq!(window.next_sequence(), 1);
    assert_eq!(window.stats(0).unacked_frames, 0);
}

#[test]
fn the_native_buffer_high_water_is_a_separate_decision_from_the_application_window() {
    // The kernel buffer bounds bytes in the kernel; the application window bounds
    // frames the client has not confirmed. "Only a slow client" and "the kernel
    // buffer is full" are different problems with different fixes, and folding
    // them together loses the distinction.
    assert!(!buffered_over_high_water(100, 100));
    assert!(buffered_over_high_water(101, 100));
    // The timer arms once, not per send: arming per send would reset it on every
    // frame and the socket would never time out.
    assert!(should_arm_recovery_timer(true, false));
    assert!(!should_arm_recovery_timer(true, true));
    assert!(!should_arm_recovery_timer(false, false));
}

// ── the worker's durable-event window ──────────────────────────────────────

#[test]
fn the_first_six_hundred_durable_events_in_a_minute_are_admitted_and_the_next_is_not() {
    let mut window = DurableEventWindow::new();
    for index in 0..DURABLE_EVENT_LIMIT {
        window
            .admit(0)
            .unwrap_or_else(|breach| panic!("event {index} should be admitted: {breach:?}"));
    }
    let breach = window.admit(0).expect_err("the 601st is over the limit");
    assert_eq!(breach.limit, 600);
    assert_eq!(breach.window_ms, 60_000);
    assert_eq!(window.admitted(), 600);
}

#[test]
fn the_window_rolls_after_a_minute_so_a_reconnecting_worker_is_not_locked_out() {
    // A worker reconciling hundreds of sessions after a keeper restart bursts well
    // past 600, and the window is fixed rather than a token bucket precisely so
    // the burst is allowed and the sustained rate is not.
    let mut window = DurableEventWindow::new();
    for _ in 0..DURABLE_EVENT_LIMIT {
        window.admit(0).expect("inside the window");
    }
    assert!(window.admit(0).is_err());
    window
        .admit(DURABLE_EVENT_WINDOW_MS)
        .expect("a new window starts");
    assert_eq!(window.admitted(), 1);
}

#[test]
fn a_backwards_clock_rolls_the_window_rather_than_wedging_the_socket() {
    // Refusing to roll would leave the socket permanently exhausted, and a
    // permanently exhausted socket is worse than one wasted window.
    let mut window = DurableEventWindow::new();
    for _ in 0..DURABLE_EVENT_LIMIT {
        window.admit(10_000).expect("inside the window");
    }
    assert!(window.admit(10_000).is_err());
    window
        .admit(9_000)
        .expect("a backwards step rolls the window");
    assert_eq!(window.admitted(), 1);
}
