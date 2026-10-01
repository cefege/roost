//! What one recording has heard and what it is still waiting for.
//!
//! Split out of `super::deepgram_engine` because this is state a reading can ask
//! about — the settled words, whether the device attached, whether anything was
//! heard at all — and it is the same state the engine mutates. Keeping it here
//! names the two halves: the engine acts, this remembers.
//! Ports the closure state of `apps/web/src/voice/deepgramDictation.ts`.

use web_sys::WebSocket;

use super::handshake::CloseIntent;

/// The mutable half of one recording: the socket, what has been said, and the
/// one retry each of the grant failure and the close is allowed.
#[derive(Default)]
pub(super) struct Session {
    pub(super) socket: Option<WebSocket>,
    /// Audio that arrived before the socket opened, flushed in order on open.
    pub(super) prebuffer: Vec<Vec<u8>>,
    pub(super) segments: Vec<String>,
    pub(super) interim: String,
    pub(super) end_intent: Option<CloseIntent>,
    pub(super) mic_attached: bool,
    pub(super) socket_open: bool,
    pub(super) announced_live: bool,
    /// One reconnect per recording, shared by the grant failure and the close.
    pub(super) retried: bool,
    /// One pipeline rebuild per recording.
    pub(super) repaired: bool,
    /// This recording has already reported its ending, so a timer or a socket
    /// callback that arrives afterwards has nothing left to decide.
    pub(super) failed: bool,
    pub(super) results: usize,
}

impl Session {
    /// The finalized words, joined the way the engine joined them.
    pub(super) fn settled(&self) -> String {
        self.segments.join(" ")
    }
}
