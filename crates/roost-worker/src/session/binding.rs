//! The object a keeper delivers a channel's bytes into, and the two states it
//! holds them in. `keeper_pool` holds one of these per channel and
//! `SessionManager` installs and closes it; `session::resume` starts a survivor
//! here. Depends on `super::lifecycle` for the table, `super::sinks` for the
//! binding contract and `roost_observability` for the clock — and on nothing
//! that depends on it back.
//!
//! WHY IT IS ITS OWN FILE. Two invariants live here and neither is visible from
//! the call sites. A channel's output is HELD until the record it belongs to
//! exists, and the hold is released in ONE critical section that replays what it
//! held — so the swap an adoption performs is atomic in stream terms, and a
//! chunk the keeper delivers the instant after the flip is parsed after the
//! staged bytes and never between them. And the hold is BOUNDED, where past the
//! bound the whole delivery is refused rather than trimmed, because a PTY stream
//! is contiguous: discarding either end splices a hole into parser state nothing
//! downstream re-parses. Both are properties of the OBJECT rather than of any
//! one caller, which is why burying them in the adoption file would make them
//! the adoption's business instead of the worker's.
//!
//! THE BOUND IS CHECKED TWICE, ON PURPOSE. Once where the bytes arrive, so the
//! buffer cannot grow without limit, and once at the moment of the swap, so a
//! buffer that overflowed can never be replayed as though it had not.

use std::sync::{Arc, Mutex, MutexGuard};

use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::ChannelId;

use super::binding_staging::{Held, Mode, Staging};
use super::lifecycle::SessionTable;
use super::sinks::ChannelBinding;
use super::types::SessionRecord;

/// How many bytes of output may be held for a record that does not exist yet.
///
/// A survivor is adopted, and a shell is spawned, inside one synchronous window,
/// so this is what a chatty build can emit while a core is being rebuilt or
/// installed. A REFUSAL bound and not a buffer bound: past it the stream cannot
/// be delivered whole, and a truncated delivery is a hole in the parser rather
/// than a smaller window.
pub const RESUME_STAGE_CAP_BYTES: usize = 256 * 1024;

/// The per-channel cell delivery registration the lifecycle owes the emitter.
/// Both facts a channel's delivery has — that it started, and that it is over —
/// are transitions the session layer owns, so both are announced through here.
///
/// `session::emit::CellEmitter` is the DELIVERY SURFACE, and that is a weaker
/// claim than "the implementation": it holds the state these two calls change
/// (`emit_streams::StreamOutput`), and it exposes the changes as inherent
/// methods. It does not implement this trait, because the inherent forms take
/// what the state needs — `install_stream` there reads a `&mut SessionRecord`
/// to pick up the core, and this trait's signature has no record. The wiring
/// that bridges the two is `runtime`'s, and it is the only place allowed to
/// hold both halves.
pub trait CellDelivery: Send + Sync {
    /// A channel's stream generation, so deltas can flow to it.
    fn install_stream(&mut self, channel_id: ChannelId, stream_id: &str);
    /// A channel is gone; its delivery state and parked cursors go with it.
    fn forget_channel(&mut self, channel_id: ChannelId);
    /// v2 `markInputSensitive`: the next echo chunk leads instead of waiting out
    /// the coalesce window. A channel the table does not hold is ignored.
    fn note_input_echo(&mut self, channel_id: ChannelId);
    /// v2 `cancelCellEmission`: the channel's queued emission only.
    fn cancel_cell_emission(&mut self, channel_id: ChannelId);
    /// v2 `_releaseSyncOutputHold`: the core the hold is expressed in froze or
    /// was replaced.
    fn release_sync_output_hold(&mut self, channel_id: ChannelId);
}

/// What a frozen core's capture held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturedOutput {
    /// The bytes, oldest first and in delivery order.
    pub bytes: Vec<u8>,
    /// More arrived than the retained window holds, so they were retained in
    /// history but the core cannot be brought forward to them.
    pub overflowed: bool,
}

/// What a channel's bytes are parsed and shipped through.
///
/// Named apart from [`CellDelivery`] because a CAPTURE is not
/// a delivery change: freezing a core is a stream-state question, and a second
/// answer to "may this chunk be parsed yet" is how a half-applied resize paints
/// a grid that was never on that terminal. `session::emit::CellEmitter` is the
/// delivery surface for this one too, and again does not implement it — see
/// the note on [`CellDelivery`]. The staged/ordered half is here, in
/// [`RecordBinding`], which is what makes the ordering guarantee total: bytes
/// for a record that does not exist yet are held, and the trait is what
/// decides whether they may be parsed.
/// THREE METHODS, AND TWO OF THE FIVE ARE GONE BECAUSE v2 HAS TWO HANDLERS.
///
/// A port that collapses two v2 paths into one does not fail where the paths
/// agree — it fails where their CONTEXTS differ, and the context here is a
/// thread and a lifecycle stage rather than an argument.
///
/// `ingest_exit` is not on this trait because a child's exit is a SESSION
/// close, not a cell delivery. v2 says so twice, and the two say different
/// things: `session-emit.ts:316-322` routes the live `onExit` to
/// `closedByKeeper`, while `session-resume-events.ts:47-50` does NOT — the
/// adoption path replays `{kind: "exit"}` through `flushResumeEvents` and the
/// close happens on the other side, after the record is installed. Welding them
/// into one `ended` is what made this method look like a delivery's job.
///
/// `ingest_error` is gone for the same reason and a different cause. v2's live
/// `onError` (`session-emit.ts:324-328`) only LOGS `mux_channel_err` and stops;
/// it closes nothing and emits nothing to a browser. **A break is not an exit.**
/// The adoption path's error is already carried by [`Held::Error`], the
/// pre-record stager, which is disjoint from the capture below.
pub trait ChannelDelivery: Send + Sync {
    /// Parse and ship one chunk, or retain it without parsing while the core is
    /// frozen.
    fn ingest_output(&self, record: &mut SessionRecord, chunk: &[u8], now_ms: i64);
    /// Stop parsing this channel and hold what arrives; `now_ms` dates the
    /// emission hold, so a gate that overstays its budget is measured from
    /// the moment it was set.
    ///
    /// `false` when a capture is already open on the channel, which is a caller
    /// bug rather than a race: two unresolved boundaries would each have to be
    /// the one the core is resized at.
    fn freeze_capture(&self, channel_id: ChannelId, now_ms: i64) -> bool;
    /// Close the capture and hand back what it held, oldest first.
    ///
    /// The gate opens in the same call, so a chunk delivered afterwards is
    /// parsed after the captured bytes. The CALLER writes them, because whether
    /// they are parsed at the old or the new geometry is the boundary's answer
    /// and not the delivery's.
    fn close_capture(&self, channel_id: ChannelId) -> CapturedOutput;
    /// The emitter a stream transaction mints, baselines and traps through,
    /// under this delivery's lock so it never interleaves with a parse.
    /// `None` is a delivery that ships no cells; a stream transaction refuses
    /// against it rather than committing a generation nothing can paint.
    fn stream_emission(&self) -> Option<&dyn super::terminal_state::StreamEmission> {
        None
    }
}

/// The keeper's output for one channel, delivered into its record.
///
/// THE ONE BINDING IN THE WORKER. Every channel starts here — a freshly spawned
/// shell's first prompt arrives before its record has been installed, exactly as
/// a survivor's does — and goes live exactly once. No byte is ever delivered to
/// a record that is not in the table, and none is ever parsed out of order.
pub struct RecordBinding {
    channel_id: u16,
    sessions: Arc<SessionTable>,
    delivery: Arc<Mutex<dyn ChannelDelivery>>,
    clock: Arc<dyn EventClock>,
    mode: Mutex<Mode>,
}

impl RecordBinding {
    /// A binding for a channel whose record does not exist yet.
    pub fn staged(
        channel_id: u16,
        sessions: Arc<SessionTable>,
        delivery: Arc<Mutex<dyn ChannelDelivery>>,
        clock: Arc<dyn EventClock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            channel_id,
            sessions,
            delivery,
            clock,
            mode: Mutex::new(Mode::Staged(Staging::default())),
        })
    }

    /// A binding that delivers straight through, for a channel already held.
    pub fn live(
        channel_id: u16,
        sessions: Arc<SessionTable>,
        delivery: Arc<Mutex<dyn ChannelDelivery>>,
        clock: Arc<dyn EventClock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            channel_id,
            sessions,
            delivery,
            clock,
            mode: Mutex::new(Mode::Live),
        })
    }

    /// Replay what was held and deliver from here on.
    ///
    /// `false` when the staging overflowed, and nothing was replayed: those
    /// bytes are gone and no ordering puts them back. The overflow is re-read
    /// HERE and not only where the bytes arrived, so a slow consumer cannot
    /// reach the swap with a buffer that was already over the bound.
    ///
    /// A HELD EXIT OR BREAK IS REPORTED, NOT REPLAYED, and that is the whole of
    /// the v2 split. A survivor that had already exited by the time its record
    /// was installed is closed by the ADOPTION, after the record exists, because
    /// `close_channel` needs a record the table still holds — and calling it from
    /// here would route the same question through two callers whose legality is
    /// opposite: this runs on a tokio worker during an adoption, and the live
    /// `on_exit` runs on the keeper's plain dispatch thread, where blocking is
    /// legal and here it panics. Two callers, one question, no way for the callee
    /// to tell them apart. So the close is asked for once, on the side that can
    /// answer it.
    ///
    /// Returns `(the drain was clean, the exit that was held)`. BOTH, because
    /// collapsing them loses the overflow: an overflowed drain and a clean drain
    /// with nothing held both look like "no exit", and the caller has to be able
    /// to tell "this adoption is dead" from "this survivor had exited before its
    /// record existed" — opposite responses.
    pub fn go_live(&self) -> (bool, Option<i32>) {
        let mut mode = self.lock();
        let Mode::Staged(staging) = &mut *mode else {
            return (true, None);
        };
        if staging.overflowed {
            return (false, None);
        }
        let mut held_exit = None;
        for held in std::mem::take(&mut staging.events) {
            match held {
                Held::Output(chunk) => self.ingest(&chunk),
                Held::Exit(code) => held_exit = Some(code.unwrap_or(0)),
                // A break is a LOG, exactly as v2's live `onError` is, and for
                // the same reason: it says the channel could not be driven, and
                // the session decides what that means.
                Held::Error(reason) => {
                    tracing::warn!(
                        channel_id = self.channel_id,
                        reason = %reason,
                        "keeper: a channel could not be driven while it was staged"
                    );
                }
            }
        }
        *mode = Mode::Live;
        (true, held_exit)
    }

    /// Drop what was held, because the record it was for is never coming.
    pub fn abandon(&self) -> usize {
        let mut mode = self.lock();
        match &mut *mode {
            Mode::Staged(staging) => {
                let bytes = staging.bytes;
                staging.events.clear();
                bytes
            }
            Mode::Live => 0,
        }
    }

    /// How many bytes this binding is currently holding.
    pub fn staged_bytes(&self) -> usize {
        match &*self.lock() {
            Mode::Staged(staging) => staging.bytes,
            Mode::Live => 0,
        }
    }

    /// Whether this binding is still holding output for a record.
    pub fn is_staged(&self) -> bool {
        matches!(&*self.lock(), Mode::Staged(_))
    }

    /// The channel this binding delivers.
    pub fn channel_id(&self) -> u16 {
        self.channel_id
    }

    fn ingest(&self, chunk: &[u8]) {
        let now_ms = self.clock.now_epoch_ms();
        let Some(entry) = self.sessions.entry(self.channel_id) else {
            tracing::warn!(
                channel_id = self.channel_id,
                len = chunk.len(),
                "pty output arrived for a channel this worker holds no record for; it is dropped"
            );
            return;
        };
        let mut record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ingest_output(&mut record, chunk, now_ms);
    }

    /// THE LIVE EXIT ONLY. A child's end closes the SESSION — v2 routes it to
    /// `closedByKeeper` and the chain ends at a `SessionEvent::closed` — and
    /// that is the session layer's business, not a cell delivery's. This is the
    /// one path that may block to run it, because the keeper's dispatch thread is
    /// a plain `std::thread` rather than a runtime worker.
    ///
    /// THE LOCK ORDER, WHICH IS THE NOTE THE NEXT PERSON NEEDS:
    /// **`ended` holds nothing when it routes, and `close_channel` needs both of
    /// the locks it held.** It used to hold the record and the delivery, and
    /// `close_channel` takes the record (through `forget`) and ends on `cells` —
    /// the same `Arc<Mutex<dyn CellDelivery>>`. A `std::sync::Mutex` is not
    /// reentrant, so routing the close from inside either guard deadlocks on the
    /// first exit of every channel. If a future change moves the route up one
    /// line, it looks harmless and it is not.
    fn ended(&self, exit_code: Option<i32>) {
        tracing::info!(
            channel_id = self.channel_id,
            exit_code = ?exit_code,
            "keeper: a channel's child ended"
        );
    }

    /// A BREAK IS A LOG AND NOTHING ELSE, which is v2's live `onError` exactly
    /// (`session-emit.ts:324-328`): it records that the channel could not be
    /// driven and it stops. It does not close the session, does not touch the
    /// core, and emits nothing to a browser — `binding.rs`'s own contract for
    /// `on_error` says the session decides what it means and this binding only
    /// says it happened. **A break is not an exit.**
    fn broke(&self, reason: &str) {
        tracing::warn!(
            channel_id = self.channel_id,
            reason = %reason,
            "keeper: a channel could not be driven"
        );
    }

    fn lock(&self) -> MutexGuard<'_, Mode> {
        self.mode
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl ChannelBinding for RecordBinding {
    fn on_output(&self, chunk: &[u8]) {
        let mut mode = self.lock();
        if let Mode::Staged(staging) = &mut *mode {
            staging.stage_output(chunk);
            return;
        }
        drop(mode);
        self.ingest(chunk);
    }

    fn on_exit(&self, exit_code: Option<i32>) {
        let mut mode = self.lock();
        if let Mode::Staged(staging) = &mut *mode {
            staging.stage_exit(exit_code);
            return;
        }
        drop(mode);
        self.ended(exit_code);
    }

    fn on_error(&self, reason: String) {
        let mut mode = self.lock();
        if let Mode::Staged(staging) = &mut *mode {
            staging.stage_error(reason);
            return;
        }
        drop(mode);
        self.broke(&reason);
    }
}

impl std::fmt::Debug for RecordBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordBinding")
            .field("channel_id", &self.channel_id)
            .field("staged_bytes", &self.staged_bytes())
            .finish_non_exhaustive()
    }
}
