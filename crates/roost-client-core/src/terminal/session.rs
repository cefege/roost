//! One session's replica: what it expects, what it holds, what it is owed.
//!
//! This is the client half of `protocol/spec/terminal-stream.md:22-30`. It owns
//! the expectation (stream id and pane geometry), the canonical frame, the chunk
//! assembler, the repair latch, and the views that hold leases — and it admits
//! nothing that has not passed `frame_fold` first.
//!
//! Ported from `apps/web/src/store/terminal-stream-replica.ts` (admission),
//! `apps/web/src/store/terminal-stream-state.ts` (the record) and
//! `apps/web/src/store/terminal-stream-chunks.ts` (the chunk path). The rules and
//! their sources are in `docs/phase4-client-contract.md` §6.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::terminal::frame_fold::{
    FoldTarget, FrameFoldOutcome, decode_assembled_frame, decode_chunk_part, decode_wire_frame,
    fold,
};
use crate::terminal::liveness::ForegroundLiveness;
use crate::terminal::renderer_deliveries::RendererDeliveries;
use crate::terminal::repair::RepairLatch;
use crate::terminal::token::TerminalToken;
use crate::terminal::view::TerminalView;
use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::cell::{CellGridChunkAssembler, CellGridChunkAssembly, CellGridFrame};

pub use crate::terminal::admission::{Admission, ViewStateAdmission};

/// One session's canonical replica and everything fenced to it.
#[derive(Debug)]
pub struct TerminalSession {
    /// The session this replica belongs to.
    pub session_id: String,
    /// The worker that owns the PTY.
    pub worker_fp: String,
    target: FoldTarget,
    assembler: CellGridChunkAssembler,
    latch: RepairLatch,
    generation: Option<TerminalToken>,
    pub(crate) views: BTreeMap<String, TerminalView>,
    /// The last stream id seen on the wire, whatever its fate. Diagnostics only:
    /// it is how a stuck session is told "the sender thinks it is on X" apart
    /// from "the sender is on X and we are not accepting it".
    pub wire_stream_id: Option<String>,
    /// The last grid epoch seen on the wire, for the same reason.
    pub wire_grid_epoch: Option<String>,
    /// The last sequence seen on the wire.
    pub wire_seq: Option<u64>,
    /// Decoded wire frames by kind (v2 `noteWireFrame`), for the smoke counters.
    pub frame_counts: crate::terminal::frame_counts::FrameCounts,
    /// The revision every renderer repaints on, and the deltas since the last
    /// full a renderer folds so their appended history is painted.
    deliveries: RendererDeliveries,
    /// The foreground liveness watchdog's state for this replica, owned by
    /// `session_liveness` and fired by the sweep.
    pub(crate) liveness: ForegroundLiveness,
}

impl TerminalSession {
    /// A session with no expectation installed and nothing to paint.
    pub fn new(session_id: impl Into<String>, worker_fp: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            worker_fp: worker_fp.into(),
            target: FoldTarget::default(),
            assembler: CellGridChunkAssembler::new(),
            latch: RepairLatch::new(),
            liveness: ForegroundLiveness::new(),
            generation: None,
            views: BTreeMap::new(),
            wire_stream_id: None,
            wire_grid_epoch: None,
            wire_seq: None,
            frame_counts: crate::terminal::frame_counts::FrameCounts::default(),
            deliveries: RendererDeliveries::default(),
        }
    }

    /// The replica, for a host that renders it. Shared by refcount, so a host
    /// that keeps it across a paint holds a snapshot rather than a copy.
    pub fn canonical(&self) -> Option<&Rc<CellGridFrame>> {
        self.target.canonical.as_ref()
    }

    /// Whether a complete authoritative full for the CURRENT stream is installed.
    /// A renderer that asks this and gets false must keep painting what it had.
    pub fn baseline_ready(&self) -> bool {
        self.target.baseline_ready
    }

    /// The stream id the replica is fenced to.
    pub fn expected_stream_id(&self) -> Option<&str> {
        self.target.expected_stream_id.as_deref()
    }
    /// How many frames have been applied to this replica.
    pub fn frame_revision(&self) -> u64 {
        self.deliveries.revision()
    }

    /// Every accepted delta after `painted_revision`, or `None` when a
    /// renderer that far behind must paint the canonical full instead.
    pub fn deltas_since(&self, painted_revision: u64) -> Option<&[CellGridFrame]> {
        self.deliveries.deltas_since(painted_revision)
    }

    /// The pane geometry the replica is fenced to.
    pub fn effective_geometry(&self) -> (u32, u32) {
        (self.target.effective_cols, self.target.effective_rows)
    }

    /// Whether a repair is outstanding.
    pub fn repair_latched(&self) -> bool {
        self.latch.is_latched()
    }

    /// The repair latch, for the state machine that sends its request.
    pub fn latch(&self) -> &RepairLatch {
        &self.latch
    }

    /// The generation this replica is fenced to.
    pub fn generation(&self) -> Option<&TerminalToken> {
        self.generation.as_ref()
    }

    /// Bind the replica to a carrier generation.
    ///
    /// A frame is admitted only against the generation this call installed. A
    /// redial, a direct promotion, or a domain-generation bump all change the
    /// token, and everything already in flight for the old one becomes inert —
    /// which is what stops a coordinator→worker write that was already on the
    /// wire from landing in a replica the new carrier has not baselined.
    pub fn bind_generation(&mut self, token: &TerminalToken) -> bool {
        if self.generation.as_ref() == Some(token) {
            return false;
        }
        self.generation = Some(token.clone());
        true
    }

    /// Install an expectation. Returns true when `baseline_ready` changed, so
    /// the host knows to re-evaluate what it paints.
    ///
    /// A change in stream id invalidates liveness, requires a fresh baseline, and
    /// drops any chunked transfer in flight: a partial belongs to the stream
    /// being replaced, and completing it would publish a grid assembled from two
    /// generations. A geometry change alone is recomputed but does not by itself
    /// require a fresh baseline, because the authority mints a NEW stream id for
    /// a resize (`protocol/spec/terminal-stream.md:24`) — so the id change is
    /// what normally arrives with the geometry change, and it is the id check
    /// that carries the invalidation.
    pub fn install_expected_stream(&mut self, stream_id: &str, cols: u32, rows: u32) -> bool {
        let prior = self.target.baseline_ready;
        let stream_changed = self.target.expected_stream_id.as_deref() != Some(stream_id);
        if !stream_changed
            && self.target.effective_cols == cols
            && self.target.effective_rows == rows
        {
            return false;
        }
        self.target.expected_stream_id = Some(stream_id.to_string());
        self.target.effective_cols = cols;
        self.target.effective_rows = rows;
        if stream_changed {
            self.assembler.reset();
            self.latch.clear();
            self.target.baseline_ready = false;
        } else {
            self.target.baseline_ready = self.target.canonical.as_ref().is_some_and(|frame| {
                frame.stream_id == stream_id && frame.cols == cols && frame.rows == rows
            });
        }
        prior != self.target.baseline_ready
    }

    /// Admit one wire cell frame, from either carrier.
    pub fn admit_frame(
        &mut self,
        frame: &PbCellGridFrame,
        assembled: bool,
        token: &TerminalToken,
        now_ms: u64,
    ) -> Admission {
        if self.generation.as_ref() != Some(token) {
            return Admission::unbound(
                &self.session_id,
                "generation",
                self.generation
                    .as_ref()
                    .map_or("", |bound| bound.process_epoch.as_str()),
                &frame.stream_id,
                token,
            );
        }
        if frame.session_id != self.session_id {
            return self.refuse("terminal frame session mismatch", token, now_ms);
        }
        if Some(frame.stream_id.as_str()) != self.target.expected_stream_id.as_deref() {
            // Not this replica's frame. Latching a repair for it would ask the
            // authority for a baseline it already sent and this client ignored,
            // which is how one stale frame turns into a request storm.
            //
            // It still gets a line, though. This fence is also what a PERMANENTLY
            // dropped baseline looks like — the authority sent the frame, this
            // client ignored it, and with neither a line nor a latch `roost
            // doctor` and the watchdog both read it as a pane nobody is painting.
            return Admission::unbound(
                &self.session_id,
                "stream",
                self.target.expected_stream_id.as_deref().unwrap_or(""),
                &frame.stream_id,
                token,
            );
        }
        self.note_wire(&frame.stream_id, &frame.grid_epoch, frame.seq);
        match decode_wire_frame(frame, assembled) {
            Ok(decoded) => self.admit_decoded(decoded, token, now_ms),
            Err(reason) => self.refuse_owned(reason, token, now_ms),
        }
    }

    fn admit_decoded(
        &mut self,
        decoded: CellGridFrame,
        token: &TerminalToken,
        now_ms: u64,
    ) -> Admission {
        self.frame_counts.note_decoded(&decoded);
        // Read the assembler's own answer rather than tracking it separately: a
        // second source for "is something in flight" is a second thing to keep
        // correct, and the fold's refusal depends on it.
        self.target.canonical_chunk_in_flight = self.assembler.active_snapshot_id().is_some();
        match fold(&mut self.target, decoded) {
            FrameFoldOutcome::Invalid { reason } => self.refuse(reason.as_str(), token, now_ms),
            FrameFoldOutcome::Full => {
                // Only a complete authoritative full clears an outstanding gap:
                // an accepted delta proves the lane, not the hole.
                self.latch.clear();
                self.assembler.reset();
                self.target.canonical_chunk_in_flight = false;
                self.deliveries.note_full();
                self.note_accepted_frame(token, now_ms);
                tracing::info!(
                    target: "terminal",
                    session_id = %self.session_id,
                    stream_id = %self.wire_stream_id.as_deref().unwrap_or(""),
                    "replica baseline replaced"
                );
                Admission::BaselineReplaced
            }
            FrameFoldOutcome::Delta { delta } => {
                self.deliveries.note_delta(delta);
                self.note_accepted_frame(token, now_ms);
                Admission::DeltaApplied
            }
        }
    }

    /// Admit one part of a chunked baseline.
    ///
    /// The assembler owns every ordering and completeness rule
    /// (`protocol/spec/terminal-stream.md:30`). This owns the three it cannot:
    /// which replica a part belongs to, that a refusal latches a repair, and that
    /// the part ceiling is not re-applied to the assembled product — a chunked
    /// baseline is supposed to exceed one part, which is why it was chunked.
    pub fn admit_chunk(
        &mut self,
        chunk: &PbCellGridChunk,
        token: &TerminalToken,
        now_ms: u64,
    ) -> Admission {
        if self.generation.as_ref() != Some(token) {
            return Admission::unbound(
                &self.session_id,
                "generation",
                self.generation
                    .as_ref()
                    .map_or("", |bound| bound.process_epoch.as_str()),
                "",
                token,
            );
        }
        let Some(part) = decode_chunk_part(chunk) else {
            return self.refuse("missing-part", token, now_ms);
        };
        if part.session_id != self.session_id {
            return self.refuse("terminal frame session mismatch", token, now_ms);
        }
        if Some(part.stream_id.as_str()) != self.target.expected_stream_id.as_deref() {
            return Admission::unbound(
                &self.session_id,
                "stream",
                self.target.expected_stream_id.as_deref().unwrap_or(""),
                &part.stream_id,
                token,
            );
        }
        self.note_chunk_progress(token, &part.stream_id, part.seq);
        self.note_wire(&part.stream_id, &part.grid_epoch, part.seq);
        match self.assembler.push(chunk, now_ms) {
            Ok(CellGridChunkAssembly::Pending { .. }) => {
                self.target.canonical_chunk_in_flight = true;
                Admission::ChunkPending
            }
            Ok(CellGridChunkAssembly::Complete { frame, .. }) => {
                self.target.canonical_chunk_in_flight = false;
                match decode_assembled_frame(frame) {
                    Ok(decoded) => self.admit_decoded(decoded, token, now_ms),
                    Err(reason) => self.refuse_owned(reason, token, now_ms),
                }
            }
            Err(error) => {
                self.target.canonical_chunk_in_flight = false;
                let code = error.code.as_str();
                self.refuse(code, token, now_ms)
            }
        }
    }

    /// Sweep the chunk-stall deadline. Returns true when a partial was dropped.
    ///
    /// A partial that has not advanced for `CELL_GRID_CHUNK_STALL_MS` will never
    /// complete — the parts that would complete it are not coming — so it is
    /// dropped and the gap repaired from scratch rather than waited on. The
    /// boundary itself belongs to `roost-protocol`; this crate does not restate
    /// the number, because a client-side copy is a client-side second answer to
    /// "how long is too long".
    pub fn sweep(&mut self, now_ms: u64) -> bool {
        if !self.assembler.expire(now_ms) {
            return false;
        }
        self.target.canonical_chunk_in_flight = false;
        let stalled_token = self.latch.token().cloned();
        match stalled_token {
            Some(token) => {
                self.refuse("snapshot-stalled", &token, now_ms);
            }
            None => {
                // A partial with no gap latched behind it is still abandoned; the
                // replica simply waits for the next attempt.
                self.assembler.reset();
            }
        }
        true
    }

    /// Whether a latched repair should be requested now.
    pub fn repair_due(&self, token: &TerminalToken, now_ms: u64) -> bool {
        self.latch.should_send(Some(token), now_ms)
    }

    /// Record that a repair request went out.
    pub fn mark_repair_sent(&mut self, token: &TerminalToken, now_ms: u64) {
        self.latch.record_sent(token, now_ms);
    }

    /// Unbind the replica from its carrier generation, returning the one it had.
    ///
    /// The grid stays: what the reader sees does not change, only the claim
    /// about which carrier is delivering it. The next publication binds again.
    pub(crate) fn release_generation(&mut self) -> Option<TerminalToken> {
        self.generation.take()
    }

    /// Refuse with a contract reason string, which is `&'static` because the
    /// assembler's error codes and the fold's own vocabulary both are.
    fn refuse(&mut self, reason: &'static str, token: &TerminalToken, now_ms: u64) -> Admission {
        self.refuse_owned(reason.to_string(), token, now_ms)
    }

    /// Refuse with a diagnosis built at runtime, as a decode failure is.
    fn refuse_owned(&mut self, reason: String, token: &TerminalToken, now_ms: u64) -> Admission {
        let latched = self.latch.latch(&reason, token, now_ms);
        if latched {
            // One line per GAP, not per refusal: the frames that follow a refused
            // delta are the rest of the same gap.
            tracing::warn!(
                target: "terminal",
                session_id = %self.session_id,
                stream_id = %self.target.expected_stream_id.as_deref().unwrap_or(""),
                reason = %reason,
                "replica gap latched"
            );
            self.assembler.reset();
            self.target.canonical_chunk_in_flight = false;
        }
        Admission::Refused { reason, latched }
    }
}
