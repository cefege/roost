//! The Sync recovery cursor: the highest event id this client has folded, what
//! storage holds, and what is still owed to it.
//!
//! It is the one piece of client state that must outlive a reload. Sent as
//! `since=` on the next dial, it is what makes a reconnect a durable backfill
//! rather than a full re-hydration of every domain.
//!
//! Two rules, both from incidents:
//!
//! - It advances INSIDE the dispatch that applies the event, so it can never
//!   name an event the store has not applied. A cursor that runs ahead of the
//!   store is a reconnect that silently skips a session's whole history.
//! - The write to storage is DEBOUNCED here rather than in the host, so a
//!   credential boundary can discard it. A persisted global cursor would make
//!   the next socket's initial history invisible
//!   (`apps/web/src/store/sync-frame.ts:55-69`).

use crate::platform::KeyValueStore;
use crate::sync::link::SYNC_WATERMARK_KEY;

/// The three numbers a recovery cursor is made of.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryWatermark {
    /// The highest event id folded into the store.
    pub last_seen: u64,
    /// What storage actually holds.
    pub persisted: u64,
    /// What is owed to storage on the next sweep.
    pub pending: Option<u64>,
}

impl RecoveryWatermark {
    /// Read the cursor from storage. A missing, unparseable, or absent value is
    /// zero: a first connect, or state the user cleared.
    pub fn from_storage(storage: &dyn KeyValueStore) -> Self {
        let persisted = storage
            .get(SYNC_WATERMARK_KEY)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        Self {
            last_seen: persisted,
            persisted,
            pending: None,
        }
    }

    /// Advance the cursor. Returns true when it moved.
    pub fn note(&mut self, event_id: u64) -> bool {
        if event_id <= self.last_seen {
            return false;
        }
        self.last_seen = event_id;
        self.pending = Some(event_id);
        true
    }

    /// Take what is owed to storage, if anything changed.
    ///
    /// Called on the sweep, so a burst of a thousand events in one drain writes
    /// once rather than a thousand times.
    pub fn take_pending(&mut self) -> Option<u64> {
        let pending = self.pending?;
        if pending == self.persisted {
            return None;
        }
        self.pending = None;
        self.persisted = pending;
        Some(pending)
    }

    /// Discard the cursor and everything owed to storage.
    pub fn reset(&mut self, storage: &dyn KeyValueStore) {
        self.last_seen = 0;
        self.persisted = 0;
        self.pending = None;
        storage.remove(SYNC_WATERMARK_KEY);
    }
}

#[cfg(test)]
mod tests {
    use super::RecoveryWatermark;
    use crate::platform::{KeyValueStore, MemoryKeyValueStore};

    #[test]
    fn a_stored_cursor_resumes_a_backfill() {
        let storage = MemoryKeyValueStore::new();
        storage.set("roost.syncLastEventId", "900");
        let mut watermark = RecoveryWatermark::from_storage(&storage);
        assert_eq!(watermark.last_seen, 900);
        assert_eq!(watermark.take_pending(), None, "storage is already current");
    }

    #[test]
    fn a_burst_collapses_into_one_write() {
        let storage = MemoryKeyValueStore::new();
        let mut watermark = RecoveryWatermark::from_storage(&storage);
        for event_id in 1..=1_000 {
            assert!(watermark.note(event_id));
        }
        assert_eq!(watermark.last_seen, 1_000);
        assert_eq!(watermark.take_pending(), Some(1_000));
        assert_eq!(watermark.take_pending(), None, "the write happened once");
    }

    #[test]
    fn a_stale_event_never_moves_the_cursor_backwards() {
        let mut watermark = RecoveryWatermark::default();
        assert!(watermark.note(10));
        assert!(!watermark.note(9), "a reordered event must not rewind");
        assert_eq!(watermark.last_seen, 10);
    }

    #[test]
    fn a_credential_boundary_discards_the_pending_write() {
        let storage = MemoryKeyValueStore::new();
        let mut watermark = RecoveryWatermark::from_storage(&storage);
        watermark.note(500);
        watermark.reset(&storage);
        assert_eq!(watermark.last_seen, 0);
        assert_eq!(watermark.take_pending(), None);
    }
}
