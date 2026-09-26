//! The canonical screen the coordinator holds for one watched session.
//!
//! Ported from `apps/coord/src/terminal/screen/terminal-screen-hub.ts` and
//! its `-state`, `-residency` and `-frames` siblings, minus the socket fan-out
//! and the snapshot controller, which belong to whichever slice owns the Sync
//! sink. What is here is the part every one of those callers depends on: the
//! expected stream, the resident cache, the chunk assembly gate, and the one
//! rule that matters most -- a full that does not fit degrades loudly and
//! never half-served.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::cell::frame_chunks::encoded_cell_grid_frame_size;
use roost_protocol::cell::{CellGridChunkAssembler, CellGridChunkAssembly};
use roost_protocol::wire::SessionId;

use crate::terminal_screen::byte_hub::PublishOutcome;
use crate::terminal_screen::residency::{
    SessionCharge, TERMINAL_SCREEN_MAX_RESIDENT_ROWS, TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
    TerminalScreenResidency,
};
use crate::terminal_screen::screen_budget::TerminalScreenCaps;

/// Mirrors the Sync v2 per-domain queue bounds: enough to ride out one
/// baseline transfer.
pub const TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES: usize = 512;
pub const TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The boundary callbacks a replica needs from the side that owns the sockets.
///
/// Separated so a replica is testable without one, and so the owner of the Sync
/// sink is not a dependency of the screen.
pub trait ScreenReplicaSink: Send + Sync {
    /// The session's screen cannot be served, and the named reason is why. The
    /// one call a budget refusal makes before dropping the cache.
    fn unavailable(&self, session_id: &SessionId, reason: &str);
    /// A full baseline was admitted and is now the session's current replica.
    fn full_accepted(&self, session_id: &SessionId, stream_id: &str);
}

/// A sink that reports nothing, for a coordinator with no socket fan-out.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoScreenReplicaSink;

impl ScreenReplicaSink for NoScreenReplicaSink {
    fn unavailable(&self, session_id: &SessionId, reason: &str) {
        tracing::warn!(session_id = %session_id, reason, "a terminal screen became unavailable");
    }

    fn full_accepted(&self, _session_id: &SessionId, _stream_id: &str) {}
}

/// The stream a replica currently expects, whether or not it holds a baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedStream {
    pub stream_id: String,
    pub cols: u32,
    pub rows: u32,
}

/// Bounded parking for ordinary deltas that arrive while a chunked baseline
/// assembles. Reaching either cap means the transfer lost the race anyway, so
/// a push that does not fit is refused and the caller falls back to the
/// single-resync latch rather than growing.
#[derive(Debug, Default)]
pub struct TerminalAssemblyHold {
    frames: Vec<PbCellGridFrame>,
    bytes: u64,
}

impl TerminalAssemblyHold {
    /// Park one delta, or refuse it when the hold is full.
    pub fn push(&mut self, frame: &PbCellGridFrame) -> bool {
        let bytes = u64::from(encoded_cell_grid_frame_size(frame));
        if self.frames.len() + 1 > TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES
            || self.bytes + bytes > TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_BYTES
        {
            return false;
        }
        self.frames.push(frame.clone());
        self.bytes += bytes;
        true
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    /// Empties the hold. A held delta whose `base_seq` no longer matches the
    /// live replica is skipped, never folded: the installed full already
    /// contains everything emitted before it.
    pub fn drain(&mut self, live_seq: Option<u64>, live: bool) -> Vec<PbCellGridFrame> {
        let frames = std::mem::take(&mut self.frames);
        self.bytes = 0;
        if !live {
            return Vec::new();
        }
        let Some(seq) = live_seq else {
            return Vec::new();
        };
        frames
            .into_iter()
            .filter(|frame| frame.base_seq == seq)
            .collect()
    }
}

/// One session's screen state.
#[derive(Debug, Default)]
pub struct SessionScreen {
    pub expected: Option<ExpectedStream>,
    pub charge: SessionCharge,
    pub chunks: CellGridChunkAssembler,
    /// True once a delta or baseline failed and no baseline has repaired it.
    /// Nothing is served to a watcher while this holds.
    pub resync_latched: bool,
    pub hold: TerminalAssemblyHold,
}

/// The replicas, the residency pool they charge against, and the sink.
pub struct ScreenHub {
    pub(crate) sessions: Mutex<HashMap<SessionId, SessionScreen>>,
    pub(crate) residency: Mutex<TerminalScreenResidency>,
    pub(crate) sink: Arc<dyn ScreenReplicaSink>,
}

impl ScreenHub {
    /// A hub with the hard maxima, for a coordinator that declared no budget.
    #[must_use]
    pub fn new() -> Self {
        Self::with_caps(TerminalScreenCaps {
            max_resident_rows: TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
            max_resident_spans: TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
        })
    }

    /// A hub over a budget's two ceilings.
    #[must_use]
    pub fn with_caps(caps: TerminalScreenCaps) -> Self {
        Self::with_sink(caps, Arc::new(NoScreenReplicaSink))
    }

    /// A hub over a budget's ceilings, told when a screen becomes unavailable.
    #[must_use]
    pub fn with_sink(caps: TerminalScreenCaps, sink: Arc<dyn ScreenReplicaSink>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            residency: Mutex::new(TerminalScreenResidency::new(
                caps.max_resident_rows,
                caps.max_resident_spans,
            )),
            sink,
        }
    }

    /// The stream a session's replica expects, or `None` if it is not watching.
    #[must_use]
    pub fn expected_stream_id(&self, session_id: &SessionId) -> Option<String> {
        let sessions = self.sessions.lock().ok()?;
        sessions
            .get(session_id)
            .and_then(|screen| screen.expected.as_ref())
            .map(|expected| expected.stream_id.clone())
    }

    /// The current replica's validity, for a caller deciding whether to seed.
    #[must_use]
    pub fn has_valid_cache(&self, session_id: &SessionId) -> bool {
        let Ok(sessions) = self.sessions.lock() else {
            return false;
        };
        sessions.get(session_id).is_some_and(|screen| {
            screen.charge.current().is_some_and(|cache| cache.valid) && !screen.resync_latched
        })
    }

    /// The current replica's sequence, for the hold's replay guard.
    #[must_use]
    pub fn current_seq(&self, session_id: &SessionId) -> Option<u64> {
        let sessions = self.sessions.lock().ok()?;
        sessions
            .get(session_id)
            .and_then(|screen| screen.charge.current().map(|cache| cache.frame.seq))
    }

    /// Declare the stream this session's replica will serve. A change of any
    /// of the three fields invalidates the baseline: the old grid is not a
    /// prefix of the new one at a different geometry.
    pub fn expect_stream(&self, session_id: &SessionId, stream_id: &str, cols: u32, rows: u32) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        let screen = sessions.entry(session_id.clone()).or_default();
        if let Some(expected) = &screen.expected
            && expected.stream_id == stream_id
            && expected.cols == cols
            && expected.rows == rows
        {
            return;
        }
        screen.chunks.reset();
        screen.hold.clear();
        let _ = self
            .residency
            .lock()
            .map(|mut pool| pool.drop_cache(&mut screen.charge));
        screen.expected = Some(ExpectedStream {
            stream_id: stream_id.to_owned(),
            cols,
            rows,
        });
        screen.resync_latched = false;
    }

    /// A baseline or delta failed against this session's replica. Nothing is
    /// served from it until a fresh full arrives.
    pub fn invalidate(&self, session_id: &SessionId, reason: &str) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        let Some(screen) = sessions.get_mut(session_id) else {
            return;
        };
        if screen.expected.is_none() {
            return;
        }
        screen.chunks.reset();
        screen.hold.clear();
        if let Some(cache) = screen.charge.current.as_mut() {
            cache.valid = false;
        }
        screen.resync_latched = true;
        tracing::warn!(session_id = %session_id, reason, "a terminal screen failed closed");
    }

    /// Forget a session entirely, returning its bytes to the pool.
    pub fn drop_session(&self, session_id: &SessionId) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        if let Some(mut screen) = sessions.remove(session_id) {
            let _ = self
                .residency
                .lock()
                .map(|mut pool| pool.drop_cache(&mut screen.charge));
        }
    }

    /// Feed one whole cell frame into a session's replica.
    pub fn publish_frame(
        &self,
        session_id: &SessionId,
        frame: &mut PbCellGridFrame,
        _now_ms: i64,
    ) -> PublishOutcome {
        let Ok(mut sessions) = self.sessions.lock() else {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        };
        let Some(screen) = sessions.get_mut(session_id) else {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        };
        if screen.expected.is_none() {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        }
        let assembling = screen.chunks.active_snapshot_id().is_some();
        if assembling {
            // Deltas park in the bounded hold during assembly; a matching full
            // supersedes; anything else resyncs.
            let interrupts = !frame.full;
            if !frame.full {
                if screen.hold.push(frame) {
                    return PublishOutcome::Published {
                        session_id: session_id.clone(),
                    };
                }
                screen.resync_latched = true;
                return PublishOutcome::Published {
                    session_id: session_id.clone(),
                };
            }
            screen.hold.clear();
            screen.chunks.reset();
            if interrupts {
                screen.resync_latched = true;
            }
        }
        self.accept_full(session_id, screen, frame, false)
    }

    /// Feed one part of a chunked baseline into a session's replica.
    pub fn publish_chunk(
        &self,
        session_id: &SessionId,
        chunk: &PbCellGridChunk,
        now_ms: i64,
    ) -> PublishOutcome {
        let Ok(mut sessions) = self.sessions.lock() else {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        };
        let Some(screen) = sessions.get_mut(session_id) else {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        };
        let Some(expected) = screen.expected.clone() else {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        };
        let Some(part) = chunk.part.as_option() else {
            return PublishOutcome::Published {
                session_id: session_id.clone(),
            };
        };
        if part.stream_id != expected.stream_id {
            return PublishOutcome::NoExpectedStream {
                session_id: session_id.clone(),
            };
        }
        let assembly = screen.chunks.push(chunk, now_ms.max(0) as u64);
        match assembly {
            Err(error) => {
                screen.hold.clear();
                screen.resync_latched = true;
                tracing::warn!(session_id = %session_id, reason = %error.reason, "a terminal baseline chunk was refused");
            }
            Ok(CellGridChunkAssembly::Pending { .. }) => {}
            Ok(CellGridChunkAssembly::Complete { frame, .. }) => {
                let mut frame = frame;
                screen.chunks.reset();
                self.accept_full(session_id, screen, &mut frame, true);
            }
        }
        PublishOutcome::Published {
            session_id: session_id.clone(),
        }
    }
}

impl std::fmt::Debug for ScreenHub {
    /// The sink is a trait object; a log line needs how many sessions are held
    /// and what the pool currently costs, which is the question a capacity
    /// incident is asked.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sessions = self
            .sessions
            .lock()
            .map(|held| held.len())
            .unwrap_or_default();
        let (rows, spans) = self
            .residency
            .lock()
            .map(|pool| pool.usage())
            .unwrap_or((0, 0));
        formatter
            .debug_struct("ScreenHub")
            .field("sessions", &sessions)
            .field("resident_rows", &rows)
            .field("resident_spans", &spans)
            .finish()
    }
}

impl Default for ScreenHub {
    fn default() -> Self {
        Self::new()
    }
}
