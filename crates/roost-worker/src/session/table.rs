//! The worker's live sessions, by channel and by session.
//!
//! ONE TYPE, ONE QUESTION: does this worker hold this session, and if so what is
//! its record. `SessionManager` owns the sessions' BEHAVIOUR and is in
//! `lifecycle.rs`; this owns the INDEX they are held in. They are split by
//! concept rather than by line count, and every method here hands the record to
//! a closure or to a caller that holds the `Arc` — so no caller ever names this
//! type's lock. That is the property the split must not cost, because a guard
//! escaping into a caller is a lock order this crate does not otherwise have.
//!
//! Called by `SessionManager` and by every capability that reads sessions —
//! `browser_commands::search_scan`, `capture::recorder`,
//! `session::retained_grid`, `runtime::deps`. Depends on `super::types` for the
//! record and on `roost_protocol` for the two id brands — nothing that depends
//! on it back.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use roost_protocol::wire::brand::SessionId;

use super::types::SessionRecord;
use crate::browser_commands::Refusal;

/// The live records, by channel and by session.
#[derive(Debug, Default)]
pub(super) struct LiveSet {
    by_channel: HashMap<u16, Arc<Mutex<SessionRecord>>>,
    by_session: HashMap<SessionId, u16>,
}

/// The worker's live sessions: the one answer to "does this worker hold it".
/// Every read hands the record to a closure rather than returning a guard, so no
/// caller names this type's lock.
#[derive(Debug, Default)]
pub struct SessionTable {
    live: Mutex<LiveSet>,
}

impl SessionTable {
    /// Hold a new record. An id already here is refused rather than overwritten:
    /// replacing one leaves a PTY whose bytes reach a record nobody can address.
    pub fn insert(&self, record: SessionRecord) -> Result<Arc<Mutex<SessionRecord>>, Refusal> {
        // The table is keyed by the raw keeper id that every binding and the
        // resume path already carry; the record's branded id is that same
        // number in a stronger type.
        let channel_id = record.channel_id().as_u32() as u16;
        let session_id = record.session_id().clone();
        let mut live = self.lock();
        if live.by_channel.contains_key(&channel_id) || live.by_session.contains_key(&session_id) {
            return Err(Refusal::failed(
                "sessions",
                format!("channel {channel_id} or session {session_id} is already live here"),
            ));
        }
        let entry = Arc::new(Mutex::new(record));
        live.by_channel.insert(channel_id, Arc::clone(&entry));
        live.by_session.insert(session_id, channel_id);
        Ok(entry)
    }

    /// Read the live record for a session id.
    pub fn with_record<R>(
        &self,
        session_id: &SessionId,
        read: impl FnOnce(&SessionRecord) -> R,
    ) -> Option<R> {
        let channel_id = self.lock().by_session.get(session_id).copied()?;
        self.with_channel_record(channel_id, read)
    }

    /// Change the live record for a session id. Separate from the read form
    /// because taking `&mut` is how a caller stops being a reader.
    pub fn with_record_mut<R>(
        &self,
        session_id: &SessionId,
        change: impl FnOnce(&mut SessionRecord) -> R,
    ) -> Option<R> {
        let channel_id = self.lock().by_session.get(session_id).copied()?;
        let entry = Arc::clone(self.lock().by_channel.get(&channel_id)?);
        let mut record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(change(&mut record))
    }

    /// Read the live record for a keeper channel id.
    pub fn with_channel_record<R>(
        &self,
        channel_id: u16,
        read: impl FnOnce(&SessionRecord) -> R,
    ) -> Option<R> {
        let entry = Arc::clone(self.lock().by_channel.get(&channel_id)?);
        let record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(read(&record))
    }

    /// The live record for a keeper channel id, as an `Arc` the CALLER holds.
    ///
    /// Distinct from [`SessionTable::with_channel_record`], which lends the
    /// record to a closure while holding the lock. This one hands over the `Arc`
    /// and releases the lock immediately, which is what a caller needs when the
    /// thing it will do with the record — install a delivery generation, reframe
    /// a core — also wants a lock of its own, and holding the table's across it
    /// would be a lock order nothing else in this crate observes.
    pub fn record_of_channel(&self, channel_id: u16) -> Option<Arc<Mutex<SessionRecord>>> {
        self.lock().by_channel.get(&channel_id).map(Arc::clone)
    }

    /// The channel a session lives on, or `None` when this worker does not hold
    /// it.
    pub fn channel_of(&self, session_id: &SessionId) -> Option<u16> {
        self.lock().by_session.get(session_id).copied()
    }

    /// Every live session and its channel, in no particular order.
    pub fn live(&self) -> Vec<(SessionId, u16)> {
        self.lock()
            .by_session
            .iter()
            .map(|(session, channel)| (session.clone(), *channel))
            .collect()
    }

    /// The record a channel's bytes are delivered into.
    pub(super) fn entry(&self, channel_id: u16) -> Option<Arc<Mutex<SessionRecord>>> {
        self.lock().by_channel.get(&channel_id).map(Arc::clone)
    }

    /// Remove a channel's record. The returned entry is the last reference, so a
    /// second close finds nothing to close.
    pub(super) fn forget(&self, channel_id: u16) -> Option<Arc<Mutex<SessionRecord>>> {
        let mut live = self.lock();
        let entry = live.by_channel.remove(&channel_id)?;
        let held: Vec<SessionId> = live
            .by_session
            .iter()
            .filter(|(_, channel)| **channel == channel_id)
            .map(|(session, _)| session.clone())
            .collect();
        for session_id in held {
            live.by_session.remove(&session_id);
        }
        Some(entry)
    }

    fn lock(&self) -> MutexGuard<'_, LiveSet> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
