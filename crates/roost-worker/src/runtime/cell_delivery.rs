//! The bridge between the session layer's `CellDelivery` trait and
//! `CellEmitter`'s inherent methods. `SessionManager` announces a channel's
//! delivery lifecycle through the trait; `CellEmitter` holds the state those
//! calls change. Called by `runtime::deps` when it builds the production
//! capability set. Depends on `session::emit`, `session::lifecycle` and
//! `session::binding` — and on nothing that depends on it back.
//!
//! WHY THIS EXISTS AND NOT AN `impl CellDelivery FOR CellEmitter`, which is the
//! short answer and the wrong one. `CellEmitter::install_stream` takes a
//! `&mut SessionRecord` because it re-addresses the record's emit state to the
//! generation the coordinator just minted. The trait's signature has no record —
//! it names a channel — so the record has to be FOUND, and finding it means
//! holding the session table. That is the second half, and it is why this is a
//! type rather than an impl: `session::binding` says in so many words that the
//! wiring is `runtime`'s and "the only place allowed to hold both halves".
//!
//! IT SHARES THE EMITTER, and that is the reason it is behind a lock.
//! `SessionManager::new` takes TWO slots that both drive one emitter — `cells:
//! Arc<Mutex<dyn CellDelivery>>` and `ingest: Arc<Mutex<dyn ChannelDelivery>>` —
//! so this type hands [`TableCellDelivery::emitter`] to the sibling bridge in
//! `super::channel_delivery` and the two share one `Arc<Mutex<CellEmitter>>`.
//! Two emitters would be two answers to "is this channel due a frame", and the
//! two would disagree within one tick.
//!
//! THE LOCK ORDER IS THE NOTE THE NEXT PERSON NEEDS. `install_stream` and
//! `forget_channel` take the inner lock, and so do `ingest_output` and the
//! capture pair on the other trait. Neither trait's methods route through the
//! other's, so the inner lock is never held across a call that wants it again.
//! A future edit that reaches `install_stream` from an ingest call deadlocks on
//! a non-reentrant `std::sync::Mutex`, and it presents as a parked thread
//! rather than as an error.

use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::ChannelId;

use crate::session::binding::CellDelivery;
use crate::session::emit::CellEmitter;
use crate::session::lifecycle::SessionTable;

/// `CellDelivery` over a `CellEmitter` and the table that locates the records it
/// re-addresses.
pub struct TableCellDelivery {
    emitter: Arc<Mutex<CellEmitter>>,
    sessions: Arc<SessionTable>,
}

impl std::fmt::Debug for TableCellDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The emitter's own `Debug` would print every live channel's stream
        // state, which is a page of noise in a log line about a capability.
        formatter
            .debug_struct("TableCellDelivery")
            .field("sessions", &self.sessions.live().len())
            .finish_non_exhaustive()
    }
}

impl TableCellDelivery {
    /// The bridge over an emitter and the table that holds its records.
    pub fn new(emitter: CellEmitter, sessions: Arc<SessionTable>) -> Self {
        Self {
            emitter: Arc::new(Mutex::new(emitter)),
            sessions,
        }
    }

    /// The emitter behind the lock, so `super::channel_delivery` can share it.
    pub fn emitter(&self) -> Arc<Mutex<CellEmitter>> {
        Arc::clone(&self.emitter)
    }

    /// Take the emitter for the duration of one call, or recover the lock.
    fn with_emitter<R>(&self, call: impl FnOnce(&mut CellEmitter) -> R) -> R {
        let mut emitter = self
            .emitter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        call(&mut emitter)
    }
}

impl CellDelivery for TableCellDelivery {
    /// A channel's stream generation, so deltas can flow to it.
    ///
    /// A channel the table does not hold is NOT an error and NOT a silent
    /// success. It means the stream was announced for a session that is already
    /// gone, and the emitter's answer would be to install a generation nothing
    /// will ever emit against — a row that looks live and produces no cells. So
    /// it is reported, and the emitter is not touched: a session that is gone has
    /// nothing to re-address, and inventing state for it is how a second answer
    /// to "is this channel delivering" gets created.
    fn install_stream(&mut self, channel_id: ChannelId, stream_id: &str) {
        // The table is keyed by the wire's `u16`; `ChannelId` is a `u32` brand
        // over the same number, and a keeper addresses channels in `u16`. An id
        // that does not fit is a channel the keeper could not have opened, so it
        // takes the same branch as one this worker no longer holds.
        let Some(raw) = u16::try_from(channel_id.as_u32()).ok() else {
            tracing::warn!(
                %channel_id,
                stream_id,
                "a stream was announced for a channel a keeper cannot address"
            );
            return;
        };
        let Some(record) = self.sessions.record_of_channel(raw) else {
            tracing::warn!(
                %channel_id,
                stream_id,
                "a stream was announced for a channel this worker no longer holds; \
                 nothing is installed for it"
            );
            return;
        };
        let mut record = record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.with_emitter(|emitter| emitter.install_stream(&mut record, stream_id));
    }

    /// A channel is gone; its delivery state and parked cursors go with it.
    ///
    /// No record lookup, and deliberately: the emitter forgets BY CHANNEL and
    /// the point of the call is that the record is already unreachable. Looking
    /// it up first would make a forget depend on the thing it is cleaning up
    /// after, and a record that outlives its forget would leak the state
    /// forever.
    fn forget_channel(&mut self, channel_id: ChannelId) {
        self.with_emitter(|emitter| emitter.forget_channel(channel_id));
    }
}
