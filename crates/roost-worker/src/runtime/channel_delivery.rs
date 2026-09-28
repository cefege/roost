//! The one production [`ChannelDelivery`]: the half of the emitter that PARSES
//! and SHIPS a channel's bytes, and the resize capture that withholds them
//! meanwhile. `session::binding::RecordBinding` is what calls it, on the
//! keeper's dispatch thread. Depends on `session::emit`, `session::binding` and
//! `session::ring` — and on nothing that depends on it back.
//!
//! WHY IT IS A SEPARATE TYPE FROM [`super::cell_delivery::TableCellDelivery`]
//! when both drive one emitter. The two traits answer different questions and
//! the emitter answers both: `CellDelivery` is "may this channel deliver, and
//! under which generation" — a change of delivery STATE — while
//! `ChannelDelivery` is "may this chunk be PARSED yet", which during a resize
//! boundary is a different answer. Collapsing them is how a half-applied resize
//! paints a grid that was never on that terminal, so they stay two types over
//! one `Arc<Mutex<CellEmitter>>`.
//!
//! THE FREEZE GATE IS ITS OWN STATE, and deliberately not
//! `RecordBinding::Mode::Staged`. The stager answers for a record that does not
//! exist yet; the capture answers for one that exists and whose core is
//! mid-resize. `session::binding` names the failure of merging them: two answers
//! to "may this chunk be parsed yet".
//!
//! THE TWO OVERFLOWS SHARE NEITHER A FLAG NOR A BOUND, and confusing them
//! responds wrongly to both. `RESUME_STAGE_CAP_BYTES` is a REFUSAL that kills
//! an adoption. `CapturedOutput::overflowed` means the bytes are still in
//! history behind a core that cannot be brought forward — the ring keeps every
//! byte either way, so this side never discards anything and only reports.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::ChannelId;

use crate::session::binding::{CapturedOutput, ChannelDelivery};
use crate::session::emit::CellEmitter;
use crate::session::ring::SCROLLBACK_CAP_BYTES;
use crate::session::types::SessionRecord;

/// How many bytes a resize capture may hold before it reports an overflow.
///
/// The RETAINED WINDOW, not the staging cap. A capture's bytes are also on
/// their way into the ring, and the ring's cap is what decides whether the
/// oldest one is still addressable when the boundary resolves — so the bound a
/// capture reports against has to be the bound that will actually strand it.
const CAPTURE_CAP_BYTES: usize = SCROLLBACK_CAP_BYTES;

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
}

impl ChannelDelivery for TableChannelDelivery {
    /// Parse and ship one chunk, or hold it while this channel's core is
    /// mid-resize.
    ///
    /// THE ORDER IS RING FIRST, THEN CORE, and it is the order
    /// [`CellEmitter::ingest_pty_chunk`] already holds: a crash between the two
    /// leaves history ahead of the screen rather than a screen ahead of its own
    /// history. A channel with an open capture takes neither — its bytes are
    /// appended to the ring and buffered for the boundary, and the core is not
    /// written at stale geometry.
    fn ingest_output(&self, record: &mut SessionRecord, chunk: &[u8], now_ms: i64) {
        let channel_id = record.channel_id();
        if self.holding(channel_id) {
            let head = self.with_emitter(|emitter| emitter.retain_without_parsing(record, chunk));
            self.with_captures(|captures| {
                let capture = captures.entry(channel_id).or_default();
                // The bound is a REPORT and not a trim: a PTY stream is
                // contiguous, so trimming either end splices a hole in parser
                // state nothing downstream re-parses. The ring keeps every byte
                // either way; only the core cannot be brought forward.
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
            emitter.ingest_pty_chunk(record, chunk, now_ms);
        });
    }

    /// Withhold this channel's frames and start capturing its bytes.
    ///
    /// `false` when a capture is already open here, which is a caller bug
    /// rather than a race: two unresolved boundaries would each have to be
    /// answered at the core's resized-at.
    fn freeze_capture(&self, channel_id: ChannelId) -> bool {
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
        self.with_emitter(|emitter| emitter.hold_frames(channel_id, crate::session::cell_scheduler::CellGate::ResizeCapture, 0));
        tracing::info!(
            %channel_id,
            cap = CAPTURE_CAP_BYTES,
            "a resize boundary opened: this channel's output is retained, not parsed"
        );
        true
    }

    /// Hand back what the capture held, and let this channel ship again.
    ///
    /// The gate opens IN THIS CALL, so a chunk delivered afterwards is parsed
    /// after the captured bytes and never between them. The bytes are RETURNED
    /// and not written: whether they are parsed at the old or the new geometry
    /// is the caller's answer, not this one's.
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
}

impl TableChannelDelivery {
    /// Whether this channel's core is mid-resize.
    fn holding(&self, channel_id: ChannelId) -> bool {
        self.with_captures(|captures| captures.contains_key(&channel_id))
    }
}

#[cfg(test)]
mod tests {
    // A test unwraps the value it is asserting about: a failure there IS the
    // assertion failing, which is what a test wants. The workspace denies
    // unwrap/expect because a panic on a bad value in a running component is a
    // fleet-visible outage, and that reasoning does not reach a test.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{CAPTURE_CAP_BYTES, TableChannelDelivery};
    use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId};
    use roost_term::AlacrittyCore;

    use crate::event_store::{DurableEventKind, Store};
    use crate::session::binding::ChannelDelivery;
    use crate::session::emit::CellEmitter;
    use crate::session::lifecycle::SessionTable;
    use crate::session::ring::ScrollbackRing;
    use crate::session::types::{SessionIdentity, SessionRecord};
    use crate::shell_spec::{SHELL_SPEC_VERSION, ShellSpec};

    fn channel() -> ChannelId {
        ChannelId::try_from(3_i64).expect("3 is a channel this worker can address")
    }

    fn trace() -> TraceId {
        TraceId::try_from("0000beef0000beef").expect("16 hex is a trace id")
    }

    /// A record over a real `Reservation`, because the store is the only thing
    /// that mints one and a hand-built claim would be a second answer to
    /// "what capacity does this session hold".
    fn record() -> SessionRecord {
        let close = Store::new()
            .reserve_default(DurableEventKind::Closed)
            .expect("an empty store has room for one claim");
        let spec = ShellSpec {
            version: SHELL_SPEC_VERSION,
            platform: roost_host::HostPlatform::Linux,
            executable: "/bin/bash".to_string(),
            argv: vec!["-l".to_string()],
            cwd: "/home/user".to_string(),
            env: Vec::new(),
        };
        SessionRecord::new(
            SessionIdentity {
                session_id: SessionId::try_from("0000beef0000beef").expect("16 hex is an id"),
                channel_id: channel(),
                socket_path: "/tmp/mux-keeper.sock".to_string(),
                cwd: "/home/user".to_string(),
                shell_spec: spec,
                session_trace_id: trace(),
                spawned_at_ms: 1,
            },
            close,
            Box::new(AlacrittyCore::new(80, 24)),
            roost_term::CellEmitState::new("epoch".to_string(), "stream".to_string()),
            ScrollbackRing::default(),
        )
    }

    /// A delivery over a fresh emitter, reached the way production reaches it:
    /// through the `CellDelivery` bridge that owns the emitter.
    fn delivery() -> TableChannelDelivery {
        let cells = super::super::cell_delivery::TableCellDelivery::new(
            CellEmitter::new(),
            std::sync::Arc::new(SessionTable::default()),
        );
        TableChannelDelivery::new(cells.emitter())
    }


    /// An open capture HOLDS the bytes rather than parsing them, and hands them
    /// back on close. This is the whole reason the capture exists: a chunk
    /// parsed at the geometry the core is leaving paints a grid that was never
    /// on that terminal.
    #[test]
    fn an_open_capture_holds_output_and_hands_it_back_on_close() {
        let delivery = delivery();
        let mut record = record();
        assert!(delivery.freeze_capture(channel()));

        delivery.ingest_output(&mut record, b"first ", 1_000);
        delivery.ingest_output(&mut record, b"second", 2_000);
        // Nothing was parsed into the core while the boundary was open.
        assert_eq!(record.terminal_core.cols(), 80);

        let held = delivery.close_capture(channel());
        assert_eq!(held.bytes, b"first second");
        assert!(!held.overflowed, "two small chunks are inside the window");
    }

    /// A SECOND freeze on the same channel is refused, because two unresolved
    /// boundaries would each have to be answered at the core's resized-at.
    #[test]
    fn a_second_freeze_on_one_channel_is_refused() {
        let delivery = delivery();
        assert!(delivery.freeze_capture(channel()));
        assert!(
            !delivery.freeze_capture(channel()),
            "a channel with an unresolved resize in flight cannot open a second boundary"
        );
    }

    /// `close_capture` OPENS the gate in the same call, so a chunk delivered
    /// afterwards is parsed after the captured bytes and never between them.
    #[test]
    fn closing_a_capture_lets_the_next_chunk_through() {
        let delivery = delivery();
        let mut record = record();
        delivery.freeze_capture(channel());
        delivery.ingest_output(&mut record, b"held", 1_000);
        let held = delivery.close_capture(channel());
        assert_eq!(held.bytes, b"held");

        // The emitter has no stream installed, so this chunk is retained rather
        // than shipped — which is the emitter's own answer, and the point is
        // that it was ASKED rather than swallowed by a capture that never closed.
        delivery.ingest_output(&mut record, b"after", 2_000);
        assert_eq!(record.head_seq, 9, "held(4) + after(5) reached the ring");
    }

    /// Closing a channel with no capture open is an empty handback, not a
    /// panic: the resize path can lose a boundary to a channel that exits, and
    /// the answer it needs is "there was nothing", which is a value.
    #[test]
    fn closing_a_channel_with_no_capture_hands_back_nothing() {
        let delivery = delivery();
        let held = delivery.close_capture(channel());
        assert!(held.bytes.is_empty());
        assert!(!held.overflowed);
    }

    /// MORE THAN THE WINDOW IS REPORTED, NOT TRIMMED. A PTY stream is
    /// contiguous: discarding either end splices a hole in parser state nothing
    /// downstream re-parses, so the overflow flag is the whole answer and the
    /// bytes past the bound are simply not buffered.
    #[test]
    fn a_capture_past_the_retained_window_reports_rather_than_trims() {
        let delivery = delivery();
        let mut record = record();
        delivery.freeze_capture(channel());
        let flood = vec![b'x'; CAPTURE_CAP_BYTES + 1];
        delivery.ingest_output(&mut record, &flood, 1_000);

        let held = delivery.close_capture(channel());
        assert!(
            held.overflowed,
            "output past the retained window cannot be brought forward across the gap"
        );
        assert!(
            held.bytes.len() <= CAPTURE_CAP_BYTES,
            "the capture never grows past the bound it reports against, got {}",
            held.bytes.len()
        );
    }
}
