//! `BoundedBus<T>`: in-process publish/subscribe with a bounded replay ring.
//! One bus per domain, injected rather than global, and the ring is a
//! diagnostic seam rather than a delivery buffer.
//!
//! Ported from `apps/coord/src/events/buses.ts:82-112`. Three properties of the
//! original are load-bearing, and each is a rule a caller can get wrong:
//!
//! 1. **Subscribing does not replay the ring.** "Does NOT replay the ring --
//!    reconnect backfill goes through the events table, not bus history"
//!    (`buses.ts:103-104`). A subscriber that received the ring would see
//!    duplicates it had no way to recognise.
//! 2. **There is no per-subscriber queue, and therefore no per-subscriber drop.**
//!    `publish` calls every listener and forgets it. A consumer that cannot keep
//!    up is slow *inside its own socket queue*, which is where the per-socket
//!    frame budget and the close code that enforces it live
//!    (`docs/phase3-coord-contract.md` §7.4, §8.3) -- never here. A port that
//!    added a per-subscriber buffer would move the drop to a place with no
//!    backpressure signal, which is precisely how a terminal delta goes missing
//!    while the log still says it was published.
//! 3. **A throwing listener cannot kill the publisher.** v2 wraps each call and
//!    emits `bus.listener_throw` (`buses.ts:98`); here a panic is caught per
//!    listener so the remaining subscribers still receive the message.
//!
//! The ring's own policy is unchanged: push, then evict the **oldest** once it
//! exceeds the bound. Nothing reads the ring for delivery, so an eviction cannot
//! lose a delta -- which is what makes a bound of zero (`uiBus`) safe rather
//! than lossy, and what makes a large one harmless.
//!
//! ONE DELIBERATE DIFFERENCE, AND ITS VISIBLE CONSEQUENCE. The listener list is
//! snapshotted under the lock and the callbacks run after it is released, where v2
//! held its (single-threaded, lock-free) set for the whole loop. Holding a mutex
//! across a subscriber callback is a lock-order cycle waiting for the first hub
//! that fans out into another bus -- and `agent-status-hub` subscribes to
//! `sessionBus` and publishes to `agentStatusBus` today, so that cycle is a fact
//! about the existing call graph rather than a hypothetical.
//!
//! **The visible consequence, which is not a bug:** a listener that unsubscribes
//! *during* a fan-out still receives that one message, where v2's live `Set`
//! iteration would have skipped it. Every other ordering is unchanged -- the ring
//! is updated before any callback runs, and the callbacks run in subscription
//! order. Read this as "one message late for a listener that leaves mid-publish",
//! not as "a subscriber may receive events after it unsubscribed": the
//! subscription is gone before the next `publish`, which `tests/event_bus.rs`
//! pins.

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_observability::LogFields;
use roost_observability::log::error as log_error;

/// One subscriber. Called with a borrow of the message the bus owns for the
/// duration of the publish, which is what v2 handed every listener: one object,
/// N readers, no copy. A listener that keeps the message clones it. The `Arc` is
/// what lets `publish` take the listener list out of the lock and still call it.
type Listener<T> = Arc<dyn Fn(&T) + Send + Sync>;
/// `rg bus.listener_throw` finds the port and the original.
const LISTENER_THREW: &str = "bus.listener_throw";

/// The log target the coordinator's event core reports under.
const BUS_TARGET: &str = "events.bus";

/// A broadcast bus with a bounded replay ring.
///
/// A clone is another handle on the same bus, which is what makes `Arc<Buses>` in
/// the services struct the right way to hand thirteen of them to a transport.
pub struct BoundedBus<T> {
    inner: Arc<BusInner<T>>,
}

impl<T> Clone for BoundedBus<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct BusInner<T> {
    capacity: usize,
    state: Mutex<BusState<T>>,
}

struct BusState<T> {
    ring: VecDeque<Arc<T>>,
    listeners: Vec<BusListener<T>>,
    next_id: u64,
}

struct BusListener<T> {
    id: u64,
    call: Listener<T>,
}

impl<T> BoundedBus<T> {
    /// A bus that retains the last `capacity` messages for diagnostics.
    ///
    /// The bound is explicit at every call site rather than defaulted: v2's
    /// constructor defaulted to 64 and all thirteen of its buses passed a value,
    /// so the default was never a real answer. Zero retains nothing, which is
    /// the correct setting for a bus that must never hold a payload
    /// ([`crate::events::bus_domains::Buses::ui_bus`]).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(BusInner {
                capacity,
                state: Mutex::new(BusState {
                    ring: VecDeque::new(),
                    listeners: Vec::new(),
                    next_id: 0,
                }),
            }),
        }
    }

    /// The bound this bus was built with.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Retain the message, then hand it to every current subscriber.
    ///
    /// The listener loop is outside the lock on purpose; see the module header.
    /// A listener that panics is reported and skipped, and the remaining
    /// subscribers still receive the message -- one broken socket's frame
    /// builder must not cost every other browser its event.
    pub fn publish(&self, message: T) {
        let shared = Arc::new(message);
        let listeners = {
            let mut state = self.lock();
            state.ring.push_back(Arc::clone(&shared));
            while state.ring.len() > self.inner.capacity {
                state.ring.pop_front();
            }
            state
                .listeners
                .iter()
                .map(|listener| Arc::clone(&listener.call))
                .collect::<Vec<_>>()
        };
        for call in listeners {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| call(&shared))) {
                log_error(
                    BUS_TARGET,
                    LISTENER_THREW,
                    LogFields::new().set("error", panic_message(payload.as_ref())),
                );
            }
        }
    }

    /// Subscribe, and get the handle that ends the subscription.
    ///
    /// The subscription ends when the returned handle is dropped, which is what
    /// v2's returned unsubscribe function did and what a `finally` block spelled
    /// there. Dropping it is the only way to unsubscribe; there is no `unsubscribe`
    /// method, because a forgotten explicit call is the leak this avoids.
    pub fn subscribe(&self, call: impl Fn(&T) + Send + Sync + 'static) -> Subscription<T> {
        let id = {
            let mut state = self.lock();
            state.next_id += 1;
            let id = state.next_id;
            state.listeners.push(BusListener {
                id,
                call: Arc::new(call),
            });
            id
        };
        Subscription {
            id,
            inner: Arc::clone(&self.inner),
        }
    }

    /// How many subscribers are attached. A headless caller reads this to tell
    /// "published into the void" from "some browser will act on it" -- v2's
    /// `subscriberCount`, used by `uiDispatch` for exactly that.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.lock().listeners.len()
    }

    /// How many messages the ring holds, for owners whose retention must be
    /// provably zero. v2's `retainedCount`, and the assertion behind
    /// `apps/coord/tests/ui-state/ui-state-owner.test.ts:123`.
    #[must_use]
    pub fn retained_count(&self) -> usize {
        self.lock().ring.len()
    }

    fn lock(&self) -> MutexGuard<'_, BusState<T>> {
        // A panic inside the lock can only be an allocation failure: the ring
        // push and the listener bookkeeping are the whole critical section, and
        // every subscriber callback runs outside it. Recovering keeps one
        // impossible fault from bricking a bus for the life of the process.
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T> std::fmt::Debug for BoundedBus<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundedBus")
            .field("capacity", &self.inner.capacity)
            .field("subscribers", &self.subscriber_count())
            .field("retained", &self.retained_count())
            .finish()
    }
}

/// One subscriber's attachment, ended by dropping it.
pub struct Subscription<T> {
    id: u64,
    inner: Arc<BusInner<T>>,
}

impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.listeners.retain(|listener| listener.id != self.id);
    }
}

impl<T> std::fmt::Debug for Subscription<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Subscription")
            .field("id", &self.id)
            .finish()
    }
}

/// A panic payload as text. A payload is `Box<dyn Any>`, which is not JSON, and
/// the reporting line must not be the thing that trips over it.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}
