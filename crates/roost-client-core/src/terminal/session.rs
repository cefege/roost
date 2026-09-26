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

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::cell::{CellGridChunkAssembler, CellGridChunkAssembly, CellGridFrame};

use crate::terminal::frame_fold::{
    FoldTarget, FrameFoldOutcome, decode_chunk_part, decode_wire_frame, fold,
};
use crate::terminal::repair::RepairLatch;
use crate::terminal::token::TerminalToken;
use crate::terminal::view::TerminalView;

/// What admitting one frame did, for the host that renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Nothing changed. `latched` says whether this call is the one that latched
    /// the repair; the replica is untouched either way.
    Refused {
        /// The contract reason, or the diagnosis when there is no contract code.
        reason: String,
        /// True when this call is the one that latched.
        latched: bool,
    },
    /// A complete baseline replaced the replica. The host may repaint from
    /// `canonical()`; renderers keep their last complete DOM until then.
    BaselineReplaced,
    /// A delta extended the replica. The host may apply the row changes to the
    /// rows it already painted.
    DeltaApplied,
    /// A chunk is still assembling; nothing to paint yet.
    ChunkPending,
}

/// What a view-state result did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewStateAdmission {
    /// The result belongs to a view that is not awaiting this generation. It is
    /// the answer to a command from a socket that has been replaced, and it
    /// changes nothing.
    Stale,
    /// The authority does not hold the view.
    Refused,
    /// The authority holds the view, and is minting this stream.
    Accepted {
        /// The stream the authority is now on, when it named one.
        stream_id: Option<String>,
    },
}

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
    /// How many frames have been APPLIED to this replica.
    ///
    /// A renderer repaints when this moved, and it is per session rather than
    /// one store-wide counter because the coordinator delivers a frame per pane
    /// per tick: on a four-pane board a store-wide counter repaints all four for
    /// each of the four. This is the "repaint generation" v2 froze into history
    /// when it inferred scrolled-off rows from scrollback growth — the reason
    /// that class of bug exists is that the inference had no counter to consult.
    ///
    /// It moves only where the CANONICAL grid moves: a full that replaced the
    /// replica, and a delta that extended it. A refused frame, a chunk still
    /// assembling, and a dropped stalled partial all change the replica's
    /// bookkeeping and none of them change a cell.
    frame_revision: u64,
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
            generation: None,
            views: BTreeMap::new(),
            wire_stream_id: None,
            wire_grid_epoch: None,
            wire_seq: None,
            frame_revision: 0,
        }
    }

    /// The replica, for a host that renders it.
    pub fn canonical(&self) -> Option<&CellGridFrame> {
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
        self.frame_revision
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
            return stale(token);
        }
        if frame.session_id != self.session_id {
            return self.refuse("terminal frame session mismatch", token, now_ms);
        }
        if Some(frame.stream_id.as_str()) != self.target.expected_stream_id.as_deref() {
            // Not this replica's frame. Latching a repair for it would ask the
            // authority for a baseline it already sent and this client ignored,
            // which is how one stale frame turns into a request storm.
            return stale(token);
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
                self.frame_revision += 1;
                tracing::info!(
                    target: "terminal",
                    session_id = %self.session_id,
                    stream_id = %self.wire_stream_id.as_deref().unwrap_or(""),
                    "replica baseline replaced"
                );
                Admission::BaselineReplaced
            }
            FrameFoldOutcome::Delta { .. } => {
                self.frame_revision += 1;
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
            return stale(token);
        }
        let Some(part) = decode_chunk_part(chunk) else {
            return self.refuse("missing-part", token, now_ms);
        };
        if part.session_id != self.session_id {
            return self.refuse("terminal frame session mismatch", token, now_ms);
        }
        if Some(part.stream_id.as_str()) != self.target.expected_stream_id.as_deref() {
            return stale(token);
        }
        self.note_wire(&part.stream_id, &part.grid_epoch, part.seq);
        match self.assembler.push(chunk, now_ms) {
            Ok(CellGridChunkAssembly::Pending { .. }) => {
                self.target.canonical_chunk_in_flight = true;
                Admission::ChunkPending
            }
            Ok(CellGridChunkAssembly::Complete { frame, .. }) => {
                self.target.canonical_chunk_in_flight = false;
                match decode_wire_frame(&frame, true) {
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

    fn note_wire(&mut self, stream_id: &str, grid_epoch: &str, seq: u64) {
        self.wire_stream_id = Some(stream_id.to_string());
        self.wire_grid_epoch = Some(grid_epoch.to_string());
        self.wire_seq = Some(seq);
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

/// A frame from a generation this replica is not bound to. Never latches: the
/// replica is not damaged, it is simply not the frame's destination.
fn stale(token: &TerminalToken) -> Admission {
    Admission::Refused {
        reason: format!("generation {} is not current", token.socket_generation),
        latched: false,
    }
}
