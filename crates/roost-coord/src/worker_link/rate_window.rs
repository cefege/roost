//! The per-socket budget on durable events, and the close a breach causes.
//!
//! Owned by the coordinator's worker link and consulted before every durable
//! event frame is queued
//! (`apps/coord/src/workers/worker-ws-handler.ts:255-273`). Pure arithmetic on
//! an injected `now_ms`, so the whole window is testable without a socket.
//!
//! WHY A RATE AND NOT A QUEUE BOUND. The socket already has a bounded ordered
//! queue, and this is a *different* limit on a *different* resource: the queue
//! bounds frames waiting for a write, while this bounds how fast one
//! authenticated worker can ask the coordinator to commit to the fact that a PTY
//! exists. A worker mid-replay legitimately bursts -- it is draining its outbox
//! -- so the window is wide; what it must not do is sustain that rate forever.
//!
//! WHY THE BREACH CLOSES ONLY THAT SOCKET. The comment is explicit
//! (`worker-ws-handler.ts:45-46`): "Fixed per-socket window: the first 600
//! durable event frames are admitted; the next closes only their authenticated
//! worker socket." Closing rather than shedding frames is deliberate: shedding
//! an event frame would leave a hole in the durable log with no way for the
//! worker to know, whereas a close makes the worker reconnect and replay
//! everything it never had acknowledged, which is the same self-healing path a
//! network blip takes.
//!
//! WHY 600 PER MINUTE. Roughly ten per second sustained. A worker reconciling
//! hundreds of sessions after a keeper restart bursts well past that, and the
//! window is fixed rather than a token bucket precisely so the burst is allowed
//! and the sustained rate is not.

/// Durable event frames admitted per window, per socket.
pub const DURABLE_EVENT_LIMIT: u64 = 600;

/// The window, in milliseconds.
pub const DURABLE_EVENT_WINDOW_MS: u64 = 60_000;

/// Why a worker socket is closed for exceeding the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateBreach {
    /// The frames admitted in the window that was refused.
    pub limit: u64,
    /// The window's length in milliseconds.
    pub window_ms: u64,
}

/// The fixed window for one authenticated worker socket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DurableEventWindow {
    started_at_ms: Option<u64>,
    admitted: u64,
}

impl DurableEventWindow {
    /// A window that has admitted nothing and has not started.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit one durable event frame, or report the breach.
    ///
    /// The window rolls on `now >= started_at + window_ms` and also on a
    /// **backwards** step, because `now < started_at` means the clock moved and
    /// a window that refuses to roll would stay permanently exhausted. v2
    /// handles this the same way (`worker-ws-handler.ts:51-58`), and the
    /// asymmetry matters: rolling on a backwards step costs one window of
    /// budget, while not rolling costs the socket.
    pub fn admit(&mut self, now_ms: u64) -> Result<(), RateBreach> {
        let expired = match self.started_at_ms {
            None => true,
            Some(started) => {
                now_ms < started || now_ms.saturating_sub(started) >= DURABLE_EVENT_WINDOW_MS
            }
        };
        if expired {
            self.started_at_ms = Some(now_ms);
            self.admitted = 0;
        }
        if self.admitted >= DURABLE_EVENT_LIMIT {
            return Err(RateBreach {
                limit: DURABLE_EVENT_LIMIT,
                window_ms: DURABLE_EVENT_WINDOW_MS,
            });
        }
        self.admitted += 1;
        Ok(())
    }

    /// How many frames this window has admitted.
    #[must_use]
    pub fn admitted(&self) -> u64 {
        self.admitted
    }
}
