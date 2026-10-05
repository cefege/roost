//! The ingest half of the cell producer: v2 `apps/worker/src/session/
//! session-emit.ts` `emitUpstreamChunk` (live, capture and invalid-core lanes)
//! and `session-terminal-metadata.ts` `setTerminalMetadataNegotiated`/`replay`.
//! `runtime::channel_delivery` calls it on the keeper's dispatch thread with the
//! record locked; every chunk is retained and scanned, then routed to the cell
//! cadence and the two metadata lanes.

use std::time::Instant;

use roost_protocol::wire::brand::{ChannelId, SessionId};
use tracing::warn;

use super::cwd_events::CwdEventLane;
use super::emit::{CellEmitter, IngestOutcome};
use super::scrollback::{advance_captured_query_carry, answer_terminal_queries, append_pty_chunk};
use super::types::SessionRecord;

impl CellEmitter {
    /// v2 `emitUpstreamChunk`, live lane: retain + scan, parse, then route.
    pub fn ingest_pty_chunk(
        &mut self,
        record: &mut SessionRecord,
        chunk: &[u8],
        now_ms: i64,
    ) -> IngestOutcome {
        self.ingest_pty_chunk_at(record, chunk, now_ms, Instant::now())
    }

    pub fn ingest_pty_chunk_at(
        &mut self,
        record: &mut SessionRecord,
        chunk: &[u8],
        now_ms: i64,
        now: Instant,
    ) -> IngestOutcome {
        let channel_id = record.channel_id();
        if record.last_pty_out_ms == 0 {
            record.last_pty_out_ms = now_ms;
        }
        let cwd_events = &self.cwd_events;
        let end_seq = append_pty_chunk(record, chunk, &mut |session_id, cwd| {
            note_cwd_change(cwd_events, channel_id, session_id, cwd, now_ms);
        });
        self.capture.retain_output(record, end_seq, chunk);
        answer_terminal_queries(record, chunk, &self.query_replies);
        self.observe_sync_output(channel_id, chunk);
        let input_echo = self.input_echo_armed(channel_id, now_ms);
        if self.sinks.is_empty() && self.metadata.negotiated() {
            warn!(%channel_id, len = chunk.len(), "PTY output arrived with no cell sink registered");
        }
        let accepted = self.route_chunk_to_cells(record, input_echo, false, now_ms, now);
        self.observe_upstream(channel_id, end_seq, chunk, now_ms);
        tracing::trace!(%channel_id, len = chunk.len(), end_seq, accepted, "a PTY chunk was ingested");
        if accepted {
            IngestOutcome::Accepted {
                end_seq,
                input_echo,
            }
        } else {
            IngestOutcome::RetainedOnly { end_seq }
        }
    }

    /// v2's capture and invalid-core lanes (`appendCapturedScrollback`): retain
    /// and scan, never parse; the metadata lanes still see the bytes.
    pub fn retain_without_parsing(
        &mut self,
        record: &mut SessionRecord,
        chunk: &[u8],
        now_ms: i64,
    ) -> u64 {
        let channel_id = record.channel_id();
        if record.last_pty_out_ms == 0 {
            record.last_pty_out_ms = now_ms;
        }
        let cwd_events = &self.cwd_events;
        let end_seq = append_pty_chunk(record, chunk, &mut |session_id, cwd| {
            note_cwd_change(cwd_events, channel_id, session_id, cwd, now_ms);
        });
        self.capture.retain_output(record, end_seq, chunk);
        advance_captured_query_carry(record, chunk);
        self.observe_sync_output(channel_id, chunk);
        let input_echo = self.input_echo_armed(channel_id, now_ms);
        self.route_chunk_to_cells(record, input_echo, true, now_ms, Instant::now());
        self.observe_upstream(channel_id, end_seq, chunk, now_ms);
        end_seq
    }

    /// v2 `emitUpstreamChunk`'s stream branch. Returns whether a stream took it.
    fn route_chunk_to_cells(
        &mut self,
        record: &mut SessionRecord,
        promote_input_echo: bool,
        captured: bool,
        now_ms: i64,
        now: Instant,
    ) -> bool {
        let channel_id = record.channel_id();
        let Some(stream) = self
            .streams
            .get(&channel_id)
            .filter(|stream| stream.enabled)
        else {
            return false;
        };
        let (core_valid, sync_owed) = (stream.core_valid, stream.pending_sync_snapshot);
        let delivery = self.delivery_aggregate(channel_id);
        // A full deferred by a synchronized frame ships at the boundary the
        // application itself declared.
        let boundary_ready = !delivery.baseline_ready
            && !delivery.snapshot_pending
            && sync_owed
            && !self.synchronized_output(channel_id);
        if boundary_ready {
            self.install_terminal_baseline_at(record, now_ms, now);
        } else if !delivery.baseline_ready || delivery.snapshot_pending || captured || !core_valid {
            self.mark_stream_delivery_dirty(channel_id);
            self.note_dirty(channel_id);
        } else {
            self.schedule_cell_emission(record, promote_input_echo, now_ms, now);
        }
        true
    }

    /// The metadata lanes after the cell lane (v2 order): semantic facts always,
    /// raw bytes only while the semantic lane is not negotiated.
    fn observe_upstream(&mut self, channel_id: ChannelId, end_seq: u64, chunk: &[u8], now_ms: i64) {
        let flush_owed = self.metadata.observe(channel_id, chunk, now_ms);
        let raw_owed = !self.metadata.negotiated() && self.raw.stage(channel_id, end_seq, chunk);
        if flush_owed || raw_owed {
            self.wake_cadence();
        }
    }

    /// v2 `setTerminalMetadataNegotiated`: one flag for both metadata lanes.
    pub fn set_terminal_metadata_negotiated(&mut self, negotiated: bool) {
        self.raw.set_semantic_metadata_negotiated(negotiated);
        if self.metadata.set_negotiated(negotiated) {
            self.wake_cadence();
        }
    }

    /// v2 `replayTerminalMetadata`.
    pub fn replay_terminal_metadata(&mut self) {
        if self.metadata.replay() {
            self.wake_cadence();
        }
    }
}

/// OSC 7 moved the session (v2 `session-scrollback.ts:178-185`): the scan
/// updates the record's cwd, which the snapshot reads, and the `cwd` event is
/// published off this thread by `session::cwd_events`.
fn note_cwd_change(
    lane: &CwdEventLane,
    channel_id: ChannelId,
    session_id: &SessionId,
    cwd: &str,
    now_ms: i64,
) {
    tracing::info!(%channel_id, %session_id, cwd, "a session reported a new working folder");
    lane.send(session_id, cwd, now_ms);
}
