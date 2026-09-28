//! Repairs a terminal core from the keeper's ORDERED history: re-proving a
//! fail-closed core in place so its stream can emit again, and settling a
//! resize whose acknowledgement was lost. `session::terminal_txn` is the only
//! caller of both, from a blocking thread (every keeper read here waits).
//! Ports `apps/worker/src/session/session-core-reprove.ts` and
//! `recoverAmbiguousResize` of `session-resize-capture.ts`.
//!
//! NOT THE REBUILD-FROM-RING-ON-RESIZE PATH `docs/FAILURE-INDEX.md` condemns
//! ("A live viewport change rebuilds the terminal core"): a provable resize
//! still resizes the core it owns (`session::resize`), and a re-proof runs only
//! while the core is ALREADY fail-closed — the one state in which the live core
//! is missing bytes and only the keeper's history can fill them
//! ("A terminal never repaints again after the device that opened it went away").

use std::sync::{Arc, Mutex};

use roost_keeper::history::HistoryRecord;
use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry};
use roost_protocol::wire::brand::ChannelId;
use roost_term::{AlacrittyCore, CellEmitState, TerminalCore};

use super::binding::ChannelDelivery;
use super::ids::mint_uuid;
use super::keeper_channels::SurvivorHistory;
use super::lifecycle::SessionManager;
use super::query_reply::answer_queries;
use super::replay_align::skip_orphan_sequence_prefix;
use super::resize::{CaptureLoss, OpenBoundary, lock, report_capture_loss, trap_boundary};
use super::ring::ScrollbackRing;
use super::stream_scan::ALT_ENTER_SEQUENCES;
use super::types::SessionRecord;
use crate::terminal_core_capacity::TerminalCoreAllocationKind;

const SUPERSEDED: &str = "terminal stream was superseded during core re-proof";

/// Why a re-proof did not happen. `retryable` is v2's: a newer desire, a closed
/// session or a capacity refusal is `retryable_pre_write`, anything the keeper
/// or the replay disproved is `core_failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReproofRefusal {
    pub reason: String,
    pub retryable: bool,
}

/// A lost acknowledgement recovery could not settle. `trapped` is v2's
/// `capture.failedReason`: a trapped core is re-provable (`core_failed`); an
/// untrapped one lost its session mid-recovery (`ambiguous_boundary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrecovered {
    pub reason: String,
    pub trapped: bool,
}

/// The generation a re-proof is on behalf of.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReproofFor<'a> {
    pub version: u64,
    pub stream_id: &'a str,
}

impl SessionManager {
    /// Rebuild a fail-closed channel's core from the keeper's history and swap
    /// it in (v2 `reproveTerminalCore`). BLOCKING.
    pub(super) fn reprove_terminal_core(
        &self,
        channel_id: ChannelId,
        generation: ReproofFor<'_>,
    ) -> Result<(), ReproofRefusal> {
        let raw = channel_id.as_u32() as u16;
        let refuse =
            |reason: String, retryable: bool| refusal(channel_id, generation, reason, retryable);
        let Some(entry) = self.sessions.entry(raw) else {
            return Err(refuse("session is not live".to_owned(), true));
        };
        let history = self.keeper.channel_history(raw).map_err(|fault| {
            refuse(
                format!("keeper history unavailable: {}", fault.reason),
                false,
            )
        })?;
        let applied = self.keeper.terminal_state(raw).map_err(|fault| {
            refuse(
                format!("keeper terminal state unavailable: {}", fault.reason),
                false,
            )
        })?;
        if !geometry_ok(applied.cols, applied.rows) {
            return Err(refuse(
                "keeper did not report terminal geometry".to_owned(),
                false,
            ));
        }
        let current = |entry: &Arc<Mutex<SessionRecord>>| {
            self.sessions
                .entry(raw)
                .is_some_and(|now| Arc::ptr_eq(&now, entry))
                && self
                    .terminal_streams
                    .is_current(channel_id, generation.version)
        };
        if !current(&entry) {
            return Err(refuse(SUPERSEDED.to_owned(), true));
        }
        if !geometry_ok(history.base_cols, history.base_rows) {
            return Err(refuse(
                "keeper history reported invalid base geometry".to_owned(),
                false,
            ));
        }
        let lease = self
            .core_capacity
            .reserve(TerminalCoreAllocationKind::Replacement)
            .map_err(|error| refuse(format!("terminal core reservation refused: {error}"), true))?;
        let mut core = AlacrittyCore::new(history.base_cols, history.base_rows);
        if (core.cols(), core.rows()) != (history.base_cols, history.base_rows) {
            return Err(refuse(
                "terminal core did not retain the keeper's base geometry".to_owned(),
                false,
            ));
        }
        let epoch = mint_uuid()
            .map_err(|error| refuse(format!("a grid epoch could not be minted: {error}"), false))?;
        if !current(&entry) {
            return Err(refuse(SUPERSEDED.to_owned(), true));
        }
        // Record and delivery locks from here to the swap: no chunk can land
        // between the ring read below and the new ring taking over, which is
        // what makes the tail splice exact.
        let mut record = lock(&entry);
        let delivery = lock(&self.ingest);
        let replay = replay_history(
            &mut core,
            &history,
            &record.scrollback.to_vec(),
            record.head_seq,
        )
        .map_err(|reason| refuse(reason, false))?;
        if (core.cols(), core.rows()) != (applied.cols, applied.rows) {
            let reason = "keeper history did not converge to the keeper's reported geometry";
            return Err(refuse(reason.to_owned(), false));
        }
        // `alt_mode` is STREAM truth: the retain lane kept scanning while the
        // core was frozen, so it needs no rescan of the window.
        if record.alt_mode && !core.using_alt_screen() {
            core.write(ALT_ENTER_SEQUENCES[0]);
        }
        self.core_capacity
            .replace_channel_lease(raw, lease)
            .map_err(|misuse| {
                refuse(
                    format!("terminal core lease could not be swapped: {misuse}"),
                    false,
                )
            })?;
        let (cols, rows) = (core.cols(), core.rows());
        record.terminal_core = Box::new(core);
        record.scrollback = ScrollbackRing::default();
        record.adopt_retained_history(&replay.window, replay.head_seq);
        // The unhandled-sequence mark is per CORE instance; a retained one would
        // mute the fresh core for good.
        record.unhandled = None;
        // A NEW grid identity, not a revision: re-derived history must make a
        // browser renumber rather than merge into rows it still holds.
        record.cell_emit = CellEmitState::new(epoch, generation.stream_id);
        match delivery.stream_emission() {
            Some(emission) => emission.prove_core(channel_id),
            None => tracing::error!(%channel_id, "a re-proved core has no emitter to clear"),
        }
        // The stream table is taken BEFORE record and delivery elsewhere, so it
        // is only touched here once both are released.
        drop(delivery);
        drop(record);
        self.terminal_streams
            .note_applied_size(channel_id, cols, rows);
        tracing::info!(
            %channel_id,
            stream_id = generation.stream_id,
            cols,
            rows,
            replayed_bytes = replay.replayed_bytes,
            tail_bytes = replay.tail_bytes,
            head_seq = replay.head_seq,
            history_evicted = replay.history_evicted,
            "terminal.core_reproved: a fail-closed core was rebuilt from keeper history"
        );
        Ok(())
    }

    /// Settle a resize whose acknowledgement was lost (v2
    /// `recoverAmbiguousResize`): replay only the suffix the existing core has
    /// not parsed, resize that SAME core at the retained marker, and trap the
    /// core when the boundary cannot be found. BLOCKING.
    pub(super) fn recover_lost_ack(
        &self,
        channel_id: ChannelId,
        boundary: &OpenBoundary,
    ) -> Result<(), Unrecovered> {
        let raw = channel_id.as_u32() as u16;
        let history = self.keeper.channel_history(raw);
        let Some(entry) = self.sessions.entry(raw) else {
            lock(&self.ingest).close_capture(channel_id);
            tracing::info!(%channel_id, seq = boundary.seq, "a lost resize answer's session closed during recovery");
            let reason = "session closed during resize recovery".to_owned();
            return Err(Unrecovered {
                reason,
                trapped: false,
            });
        };
        let loss = {
            let mut record = lock(&entry);
            let delivery = lock(&self.ingest);
            let held = delivery.close_capture(channel_id);
            let settled = history
                .map_err(|fault| format!("ordered resize history unavailable: {}", fault.reason))
                .and_then(|history| {
                    self.replay_recovery(&mut record, &*delivery, &history, boundary, &held)
                });
            match settled {
                Ok((loss, replies)) => {
                    if let Some(emission) = delivery.stream_emission() {
                        emission.forward_query_replies(&record, replies);
                    }
                    loss
                }
                Err(reason) => {
                    trap_boundary(&record, &*delivery, boundary, &reason);
                    return Err(Unrecovered {
                        reason,
                        trapped: true,
                    });
                }
            }
        };
        report_capture_loss(raw, loss);
        tracing::info!(%channel_id, seq = boundary.seq, "a lost resize answer was recovered from ordered keeper history");
        Ok(())
    }

    fn replay_recovery(
        &self,
        record: &mut SessionRecord,
        delivery: &dyn ChannelDelivery,
        history: &SurvivorHistory,
        boundary: &OpenBoundary,
        held: &super::binding::CapturedOutput,
    ) -> Result<(Option<CaptureLoss>, String), String> {
        let retained_start = history
            .head_seq
            .saturating_sub(history.window().len() as u64);
        if boundary.install_seq < retained_start {
            return Err("ordered resize boundary was evicted".to_owned());
        }
        let mut output_seq = retained_start;
        let mut applied = None;
        // Replayed probes are answered from the capture's own carry, and the
        // replies go out after the resize, as at an answered boundary.
        let mut carry = boundary.query_carry.clone();
        let mut replies = String::new();
        for entry in &history.records {
            match entry {
                HistoryRecord::Output { bytes, .. } => {
                    let end = output_seq + bytes.len() as u64;
                    if end > boundary.install_seq {
                        let from = boundary.install_seq.saturating_sub(output_seq) as usize;
                        let core: Option<&mut dyn TerminalCore> =
                            Some(record.terminal_core.as_mut());
                        replies.push_str(&answer_queries(&mut carry, core, &bytes[from..]).bytes);
                    }
                    output_seq = end;
                }
                HistoryRecord::Resize { seq, cols, rows } if *seq == boundary.seq => {
                    if applied.is_some() {
                        return Err("duplicate resize boundary in ordered history".to_owned());
                    }
                    if (*cols, *rows) != boundary.to {
                        return Err(
                            "ordered history contained conflicting resize geometry".to_owned()
                        );
                    }
                    applied = Some(self.resize_in_place(record, delivery, (*cols, *rows))?);
                }
                HistoryRecord::Resize { seq, .. } if output_seq > boundary.install_seq => {
                    return Err(format!("unexpected resize {seq} inside recovery suffix"));
                }
                HistoryRecord::Resize { .. } => {}
            }
        }
        let loss = applied.ok_or_else(|| "ordered resize boundary was not retained".to_owned())?;
        // Bytes past the keeper's head exist only in the capture: splice them on
        // rather than leave a hole at the head of the recovered core.
        let tail = record.head_seq.saturating_sub(history.head_seq) as usize;
        if tail > 0 {
            if held.overflowed || tail > held.bytes.len() {
                return Err(
                    "captured tail was evicted before the boundary could be recovered".to_owned(),
                );
            }
            let core: Option<&mut dyn TerminalCore> = Some(record.terminal_core.as_mut());
            let tail_bytes = &held.bytes[held.bytes.len() - tail..];
            replies.push_str(&answer_queries(&mut carry, core, tail_bytes).bytes);
        }
        Ok((loss, replies))
    }
}

/// The rebuilt window and what the replay did.
struct HistoryReplay {
    window: Vec<u8>,
    head_seq: u64,
    replayed_bytes: usize,
    tail_bytes: usize,
    history_evicted: bool,
}

/// Replay the keeper's records into a cold core, marker by marker, then splice
/// on whatever this worker retained past the keeper's head (v2
/// `replayKeeperHistory`). Synchronous by contract: the caller holds the ring.
fn replay_history(
    core: &mut AlacrittyCore,
    history: &SurvivorHistory,
    retained: &[u8],
    live_head_seq: u64,
) -> Result<HistoryReplay, String> {
    let mut window = Vec::new();
    let history_evicted = history.evicted();
    // Only the cold core's FIRST write under eviction can open mid-sequence.
    let mut drop_orphan_prefix = history_evicted;
    for entry in &history.records {
        match entry {
            HistoryRecord::Output { bytes, .. } => {
                window.extend_from_slice(bytes);
                let from = if drop_orphan_prefix {
                    skip_orphan_sequence_prefix(bytes)
                } else {
                    0
                };
                core.write(&bytes[from..]);
                drop_orphan_prefix = false;
            }
            HistoryRecord::Resize { cols, rows, .. } => {
                if !geometry_ok(*cols, *rows) {
                    return Err("keeper history contains invalid resize geometry".to_owned());
                }
                core.resize(*cols, *rows);
                if (core.cols(), core.rows()) != (*cols, *rows) {
                    return Err("terminal core did not retain a keeper history resize".to_owned());
                }
            }
        }
    }
    let replayed_bytes = window.len();
    let tail_bytes = live_head_seq.saturating_sub(history.head_seq) as usize;
    if tail_bytes > 0 {
        if tail_bytes > retained.len() {
            return Err("captured tail was evicted before the core could be re-proved".to_owned());
        }
        let tail = &retained[retained.len() - tail_bytes..];
        window.extend_from_slice(tail);
        core.write(tail);
    }
    Ok(HistoryReplay {
        window,
        // The keeper's ordered read may be ahead of this worker's head, and
        // the rebuilt window ends at whichever head reaches further.
        head_seq: history.head_seq.max(live_head_seq),
        replayed_bytes,
        tail_bytes,
        history_evicted,
    })
}

fn geometry_ok(cols: u16, rows: u16) -> bool {
    is_terminal_geometry(&TerminalGeometry {
        cols: u32::from(cols),
        rows: u32::from(rows),
    })
}

/// Every refusal is one warn line plus the verdict, so no path reports one
/// without the other.
fn refusal(
    channel_id: ChannelId,
    generation: ReproofFor<'_>,
    reason: String,
    retryable: bool,
) -> ReproofRefusal {
    tracing::warn!(%channel_id, stream_id = generation.stream_id, %reason, retryable, "terminal_core_reproof_failed");
    ReproofRefusal { reason, retryable }
}
