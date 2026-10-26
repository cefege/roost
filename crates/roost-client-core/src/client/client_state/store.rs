//! The local half of account-scoped state sync: a `KeyValueStore` that stamps
//! each synced write with the wall clock and remembers which keys have not
//! reached the coordinator yet.
//!
//! It wraps the host's local store, so every existing writer keeps calling
//! `set` and syncs without knowing it. A remote merge writes through the inner
//! store and never marks a key dirty, which is what stops a pulled value from
//! echoing back to the coordinator as a local change.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use roost_protocol::client_state::is_synced_key;

use super::entry::ClientStateEntry;
use super::stamps::{load_stamps, save_stamps};
use crate::platform::{EpochClock, KeyValueStore, MemoryEpochClock, MemoryKeyValueStore};

pub struct ClientStateStore {
    local: Rc<dyn KeyValueStore>,
    epoch: Rc<dyn EpochClock>,
    stamps: RefCell<BTreeMap<String, i64>>,
    dirty: RefCell<BTreeMap<String, u64>>,
    next_write_seq: Cell<u64>,
}

impl std::fmt::Debug for ClientStateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientStateStore")
            .finish_non_exhaustive()
    }
}

impl ClientStateStore {
    /// Wrap `local`, stamping writes with `epoch`. Stamps persisted by an
    /// earlier run are loaded so a remote merge can compare against them.
    pub fn new(local: Rc<dyn KeyValueStore>, epoch: Rc<dyn EpochClock>) -> Self {
        let stamps = load_stamps(local.as_ref());
        Self {
            local,
            epoch,
            stamps: RefCell::new(stamps),
            dirty: RefCell::new(BTreeMap::new()),
            next_write_seq: Cell::new(0),
        }
    }

    /// A store over memory, for the in-memory core and its tests.
    pub fn in_memory() -> Self {
        Self::new(
            Rc::new(MemoryKeyValueStore::new()),
            Rc::new(MemoryEpochClock::new()),
        )
    }

    fn record_local_write(&self, key: &str) {
        let updated_ms = self.epoch.epoch_ms().max(0);
        self.stamps.borrow_mut().insert(key.to_string(), updated_ms);
        self.persist_stamps();
        self.mark_dirty(key);
    }

    fn persist_stamps(&self) {
        save_stamps(self.local.as_ref(), &self.stamps.borrow());
    }

    /// Record a local write as needing a push, numbered so a flush can tell
    /// whether the key was written again while its batch was in flight.
    pub(crate) fn mark_dirty(&self, key: &str) {
        let seq = self.next_write_seq.get() + 1;
        self.next_write_seq.set(seq);
        self.dirty.borrow_mut().insert(key.to_string(), seq);
    }

    /// Every dirty key with the write sequence it was last dirtied at.
    pub(crate) fn dirty_snapshot(&self) -> Vec<(String, u64)> {
        self.dirty
            .borrow()
            .iter()
            .map(|(key, seq)| (key.clone(), *seq))
            .collect()
    }

    /// Whether `key` has a local write its coordinator has not acknowledged yet.
    pub(crate) fn is_dirty(&self, key: &str) -> bool {
        self.dirty.borrow().contains_key(key)
    }

    /// Clear a key's dirty mark, unless it was written again after `seq`.
    /// Returns whether the mark was cleared.
    pub(crate) fn clear_dirty_if_unchanged(&self, key: &str, seq: u64) -> bool {
        let mut dirty = self.dirty.borrow_mut();
        if dirty.get(key) == Some(&seq) {
            dirty.remove(key);
            true
        } else {
            false
        }
    }

    /// The entry a flush sends for `key`: the local value, or `""` when absent,
    /// with its stamp. `None` when the key has never been stamped.
    pub(crate) fn flush_entry(&self, key: &str) -> Option<ClientStateEntry> {
        let updated_ms = self.stamps.borrow().get(key).copied()?;
        Some(ClientStateEntry {
            key: key.to_string(),
            value: self.local.get(key).unwrap_or_default(),
            updated_ms,
        })
    }

    pub(crate) fn stamp_of(&self, key: &str) -> Option<i64> {
        self.stamps.borrow().get(key).copied()
    }

    pub(crate) fn stamped_keys(&self) -> Vec<String> {
        self.stamps.borrow().keys().cloned().collect()
    }

    /// Write a remote value through to local storage with its stamp. Does not
    /// mark the key dirty. An empty value applies as a removal.
    pub(crate) fn apply_remote(&self, entry: &ClientStateEntry) {
        if entry.value.is_empty() {
            self.local.remove(&entry.key);
        } else {
            self.local.set(&entry.key, &entry.value);
        }
        self.stamps
            .borrow_mut()
            .insert(entry.key.clone(), entry.updated_ms);
        self.persist_stamps();
    }
}

impl KeyValueStore for ClientStateStore {
    fn get(&self, key: &str) -> Option<String> {
        self.local.get(key)
    }

    fn set(&self, key: &str, value: &str) {
        self.local.set(key, value);
        if is_synced_key(key) {
            self.record_local_write(key);
        }
    }

    /// Removing a synced key is a write of absence: it stamps and marks dirty
    /// like any other write, and the flush sends it as `""`.
    fn remove(&self, key: &str) {
        self.local.remove(key);
        if is_synced_key(key) {
            self.record_local_write(key);
        }
    }
}
