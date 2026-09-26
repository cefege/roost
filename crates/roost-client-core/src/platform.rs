//! The whole host surface the core calls into: a clock and a key/value store.
//!
//! Everything else a host provides — sockets, Connect calls, device-key
//! signing, direct carriers — arrives as an `Effect` the host performs and an
//! event it reports back, because the core never *calls* those; it asks. See
//! `docs/phase4-client-contract.md` §2 for why each of those is deliberately
//! not a trait, and what breaks if one of them is.
//!
//! Two in-memory implementations ship here: every test needs them, and a TUI
//! host needs them before it has a config file worth parsing.

use std::cell::RefCell;
use std::collections::BTreeMap;

/// Milliseconds, monotonic within a process, arbitrary epoch.
///
/// `roost-protocol` takes every timestamp as a parameter so its own tests are
/// deterministic, which makes the core the one place time is read. Four
/// deadlines depend on it: the chunked-snapshot stall sweep, the resync retry
/// gate, the view lease renewal, and the held-input admission timeout. Two of
/// them reading the clock independently is how a deadline starts disagreeing
/// with the sequence number that justified it, so `handle` reads it at most once
/// and passes the number down.
pub trait Clock {
    /// The current time in milliseconds.
    fn now_ms(&self) -> u64;
}

/// The `localStorage`/`IndexedDB` equivalent: three operations, no more.
///
/// The Sync recovery watermark is the one piece of client state that must
/// outlive a reload — it is sent as `since=` on the next dial, which is what
/// makes a reconnect a backfill rather than a full re-hydration. The pairing
/// tab id is the second key. A browser wants `localStorage`, a TUI a file, a
/// mobile host preferences; all three are these three calls.
pub trait KeyValueStore {
    /// The value stored under `key`, or `None` when nothing is.
    fn get(&self, key: &str) -> Option<String>;
    /// Write `value` under `key`, replacing any previous value.
    fn set(&self, key: &str, value: &str);
    /// Forget `key`. A missing key is not an error.
    fn remove(&self, key: &str);
}

/// A clock a test moves by hand.
///
/// Interior mutability rather than `Cell`, because a host that needs to advance
/// time while a `&ClientCore` borrow is live (the usual shape: assert the store,
/// then tick) cannot reach a `&mut self` clock.
#[derive(Debug, Default)]
pub struct MemoryClock {
    now_ms: RefCell<u64>,
}

impl MemoryClock {
    /// A clock reading zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// A clock reading `now_ms`.
    pub fn starting_at(now_ms: u64) -> Self {
        Self {
            now_ms: RefCell::new(now_ms),
        }
    }

    /// Move the clock forward. Saturating, because a negative jump is a bug in
    /// the caller and would silently rewind a deadline.
    pub fn advance(&self, delta_ms: u64) {
        let mut now = self.now_ms.borrow_mut();
        *now = now.saturating_add(delta_ms);
    }

    /// Set the clock to an absolute value. Monotonic by contract: going
    /// backwards would make a satisfied deadline look unsatisfied.
    pub fn set(&self, now_ms: u64) {
        let mut now = self.now_ms.borrow_mut();
        if now_ms > *now {
            *now = now_ms;
        }
    }
}

impl Clock for MemoryClock {
    fn now_ms(&self) -> u64 {
        *self.now_ms.borrow()
    }
}

/// A key/value store held in memory. Nothing here survives the process, which is
/// the point: a test host that persisted would leak one test's watermark into the
/// next.
#[derive(Debug, Default)]
pub struct MemoryKeyValueStore {
    entries: RefCell<BTreeMap<String, String>>,
}

impl MemoryKeyValueStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many keys are held. Tests use it to assert a debounced write was
    /// emitted rather than guessed at.
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Whether the store holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl KeyValueStore for MemoryKeyValueStore {
    fn get(&self, key: &str) -> Option<String> {
        self.entries.borrow().get(key).cloned()
    }

    fn set(&self, key: &str, value: &str) {
        self.entries
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
    }

    fn remove(&self, key: &str) {
        self.entries.borrow_mut().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, KeyValueStore, MemoryClock, MemoryKeyValueStore};

    #[test]
    fn the_memory_clock_moves_only_forward() {
        let clock = MemoryClock::starting_at(100);
        clock.advance(50);
        assert_eq!(clock.now_ms(), 150);
        // A backwards set is refused rather than applied: rewinding the clock
        // would make an already-satisfied deadline look outstanding again.
        clock.set(10);
        assert_eq!(clock.now_ms(), 150);
    }

    #[test]
    fn the_memory_store_round_trips_and_forgets() {
        let store = MemoryKeyValueStore::new();
        assert!(store.is_empty());
        store.set("roost.syncLastEventId", "42");
        assert_eq!(store.get("roost.syncLastEventId").as_deref(), Some("42"));
        store.set("roost.syncLastEventId", "43");
        assert_eq!(store.len(), 1, "a re-write replaces, it does not add");
        store.remove("roost.syncLastEventId");
        assert!(store.get("roost.syncLastEventId").is_none());
        store.remove("roost.syncLastEventId");
    }
}
