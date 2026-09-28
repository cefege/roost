//! The two rules every downstream reply is held to: the request budget v2
//! derives from the coordinator's RELATIVE `budget_ms`, and the connection fence
//! that keeps a reply computed for a superseded coordinator socket off the
//! replacement one (v2 `terminalBudget` and `isCurrent` in
//! `apps/worker/src/transport/coord-link-downstream.ts` /
//! `coord-link-direct-terminal.ts`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_worker::uplink::{
    RequestBudget, TERMINAL_REQUEST_BUDGET_CAP_MS, Uplink, channel,
    terminal_results::bounded_terminal_reason,
};

fn pong(ts: i64) -> CoordWorkerUpstream {
    CoordWorkerUpstream::Pong { ts, trace_id: None }
}

#[test]
fn a_budget_is_the_coordinators_relative_ms_counted_from_receipt() {
    let received = Instant::now();
    let budget = RequestBudget::from_budget_ms(8_000, received);
    assert_eq!(budget.remaining(received), Duration::from_millis(8_000));
    assert_eq!(
        budget.remaining(received + Duration::from_millis(3_000)),
        Duration::from_millis(5_000),
        "time spent inside the worker is charged against the same budget"
    );
    assert!(!budget.expired(received + Duration::from_millis(7_999)));
    assert!(budget.expired(received + Duration::from_millis(8_000)));
    assert_eq!(
        budget.remaining(received + Duration::from_secs(60)),
        Duration::ZERO,
        "an overrun budget saturates rather than wrapping"
    );
}

#[test]
fn a_zero_or_oversized_budget_is_the_cap() {
    let received = Instant::now();
    let cap = Duration::from_millis(u64::from(TERMINAL_REQUEST_BUDGET_CAP_MS));
    assert_eq!(TERMINAL_REQUEST_BUDGET_CAP_MS, 30_000);
    assert_eq!(
        RequestBudget::from_budget_ms(0, received).remaining(received),
        cap,
        "a coordinator that sent no budget gets the cap, never an unbounded wait"
    );
    assert_eq!(
        RequestBudget::from_budget_ms(u32::MAX, received).remaining(received),
        cap,
        "no request can park an admission slot past the cap"
    );
    assert_eq!(
        RequestBudget::from_budget_ms(TERMINAL_REQUEST_BUDGET_CAP_MS, received).remaining(received),
        cap
    );
}

/// The send-time half of the fence: an owner that finishes after a re-dial is
/// told its reply went nowhere.
#[test]
fn a_fenced_send_after_a_redial_is_refused() {
    let (uplink, mut receiver) = channel();
    let fence = uplink.fence();
    assert!(fence.is_current());
    receiver.advance();
    assert!(!fence.is_current());
    assert!(
        !uplink.send_fenced(&fence, pong(1)),
        "a reply for a superseded connection is refused at the sender"
    );
    assert!(receiver.try_recv().is_none());
}

/// The receive-time half: a reply sent while current, still in the channel
/// when the link re-dialled, never reaches the new connection.
#[test]
fn a_fenced_reply_in_flight_across_a_redial_is_dropped_at_admission() {
    let (uplink, mut receiver) = channel();
    let fence = uplink.fence();
    assert!(uplink.send_fenced(&fence, pong(1)));
    receiver.advance();
    assert!(
        receiver.try_recv().is_none(),
        "the link admits nothing fenced to a connection that is gone"
    );
}

#[test]
fn a_current_fenced_reply_and_an_unfenced_send_both_arrive() {
    let (uplink, mut receiver) = channel();
    let fence = uplink.fence();
    assert!(uplink.send_fenced(&fence, pong(1)));
    receiver.advance();
    assert!(
        uplink.send(pong(2)),
        "an unfenced send waits for whichever link is next"
    );
    let fresh = uplink.fence();
    assert!(uplink.send_fenced(&fresh, pong(3)));
    assert_eq!(receiver.try_recv(), Some(pong(2)));
    assert_eq!(receiver.try_recv(), Some(pong(3)));
    assert!(receiver.try_recv().is_none());
    assert_eq!(fresh.generation(), receiver.generation());
}

#[test]
fn a_detached_uplink_accepts_nothing() {
    let uplink = Uplink::detached();
    assert!(!uplink.send(pong(1)));
    assert!(!uplink.send_fenced(&uplink.fence(), pong(2)));
}

#[test]
fn a_reason_is_bounded_to_200_bytes_on_a_utf8_boundary() {
    assert_eq!(bounded_terminal_reason(None), "");
    assert_eq!(bounded_terminal_reason(Some("short")), "short");
    let exact = "a".repeat(200);
    assert_eq!(bounded_terminal_reason(Some(&exact)), exact);
    // 199 ASCII bytes then a 4-byte scalar: byte 200 falls inside it.
    let straddling = format!("{}🐙tail", "a".repeat(199));
    assert_eq!(bounded_terminal_reason(Some(&straddling)), "a".repeat(199));
    let emoji = "🐙".repeat(60);
    let bounded = bounded_terminal_reason(Some(&emoji));
    assert_eq!(bounded.len(), 200);
    assert_eq!(bounded, "🐙".repeat(50));
}
