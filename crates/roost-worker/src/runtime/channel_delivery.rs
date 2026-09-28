//! The one production [`ChannelDelivery`]: the half of the emitter that PARSES
//! and SHIPS a channel's bytes, the resize capture that withholds them
//! meanwhile, and the [`StreamEmission`] a stream transaction mints and traps
//! through. `session::binding::RecordBinding` calls it on the keeper's dispatch
//! thread; `session::terminal_txn` and `session::resize` under the same lock.
//! Ports the lane choice of `apps/worker/src/session/session-emit.ts`
//! (`emitUpstreamChunk`) and `session-resize-capture.ts`. Depends on
//! `session::emit`, `session::binding` and `session::ring`.
//!
//! WHY IT IS A SEPARATE TYPE FROM [`super::cell_delivery::TableCellDelivery`]
//! when both drive one emitter. `CellDelivery` is "may this channel deliver" —
//! a change of delivery STATE with no record in hand — while this is "may this
//! chunk be PARSED yet", which during a resize boundary or after a core trap is
//! a different answer. Collapsing them is how a half-applied resize paints a
//! grid that was never on that terminal, so they stay two types over one
//! `Arc<Mutex<CellEmitter>>`.
//!
//! THREE LANES, ONE DECISION. An open capture retains and buffers; a trapped
//! core retains only (v2 `appendCapturedScrollback`: those bytes never reach a
//! core whose parser state is already wrong); everything else parses. The ring
//! keeps every byte in all three, so a capture's overflow is a REPORT, never a
//! trim — a PTY stream is contiguous and a hole in it is never re-parsed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::ChannelId;

use crate::session::binding::{CapturedOutput, ChannelDelivery};
use crate::session::emit::CellEmitter;
use crate::session::ring::SCROLLBACK_CAP_BYTES;
use crate::session::terminal_state::StreamEmission;
use crate::session::types::SessionRecord;

/// How many bytes a resize capture may hold before it reports an overflow.
///
/// The RETAINED WINDOW, not the staging cap. A capture's bytes are also on
/// their way into the ring, and the ring's cap is what decides whether the
/// oldest one is still addressable when the boundary resolves — so the bound a
/// capture reports against has to be the bound that will actually strand it.
pub const CAPTURE_CAP_BYTES: usize = SCROLLBACK_CAP_BYTES;

/// One channel's held bytes, and whether more arrived than the window holds.
#[derive(Debug, Default)]
struct Capture {
    bytes: Vec<u8>,
    overflowed: bool,
}

/// `ChannelDelivery` over the shared emitter, plus the capture each open resize
/// boundary is holding.
pub struct TableChannelDelivery {
    emitter: Arc<Mutex<CellEmitter>>,
    captures: Mutex<HashMap<ChannelId, Capture>>,
}

impl std::fmt::Debug for TableChannelDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self
            .captures
            .lock()
            .map(|captures| captures.len())
            .unwrap_or_default();
        formatter
            .debug_struct("TableChannelDelivery")
            .field("open_captures", &held)
            .finish_non_exhaustive()
    }
}

impl TableChannelDelivery {
    /// The bridge over the emitter [`super::cell_delivery::TableCellDelivery`]
    /// already holds.
    pub fn new(emitter: Arc<Mutex<CellEmitter>>) -> Self {
        Self {
            emitter,
            captures: Mutex::new(HashMap::new()),
        }
    }

    fn with_emitter<R>(&self, call: impl FnOnce(&mut CellEmitter) -> R) -> R {
        let mut emitter = self
            .emitter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        call(&mut emitter)
    }

    fn with_captures<R>(&self, call: impl FnOnce(&mut HashMap<ChannelId, Capture>) -> R) -> R {
        let mut captures = self
            .captures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        call(&mut captures)
    }

    /// Whether this channel's core is mid-resize.
    fn holding(&self, channel_id: ChannelId) -> bool {
        self.with_captures(|captures| captures.contains_key(&channel_id))
    }
}

impl ChannelDelivery for TableChannelDelivery {
    /// Parse and ship one chunk, or hold it while this channel's core is
    /// mid-resize, or only retain it while the core is trapped.
    ///
    /// THE ORDER IS RING FIRST, THEN CORE, and it is the order
    /// [`CellEmitter::ingest_pty_chunk`] already holds: a crash between the two
    /// leaves history ahead of the screen rather than a screen ahead of its own
    /// history.
    fn ingest_output(&self, record: &mut SessionRecord, chunk: &[u8], now_ms: i64) {
        let channel_id = record.channel_id();
        if self.holding(channel_id) {
            let head =
                self.with_emitter(|emitter| emitter.retain_without_parsing(record, chunk, now_ms));
            self.with_captures(|captures| {
                let capture = captures.entry(channel_id).or_default();
                if capture.bytes.len().saturating_add(chunk.len()) > CAPTURE_CAP_BYTES {
                    capture.overflowed = true;
                } else {
                    capture.bytes.extend_from_slice(chunk);
                }
                tracing::debug!(
                    %channel_id,
                    head,
                    len = chunk.len(),
                    held = capture.bytes.len(),
                    overflowed = capture.overflowed,
                    "a pty chunk was retained for an unresolved resize boundary"
                );
            });
            return;
        }
        self.with_emitter(|emitter| {
            if emitter.stream_core_trapped(channel_id) {
                let head = emitter.retain_without_parsing(record, chunk, now_ms);
                tracing::trace!(%channel_id, head, len = chunk.len(), "a pty chunk was retained behind a trapped core");
                return;
            }
            emitter.ingest_pty_chunk(record, chunk, now_ms);
        });
    }

    /// Withhold this channel's frames and start capturing its bytes (v2
    /// `installLiveResizeCapture`: the gate, no queued emission, no open
    /// synchronized-output hold).
    ///
    /// `false` when a capture is already open here, which is a caller bug
    /// rather than a race: two unresolved boundaries would each have to be
    /// answered at the core's resized-at.
    fn freeze_capture(&self, channel_id: ChannelId, now_ms: i64) -> bool {
        let opened = self.with_captures(|captures| {
            if captures.contains_key(&channel_id) {
                return false;
            }
            captures.insert(channel_id, Capture::default());
            true
        });
        if !opened {
            return false;
        }
        self.with_emitter(|emitter| {
            emitter.hold_frames(
                channel_id,
                crate::session::cell_gates::CellGate::ResizeCapture,
                now_ms,
            );
            emitter.cancel_cell_emission(channel_id);
            emitter.release_sync_output_hold(channel_id);
        });
        tracing::info!(
            %channel_id,
            cap = CAPTURE_CAP_BYTES,
            "a resize boundary opened: this channel's output is retained, not parsed"
        );
        true
    }

    /// Hand back what the capture held, and give back the emission gate.
    ///
    /// The gate opens IN THIS CALL, on every outcome — resolved or trapped — so
    /// a capture nothing can finish never suppresses the channel for good
    /// (`docs/FAILURE-INDEX.md`, "A trapped resize capture suppresses a
    /// channel's emission for good"). The bytes are RETURNED and not written:
    /// whether they are parsed, and at which geometry, is the caller's answer.
    fn close_capture(&self, channel_id: ChannelId) -> CapturedOutput {
        let capture = self
            .with_captures(|captures| captures.remove(&channel_id))
            .unwrap_or_default();
        self.with_emitter(|emitter| emitter.release_frames(channel_id));
        tracing::info!(
            %channel_id,
            bytes = capture.bytes.len(),
            overflowed = capture.overflowed,
            "a resize boundary closed and its held output was handed back"
        );
        CapturedOutput {
            bytes: capture.bytes,
            overflowed: capture.overflowed,
        }
    }

    fn stream_emission(&self) -> Option<&dyn StreamEmission> {
        Some(self)
    }
}

impl StreamEmission for TableChannelDelivery {
    fn mint_stream(
        &self,
        record: &mut SessionRecord,
        stream_id: &str,
        enabled: bool,
        geometry_changed: bool,
    ) {
        self.with_emitter(|emitter| {
            emitter.mint_stream(record, stream_id, enabled, geometry_changed)
        });
    }

    fn core_valid(&self, channel_id: ChannelId) -> bool {
        self.with_emitter(|emitter| emitter.stream_core_valid(channel_id))
    }

    fn install_baseline(&self, record: &mut SessionRecord, now_ms: i64) -> bool {
        let channel_id = record.channel_id();
        self.with_emitter(|emitter| {
            let outcome = emitter.install_terminal_baseline(record, now_ms);
            tracing::debug!(%channel_id, ?outcome, "a terminal baseline was requested");
            emitter.stream_core_valid(channel_id)
        })
    }

    fn retire_delivery(&self, channel_id: ChannelId) {
        self.with_emitter(|emitter| {
            emitter.retire_stream_delivery(channel_id);
            emitter.clear_stream_delivery_dirty(channel_id);
        });
    }

    fn reset_delivery(&self, channel_id: ChannelId) {
        self.with_emitter(|emitter| emitter.reset_stream_delivery(channel_id));
    }

    fn trap_core(&self, channel_id: ChannelId) {
        self.with_emitter(|emitter| emitter.trap_stream_core(channel_id));
    }

    fn prove_core(&self, channel_id: ChannelId) {
        self.with_emitter(|emitter| emitter.set_core_valid(channel_id, true));
        tracing::info!(%channel_id, "a re-proved terminal core may emit again");
    }

    fn forward_query_replies(&self, record: &SessionRecord, replies: String) {
        if replies.is_empty() {
            return;
        }
        let session_id = record.session_id();
        self.with_emitter(|emitter| emitter.send_query_replies(session_id, replies.into_bytes()));
    }
}
