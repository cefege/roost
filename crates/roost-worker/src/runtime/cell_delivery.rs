//! The session layer's [`CellDelivery`] over the one shared `CellEmitter`: the
//! manager's reach into v2 `session-manager.ts` `installTerminalBaseline` (on
//! stream install), `markInputSensitive`, `cancelCellEmission` and
//! `_releaseSyncOutputHold`. Built by `runtime::session_stack`, which hands
//! [`TableCellDelivery::emitter`] to `super::channel_delivery` and the cadence.
//! Lock order: `cells` → record → emitter; never take a record under the emitter.

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

    /// The ONE emitter, shared with `super::channel_delivery` and the cadence.
    pub fn emitter(&self) -> Arc<Mutex<CellEmitter>> {
        Arc::clone(&self.emitter)
    }

    fn with_emitter<R>(&self, call: impl FnOnce(&mut CellEmitter) -> R) -> R {
        let mut emitter = self
            .emitter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        call(&mut emitter)
    }

    /// The table key for a channel, or `None` for one a keeper cannot address.
    fn table_key(channel_id: ChannelId) -> Option<u16> {
        u16::try_from(channel_id.as_u32()).ok()
    }
}

impl CellDelivery for TableCellDelivery {
    /// Adopt a channel's stream generation and owe its baseline (v2 commits a
    /// stream and then `installTerminalBaseline`s it). A channel the table does
    /// not hold is reported and left alone: installing a generation nothing
    /// will emit against is a row that looks live and paints nothing.
    fn install_stream(&mut self, channel_id: ChannelId, stream_id: &str) {
        let Some(record) =
            Self::table_key(channel_id).and_then(|raw| self.sessions.record_of_channel(raw))
        else {
            tracing::warn!(
                %channel_id,
                stream_id,
                "a stream was announced for a channel this worker does not hold; nothing is installed"
            );
            return;
        };
        let mut record = record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.with_emitter(|emitter| {
            emitter.install_stream(&mut record, stream_id);
            emitter.request_terminal_baseline(channel_id);
        });
    }

    /// A channel is gone; its delivery state, parked cursors and queued work
    /// go with it. No record lookup: the record is already unreachable.
    fn forget_channel(&mut self, channel_id: ChannelId) {
        self.with_emitter(|emitter| emitter.forget_channel(channel_id));
    }

    fn note_input_echo(&mut self, channel_id: ChannelId, now_ms: i64) {
        let held = Self::table_key(channel_id)
            .is_some_and(|raw| self.sessions.record_of_channel(raw).is_some());
        if !held {
            tracing::debug!(%channel_id, "an input echo was noted for a channel this worker does not hold");
            return;
        }
        self.with_emitter(|emitter| emitter.note_input_echo(channel_id, now_ms));
    }

    fn cancel_cell_emission(&mut self, channel_id: ChannelId) {
        self.with_emitter(|emitter| emitter.cancel_cell_emission(channel_id));
    }

    fn release_sync_output_hold(&mut self, channel_id: ChannelId) {
        self.with_emitter(|emitter| emitter.release_sync_output_hold(channel_id));
    }

    fn channel_diagnostics(
        &self,
        channel_id: ChannelId,
    ) -> crate::session::diagnostics::ChannelDiagnostics {
        let emitter = self
            .emitter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        emitter.channel_diagnostics(channel_id)
    }
}
