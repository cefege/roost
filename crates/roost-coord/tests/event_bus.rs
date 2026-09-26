//! The bus's three load-bearing properties: the bound, the absence of any
//! per-subscriber drop, and the isolation of one broken subscriber from the rest.
//!
//! Ported from the behaviour v2 pins in `apps/coord/src/events/buses.ts:82-112`
//! and its own test for the zero-capacity bus
//! (`apps/coord/tests/ui-state/ui-state-owner.test.ts:117-124`).
//!
//! The one that matters most is the second: a bus with a per-subscriber buffer
//! would move the "slow consumer" drop to a place with no backpressure signal,
//! and a terminal delta that goes missing there is history corruption that no log
//! line reports. The assertion is that a subscriber which does nothing at all
//! still receives every message.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use roost_coord::events::bus::BoundedBus;
use roost_coord::events::bus_domains::Buses;
use roost_coord::events::bus_messages::SessionBusMessage;
use roost_protocol::wire::{SessionEvent, SessionId};

/// A bus over plain integers, so a test can talk about counts.
///
/// The subscription comes back with the bus: a bus hands out RAII subscriptions,
/// so a helper that dropped it would return a bus nobody is listening to.
fn counting_bus(
    capacity: usize,
) -> (BoundedBus<u32>, Arc<Mutex<Vec<u32>>>, roost_coord::events::bus::Subscription<u32>) {
    let bus = BoundedBus::new(capacity);
    let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = bus.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(*message);
    });
    (bus, seen, subscription)
}

#[test]
fn the_ring_keeps_the_last_n_and_drops_the_oldest() {
    let (bus, _seen, _watching) = counting_bus(3);
    for value in 1..=5 {
        bus.publish(value);
    }
    assert_eq!(bus.retained_count(), 3, "the bound is the bound");
    assert_eq!(bus.capacity(), 3);
}

#[test]
fn a_zero_capacity_bus_retains_nothing_at_all() {
    // v2's own assertion: "UI bus retains no report or command payloads".
    let bus = BoundedBus::new(0);
    bus.publish(1_u32);
    assert_eq!(
        bus.retained_count(),
        0,
        "a volatile bus must hold no payload"
    );
}

#[test]
fn subscribing_does_not_replay_the_ring() {
    // "Does NOT replay the ring -- reconnect backfill goes through the events
    // table, not bus history." A subscriber that received the ring would see
    // duplicates it had no way to recognise.
    let bus = BoundedBus::new(8);
    bus.publish(1_u32);
    bus.publish(2_u32);
    let late: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&late);
    let _subscription = bus.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(*message);
    });
    assert!(
        late.lock().unwrap_or_else(PoisonError::into_inner).is_empty(),
        "a new subscriber received history it never asked for"
    );
    bus.publish(3_u32);
    assert_eq!(*late.lock().unwrap_or_else(PoisonError::into_inner), vec![3]);
}

#[test]
fn a_subscriber_that_does_nothing_still_receives_every_message() {
    // There is no per-subscriber queue and therefore no per-subscriber drop: a
    // consumer that cannot keep up is slow inside its own socket queue, which is
    // where the per-socket budget and its close code live.
    let bus = BoundedBus::new(2);
    let delivered = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&delivered);
    let _subscription = bus.subscribe(move |_message| {
        // A subscriber that reads nothing and keeps nothing. If the bus had a
        // per-subscriber buffer, this is where a delta would disappear.
        counter.fetch_add(1, Ordering::Relaxed);
    });
    for value in 0..64_u32 {
        bus.publish(value);
    }
    assert_eq!(delivered.load(Ordering::Relaxed), 64);
    assert_eq!(bus.retained_count(), 2, "the ring is still bounded");
}

#[test]
fn one_panicking_subscriber_cannot_cost_the_others_their_message() {
    let bus = BoundedBus::new(4);
    let _broken = bus.subscribe(|_message| panic!("a socket's frame builder threw"));
    let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _subscription = bus.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(*message);
    });

    bus.publish(7_u32);

    assert_eq!(
        *seen.lock().unwrap_or_else(PoisonError::into_inner),
        vec![7],
        "the publisher survived one broken listener and the rest still received"
    );
}

#[test]
fn dropping_the_subscription_unsubscribes() {
    let bus = BoundedBus::new(4);
    let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let subscription = bus.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(*message);
    });
    bus.publish(1_u32);
    assert_eq!(bus.subscriber_count(), 1);
    drop(subscription);
    assert_eq!(bus.subscriber_count(), 0);
    bus.publish(2_u32);
    assert_eq!(
        *seen.lock().unwrap_or_else(PoisonError::into_inner),
        vec![1],
        "an unsubscribed consumer receives nothing further"
    );
}

#[test]
fn the_thirteen_buses_carry_their_v2_bounds() {
    let buses = Buses::new();
    assert_eq!(buses.session_bus.capacity(), 256);
    assert_eq!(buses.workspace_bus.capacity(), 64);
    assert_eq!(buses.task_bus.capacity(), 64);
    assert_eq!(buses.mcp_bus.capacity(), 128);
    assert_eq!(buses.agent_status_bus.capacity(), 128);
    assert_eq!(buses.pair_bus.capacity(), 32);
    assert_eq!(buses.audit_bus.capacity(), 256);
    assert_eq!(buses.presence_bus.capacity(), 128);
    assert_eq!(buses.worker_routable_bus.capacity(), 64);
    assert_eq!(buses.global_presence_bus.capacity(), 64);
    assert_eq!(buses.title_bus.capacity(), 256);
    assert_eq!(buses.last_activity_bus.capacity(), 256);
    assert_eq!(
        buses.ui_bus.capacity(),
        0,
        "the UI bus is volatile and retains nothing"
    );
}

#[test]
fn every_bus_starts_with_no_subscribers() {
    let buses = Buses::new();
    assert_eq!(buses.session_bus.subscriber_count(), 0);
    assert_eq!(buses.ui_bus.subscriber_count(), 0);
}

#[test]
fn a_clone_is_another_handle_on_the_same_bus() {
    let (bus, seen, _watching) = counting_bus(4);
    let twin = bus.clone();
    let extra: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&extra);
    let _subscription = twin.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(*message);
    });
    bus.publish(5_u32);
    assert_eq!(*seen.lock().unwrap_or_else(PoisonError::into_inner), vec![5]);
    assert_eq!(*extra.lock().unwrap_or_else(PoisonError::into_inner), vec![5]);
    assert_eq!(
        bus.subscriber_count(),
        2,
        "one bus seen through two handles: the counting subscriber and this one"
    );
}

#[test]
fn the_session_bus_carries_the_committed_event_and_its_durable_id() {
    let buses = Buses::new();
    let seen: Arc<Mutex<Vec<SessionBusMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _subscription = buses.session_bus.subscribe(move |message| {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.clone());
    });
    let session =
        SessionId::try_from("00000000-0000-4000-8000-000000000001").expect("a UUID is a session id");
    let event = SessionEvent::Closed {
        session_id: session,
        exit_code: Some(0),
        ts: 9,
        trace_id: None,
    };

    buses.session_bus.publish(SessionBusMessage::committed(event, 42));

    let delivered = seen.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].event_id, Some(42));
    assert_eq!(delivered[0].event.kind_name(), "closed");
}
