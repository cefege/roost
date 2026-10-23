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
    RequestBudget, TERMINAL_REQUEST_BUDGET_CAP_MS, Uplink, UplinkAdmissionError, channel,
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
    assert!(receiver.try_recv_delivery().is_none());
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
        receiver.try_recv_delivery().is_none(),
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
    assert_eq!(
        receiver
            .try_recv_delivery()
            .and_then(|delivery| delivery._take_unconfirmed_frame()),
        Some(pong(2))
    );
    assert_eq!(
        receiver
            .try_recv_delivery()
            .and_then(|delivery| delivery._take_unconfirmed_frame()),
        Some(pong(3))
    );
    assert!(receiver.try_recv_delivery().is_none());
    assert_eq!(fresh.generation(), receiver.generation());
}

#[test]
fn a_detached_uplink_accepts_nothing() {
    let uplink = Uplink::detached();
    assert!(!uplink.send(pong(1)));
    assert!(!uplink.send_fenced(&uplink.fence(), pong(2)));
}
#[tokio::test]
async fn confirmed_admission_is_rejected_on_disconnect_and_fenced_from_redial() {
    let (uplink, mut receiver) = channel();
    receiver.link_opened();
    let pending_uplink = uplink.clone();
    let pending = tokio::spawn(async move { pending_uplink.send_confirmed(pong(4)).await });
    let old_delivery = tokio::time::timeout(Duration::from_secs(1), receiver.recv_delivery())
        .await
        .expect("the link receives the confirmed frame")
        .expect("the uplink remains open");

    receiver.link_detached();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("disconnect rejects the waiter without waiting for redial")
            .expect("the admission task remains running"),
        Err(UplinkAdmissionError)
    );
    receiver.link_opened();
    let mut old_frame_admitted = false;
    old_delivery.admit(|_| {
        old_frame_admitted = true;
        true
    });
    assert!(
        !old_frame_admitted,
        "a delivery from the lost connection cannot enter the replacement"
    );

    let new_uplink = uplink.clone();
    let confirmed = tokio::spawn(async move { new_uplink.send_confirmed(pong(5)).await });
    let new_delivery = tokio::time::timeout(Duration::from_secs(1), receiver.recv_delivery())
        .await
        .expect("the replacement link receives its frame")
        .expect("the uplink remains open");
    new_delivery.admit(|frame| matches!(frame, CoordWorkerUpstream::Pong { ts: 5, .. }));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), confirmed)
            .await
            .expect("the accepted frame wakes its sender")
            .expect("the admission task remains running")
            .is_ok()
    );
}

#[tokio::test]
async fn confirmed_admission_while_disconnected_fails_without_waiting_for_redial() {
    let (uplink, receiver) = channel();
    receiver.link_opened();
    receiver.link_detached();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), uplink.send_confirmed(pong(6)))
            .await
            .expect("a disconnected send does not wait for reconnect")
            .expect_err("a disconnected send is rejected without waiting for redial"),
        UplinkAdmissionError
    );
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
