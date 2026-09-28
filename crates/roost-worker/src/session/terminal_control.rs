//! The entry points for coordinator- and view-driven terminal stream state:
//! validate the desire, mint its generation, and queue its transaction on the
//! channel's control lane; and answer a repeated snapshot request with a fresh
//! baseline. Called by `terminal_view` (in-process) and, for the coordinator
//! link, by `session::terminal_stream_owner::StreamOwner`. Ports the stream half
//! of `apps/worker/src/session/session-terminal-control.ts`.

use std::sync::Arc;

use futures_util::FutureExt as _;
use roost_protocol::viewport::{TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS, is_terminal_uuid};
use roost_protocol::wire::brand::{ChannelId, SessionId};
use roost_protocol::wire::coord_worker::TerminalStreamFailureKind as Failure;

use super::control_lanes::{ControlKind, ControlLanes};
use super::keeper_admission::{Admission, AdmissionKind};
use super::lifecycle::SessionManager;
use super::resize::lock;
use super::terminal_state::{
    FailedStatus, StreamFacts, StreamGeneration, StreamIntent, StreamOperation, StreamSlot,
    TerminalStreamFacts, WorkerStreamResult,
};
use super::terminal_txn::{TxnGeneration, apply_terminal_stream_now};

impl SessionManager {
    /// The per-channel control and write-ordering lanes every terminal write
    /// and stream transaction on this worker shares.
    pub fn control_lanes(&self) -> &Arc<ControlLanes> {
        &self.lanes
    }

    /// Apply one aggregated stream desire (v2 `applyTerminalStreamState`).
    ///
    /// Validation, the duplicate check, admission and the mint all happen NOW,
    /// in call order; only the keeper work waits on the control lane. A refused
    /// admission leaves the previous generation exactly as it was.
    pub fn apply_terminal_stream_state(&self, intent: StreamIntent) -> StreamOperation {
        if let Some(reason) = invalid_intent(&intent) {
            return settled(refused(&intent, Failure::InvalidRequest, &reason));
        }
        let live = self.sessions.channel_of(&intent.session_id);
        let Some((raw, channel_id)) = live.and_then(|raw| Some((raw, channel(raw)?))) else {
            return settled(refused(
                &intent,
                Failure::SessionNotLive,
                "session is not live",
            ));
        };
        let Some(manager) = self.owned() else {
            let reason = "this worker's session manager is shutting down";
            return settled(refused(&intent, Failure::RetryablePreWrite, reason));
        };
        self.terminal_streams.transact(channel_id, |slot| {
            self.mint_generation(slot, manager, raw, channel_id, intent)
        })
    }

    fn mint_generation(
        &self,
        slot: &mut StreamSlot<'_>,
        manager: Arc<SessionManager>,
        raw: u16,
        channel_id: ChannelId,
        intent: StreamIntent,
    ) -> StreamOperation {
        let current = slot.current().cloned();
        if let Some(current) = current
            .as_ref()
            .filter(|current| current.stream_id == intent.stream_id)
        {
            if (current.enabled, current.cols, current.rows)
                != (intent.enabled, intent.cols, intent.rows)
            {
                let reason = "stream_id was reused with a conflicting payload";
                return settled(refused(&intent, Failure::InvalidRequest, reason));
            }
            return Box::pin(current.operation.clone());
        }
        let ticket = match self.lanes.admit(channel_id, AdmissionKind::TerminalResize) {
            Admission::Granted(ticket) => ticket,
            Admission::Refused(reason) => {
                return settled(refused(&intent, Failure::RetryablePreWrite, reason));
            }
        };
        let Some(entry) = self.sessions.entry(raw) else {
            return settled(refused(
                &intent,
                Failure::SessionNotLive,
                "session is not live",
            ));
        };
        {
            let mut record = lock(&entry);
            let delivery = lock(&self.ingest);
            let Some(emission) = delivery.stream_emission() else {
                tracing::error!(%channel_id, "a terminal stream was asked of a delivery that ships no cells");
                let reason = "this worker's channel delivery ships no cells";
                return settled(refused(&intent, Failure::CoreFailed, reason));
            };
            if current.is_some() && !emission.core_valid(channel_id) {
                tracing::warn!(
                    %channel_id,
                    stream_id = %intent.stream_id,
                    enabled = intent.enabled,
                    "terminal_stream_core_invalid: a frozen core must be re-proved before any generation can emit"
                );
            }
            // A generation owns sequence space, not grid identity: only a real
            // geometry change moves the epoch, so a warm renderer can merge a
            // renewed baseline into the history it already holds.
            let core_size = (record.terminal_core.cols(), record.terminal_core.rows());
            let applied = slot.applied_size().unwrap_or(core_size);
            let geometry_changed =
                intent.enabled && applied != (intent.cols as u16, intent.rows as u16);
            emission.mint_stream(
                &mut record,
                &intent.stream_id,
                intent.enabled,
                geometry_changed,
            );
        }
        let generation = slot.install(|version| {
            let txn = TxnGeneration {
                stream_id: intent.stream_id.clone(),
                enabled: intent.enabled,
                cols: intent.cols,
                rows: intent.rows,
                version,
            };
            let lanes = Arc::clone(&self.lanes);
            let budget = intent.budget.clone();
            let transaction: StreamOperation = Box::pin(async move {
                let run =
                    move || apply_terminal_stream_now(manager, channel_id, txn, budget, ticket);
                lanes
                    .enqueue(channel_id, ControlKind::TerminalStream, run)
                    .await
            });
            StreamGeneration {
                stream_id: intent.stream_id.clone(),
                enabled: intent.enabled,
                cols: intent.cols,
                rows: intent.rows,
                version,
                operation: transaction.shared(),
            }
        });
        tracing::info!(%channel_id, stream_id = %generation.stream_id, version = generation.version, request_id = %intent.request_id, "a terminal stream state was admitted");
        Box::pin(generation.operation)
    }

    /// A coordinator repair request (v2 `requestTerminalSnapshot`): every sink
    /// of the CURRENT enabled, valid generation gets a fresh full baseline; a
    /// request for any other stream is ignored.
    pub fn request_terminal_snapshot(&self, session_id: &SessionId, stream_id: &str) {
        let Some((raw, channel_id)) = self
            .sessions
            .channel_of(session_id)
            .and_then(|raw| Some((raw, channel(raw)?)))
        else {
            return;
        };
        self.terminal_streams.transact(channel_id, |slot| {
            let wanted = slot
                .current()
                .is_some_and(|current| current.enabled && current.stream_id == stream_id);
            let Some(entry) = self.sessions.entry(raw).filter(|_| wanted) else {
                tracing::debug!(%channel_id, stream_id, "a snapshot request named a stream this channel is not delivering");
                return;
            };
            let mut record = lock(&entry);
            let delivery = lock(&self.ingest);
            let Some(emission) = delivery.stream_emission() else {
                return;
            };
            if !emission.core_valid(channel_id) {
                return;
            }
            emission.retire_delivery(channel_id);
            let valid = emission.install_baseline(&mut record, self.clock.now_epoch_ms());
            tracing::info!(%channel_id, stream_id, core_valid = valid, "a snapshot request re-baselined every sink");
        });
    }

    /// The stream id a viewer may address, when the channel's generation is
    /// enabled and its core valid (v2 view screen `currentStreamId`).
    pub fn current_terminal_stream_id(&self, session_id: &SessionId) -> Option<String> {
        let channel_id = channel(self.sessions.channel_of(session_id)?)?;
        let facts = self.terminal_stream_facts(channel_id)?;
        (facts.enabled && facts.core_valid).then_some(facts.stream_id)
    }

    /// What a reader outside the transaction may know about a channel's stream.
    pub fn terminal_stream_facts(&self, channel_id: ChannelId) -> Option<TerminalStreamFacts> {
        let current = self.terminal_streams.current(channel_id)?;
        let core_valid = super::terminal_txn::core_valid(self, channel_id);
        Some(TerminalStreamFacts {
            version: current.version,
            stream_id: current.stream_id,
            enabled: current.enabled,
            cols: current.cols,
            rows: current.rows,
            core_valid,
        })
    }
}

/// v2's `UUID_RE`: versions 1–5 only, which is narrower than the protocol's
/// `is_terminal_uuid` (1–8) — v2 refuses a v7 id here, so this does too.
fn is_v2_uuid(value: &str) -> bool {
    is_terminal_uuid(value) && matches!(value.as_bytes().get(14), Some(b'1'..=b'5'))
}

/// The first rule an intent breaks, in v2's order.
fn invalid_intent(intent: &StreamIntent) -> Option<String> {
    if !is_v2_uuid(intent.session_id.as_str()) {
        return Some("session_id must be a UUID".to_owned());
    }
    if !is_v2_uuid(&intent.stream_id) {
        return Some("stream_id must be a UUID".to_owned());
    }
    // JavaScript's `length`: UTF-16 code units.
    let request_id_units = intent.request_id.encode_utf16().count();
    if request_id_units == 0 || request_id_units > 128 {
        return Some("request_id is invalid".to_owned());
    }
    if intent.enabled {
        let cols_ok = (1..=TERMINAL_MAX_COLS).contains(&intent.cols);
        if !cols_ok || !(1..=TERMINAL_MAX_ROWS).contains(&intent.rows) {
            return Some(format!(
                "enabled geometry must be within 1..{TERMINAL_MAX_COLS} cols and 1..{TERMINAL_MAX_ROWS} rows"
            ));
        }
    } else if intent.cols != 0 || intent.rows != 0 {
        return Some("disabled terminal stream must have zero geometry".to_owned());
    }
    None
}

/// v2 `invalidResult`: rejected before any write, echoing the intent.
fn refused(intent: &StreamIntent, failure: Failure, reason: &str) -> WorkerStreamResult {
    let facts = StreamFacts {
        stream_id: &intent.stream_id,
        enabled: intent.enabled,
        cols: intent.cols,
        rows: intent.rows,
    };
    tracing::info!(stream_id = %intent.stream_id, request_id = %intent.request_id, failure = failure.as_str(), reason, "a terminal stream state was refused before any write");
    WorkerStreamResult::failed(facts, 0, failure, reason, FailedStatus::Rejected, None)
}

fn settled(result: WorkerStreamResult) -> StreamOperation {
    Box::pin(std::future::ready(result))
}

fn channel(raw: u16) -> Option<ChannelId> {
    ChannelId::try_from(i64::from(raw)).ok()
}
