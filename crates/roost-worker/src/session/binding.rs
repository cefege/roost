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

/// The per-channel cell delivery registration the lifecycle owes the emitter;
/// `session::emit::CellEmitter` is the production implementation. Both facts a
/// channel's delivery has — that it started, and that it is over — are
/// transitions the session layer owns, so both are announced through here.
pub trait CellDelivery: Send + Sync {
    /// A channel's stream generation, so deltas can flow to it.
    fn install_stream(&mut self, channel_id: ChannelId, stream_id: &str);
    /// A channel is gone; its delivery state and parked cursors go with it.
    fn forget_channel(&mut self, channel_id: ChannelId);
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
/// production implementation of both.
pub trait ChannelDelivery: Send + Sync {
    /// Parse and ship one chunk, or retain it without parsing while the core is
    /// frozen.
    fn ingest_output(&self, record: &mut SessionRecord, chunk: &[u8], now_ms: i64);
    /// The child ended.
    fn ingest_exit(&self, record: &mut SessionRecord, exit_code: Option<i32>, now_ms: i64);
    /// The channel could not be driven.
    fn ingest_error(&self, record: &mut SessionRecord, reason: &str, now_ms: i64);
    /// Stop parsing this channel and hold what arrives.
    ///
    /// `false` when a capture is already open on the channel, which is a caller
    /// bug rather than a race: two unresolved boundaries would each have to be
    /// the one the core is resized at.
    fn freeze_capture(&self, channel_id: ChannelId) -> bool;
    /// Close the capture and hand back what it held, oldest first.
    ///
    /// The gate opens in the same call, so a chunk delivered afterwards is
    /// parsed after the captured bytes. The CALLER writes them, because whether
    /// they are parsed at the old or the new geometry is the boundary's answer
    /// and not the delivery's.
    fn close_capture(&self, channel_id: ChannelId) -> CapturedOutput;
}

/// One thing a delivery is holding because the record is not there yet.
#[derive(Debug)]
enum Held {
    Output(Vec<u8>),
    Exit(Option<i32>),
    Error(String),
}

/// Output that arrived before the record it belongs to.
#[derive(Debug, Default)]
struct Staging {
    events: Vec<Held>,
    bytes: usize,
    overflowed: bool,
}

impl Staging {
    /// Hold one chunk, or give up on the whole stream.
    ///
    /// Post-bound chunks are dropped WITHOUT buffering rather than trimmed to
    /// fit: the stream's integrity is already lost and the caller refuses the
    /// adoption the moment it looks, so keeping a partial tail would only make
    /// the hole look smaller than it is.
    fn stage_output(&mut self, chunk: &[u8]) {
        if self.overflowed {
            return;
        }
        self.bytes += chunk.len();
        if self.bytes > RESUME_STAGE_CAP_BYTES {
            self.overflowed = true;
            self.events.clear();
            tracing::warn!(
                staged_bytes = self.bytes,
                cap_bytes = RESUME_STAGE_CAP_BYTES,
                "pty output outgrew the staging bound; this stream can no longer be adopted whole"
            );
            return;
        }
        self.events.push(Held::Output(chunk.to_vec()));
    }

    fn stage_exit(&mut self, exit_code: Option<i32>) {
        if !self.overflowed {
            self.events.push(Held::Exit(exit_code));
        }
    }

    fn stage_error(&mut self, reason: String) {
        if !self.overflowed {
            self.events.push(Held::Error(reason));
        }
    }
}

/// Whether a channel's delivery is still holding output for a record.
#[derive(Debug)]
enum Mode {
    Staged(Staging),
    Live,
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
    pub fn go_live(&self) -> bool {
        let mut mode = self.lock();
        let Mode::Staged(staging) = &mut *mode else {
            return true;
        };
        if staging.overflowed {
            return false;
        }
        for held in std::mem::take(&mut staging.events) {
            match held {
                Held::Output(chunk) => self.ingest(&chunk),
                Held::Exit(code) => self.ended(code),
                Held::Error(reason) => self.broke(&reason),
            }
        }
        *mode = Mode::Live;
        true
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

    fn ended(&self, exit_code: Option<i32>) {
        let now_ms = self.clock.now_epoch_ms();
        let Some(entry) = self.sessions.entry(self.channel_id) else {
            return;
        };
        let mut record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ingest_exit(&mut record, exit_code, now_ms);
    }

    fn broke(&self, reason: &str) {
        let now_ms = self.clock.now_epoch_ms();
        let Some(entry) = self.sessions.entry(self.channel_id) else {
            return;
        };
        let mut record = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.delivery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ingest_error(&mut record, reason, now_ms);
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
