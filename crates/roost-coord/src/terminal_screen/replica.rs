//! The canonical screen the coordinator holds for every watched session, and
//! the entry points a worker's cells and a view decision drive it through.
//!
//! Ported from `apps/coord/src/terminal/screen/terminal-screen-hub.ts`. Its
//! state lives in `hub_state`, admission in `replica_admission`, socket fan-out
//! in `hub_fanout`, the repair ladder in `snapshot_controller`. The one rule
//! that matters most: a full that does not fit degrades loudly and is never
//! half-served.
//!
//! LOCK ORDER: `sessions`, then `sockets`, then `residency`. No socket sink is
//! ever called with any of them held -- a sink takes its Sync socket's lock,
//! and that socket may be asking this hub for a rebaseline while holding it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::cell::CellGridChunkAssembly;
use roost_protocol::wire::SessionId;

use crate::terminal_screen::byte_hub::PublishOutcome;
use crate::terminal_screen::hub_contract::{
    NoScreenReplicaSink, ScreenReplicaSink, ScreenTimers, TokioScreenTimers,
};
use crate::terminal_screen::hub_fanout::ScreenEffect;
use crate::terminal_screen::hub_state::{
    ExpectedStream, SessionScreen, SocketIndex, stamp_terminal_snapshot_receipt,
};
use crate::terminal_screen::residency::{
    TERMINAL_SCREEN_MAX_RESIDENT_ROWS, TERMINAL_SCREEN_MAX_RESIDENT_SPANS, TerminalScreenResidency,
};
use crate::terminal_screen::screen_budget::TerminalScreenCaps;

/// The coordinator's wall clock, in the unsigned milliseconds a chunk counts.
pub type ScreenClock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The replicas, the sockets watching them, the residency pool, and the owner
/// the repair ladder reports to.
pub struct ScreenHub {
    pub(crate) sessions: Mutex<HashMap<SessionId, SessionScreen>>,
    pub(crate) sockets: Mutex<SocketIndex>,
    pub(crate) residency: Mutex<TerminalScreenResidency>,
    pub(crate) sink: Arc<dyn ScreenReplicaSink>,
    pub(crate) timers: Arc<dyn ScreenTimers>,
    pub(crate) clock: ScreenClock,
    tokens: AtomicU64,
}

impl ScreenHub {
    /// A hub with the hard maxima and no owner, for a test.
    #[must_use]
    pub fn new() -> Self {
        Self::with_caps(TerminalScreenCaps {
            max_resident_rows: TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
            max_resident_spans: TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
        })
    }

    /// A hub over a budget's two ceilings, with no owner.
    #[must_use]
    pub fn with_caps(caps: TerminalScreenCaps) -> Self {
        Self::with_sink(caps, Arc::new(NoScreenReplicaSink))
    }

    /// A hub over a budget's ceilings, reporting to `sink`, on the runtime's
    /// timers and the wall clock.
    #[must_use]
    pub fn with_sink(caps: TerminalScreenCaps, sink: Arc<dyn ScreenReplicaSink>) -> Self {
        let clock: ScreenClock = Arc::new(|| u64::try_from(crate::serve::now_ms()).unwrap_or(0));
        Self::with_deadlines(caps, sink, Arc::new(TokioScreenTimers), clock)
    }

    /// A hub whose deadlines and clock the caller drives.
    #[must_use]
    pub fn with_deadlines(
        caps: TerminalScreenCaps,
        sink: Arc<dyn ScreenReplicaSink>,
        timers: Arc<dyn ScreenTimers>,
        clock: ScreenClock,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            sockets: Mutex::new(SocketIndex::default()),
            residency: Mutex::new(TerminalScreenResidency::new(
                caps.max_resident_rows,
                caps.max_resident_spans,
            )),
            sink,
            timers,
            clock,
            tokens: AtomicU64::new(1),
        }
    }

    /// The stream a session's replica expects, whether or not it holds a
    /// baseline for it.
    #[must_use]
    pub fn expected_stream_id(&self, session_id: &SessionId) -> Option<String> {
        self.locked_sessions()
            .get(session_id)
            .and_then(|screen| screen.expected.as_ref())
            .map(|expected| expected.stream_id.clone())
    }

    /// Whether the replica could serve a watcher right now.
    #[must_use]
    pub fn has_valid_cache(&self, session_id: &SessionId) -> bool {
        self.locked_sessions()
            .get(session_id)
            .is_some_and(|screen| {
                screen.charge.current().is_some_and(|cache| cache.valid) && !screen.resync_latched
            })
    }

    /// The current replica's sequence.
    #[must_use]
    pub fn current_seq(&self, session_id: &SessionId) -> Option<u64> {
        self.locked_sessions()
            .get(session_id)
            .and_then(|screen| screen.charge.current().map(|cache| cache.frame.seq))
    }

    /// Declare the stream this session's replica will serve. A change of any of
    /// the three fields drops the baseline -- the old grid is not a prefix of
    /// the new one -- restarts every watcher's lane, and arms the first-byte
    /// deadline for the new stream.
    pub fn expect_stream(
        self: &Arc<Self>,
        session_id: &SessionId,
        stream_id: &str,
        cols: u32,
        rows: u32,
    ) {
        let mut effects = Vec::new();
        {
            let mut sessions = self.locked_sessions();
            let screen = sessions.entry(session_id.clone()).or_default();
            let expected = ExpectedStream {
                stream_id: stream_id.to_owned(),
                cols,
                rows,
            };
            if screen.expected.as_ref() == Some(&expected) {
                return;
            }
            self.reset_repair(screen, true);
            for watcher in self.locked_sockets().watchers_of(session_id) {
                effects.push(ScreenEffect::Begin {
                    watcher,
                    session_id: session_id.clone(),
                    stream_id: stream_id.to_owned(),
                });
            }
            self.drop_cache(screen);
            screen.hold.clear();
            screen.expected = Some(expected);
            screen.resync_latched = false;
            self.arm_baseline_timer(session_id, screen);
        }
        tracing::info!(session_id = %session_id, stream_id, cols, rows, "terminal screen expects a stream");
        self.run_effects(effects);
    }

    /// A baseline or delta failed against this session's replica: nothing is
    /// served from it, and a source full is requested.
    pub fn invalidate(self: &Arc<Self>, session_id: &SessionId, reason: &str) {
        let mut effects = Vec::new();
        {
            let mut sessions = self.locked_sessions();
            let Some(screen) = sessions.get_mut(session_id) else {
                return;
            };
            if screen.expected.is_none() {
                return;
            }
            self.reset_chunks(screen);
            screen.hold.clear();
            if let Some(cache) = screen.charge.current.as_mut() {
                cache.valid = false;
            }
            self.retry(session_id, screen, reason, &mut effects);
        }
        self.run_effects(effects);
    }

    /// Forget a session entirely: its watchers lose their lanes and its bytes
    /// go back to the pool.
    pub fn drop_session(self: &Arc<Self>, session_id: &SessionId) {
        let detached = {
            let mut sessions = self.locked_sessions();
            let detached = self.locked_sockets().detach_session(session_id);
            if let Some(mut screen) = sessions.remove(session_id) {
                self.reset_repair(&mut screen, true);
                self.drop_cache(&mut screen);
            }
            detached
        };
        tracing::info!(session_id = %session_id, watchers = detached.len(), "terminal screen dropped");
        for sink in detached {
            sink.drop_terminal_session(session_id);
        }
        // A drop callback may have re-watched the session it was told is gone;
        // that watch belongs to nothing and is cleared, as v2 `dropSession` does.
        self.locked_sockets().detach_session(session_id);
    }

    /// Feed one whole cell frame into a session's replica.
    pub fn publish_frame(
        self: &Arc<Self>,
        session_id: &SessionId,
        frame: &mut PbCellGridFrame,
        _now_ms: i64,
    ) -> PublishOutcome {
        let mut effects = Vec::new();
        {
            let mut sessions = self.locked_sessions();
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
            session_id.as_str().clone_into(&mut frame.session_id);
            if screen.assembling() {
                // Deltas park in the bounded hold during assembly; a matching
                // full supersedes; anything else asks for a repair.
                let interrupts = !frame.full || frame.stream_id != expected.stream_id;
                if !frame.full && screen.hold.push(frame) {
                    return PublishOutcome::Published {
                        session_id: session_id.clone(),
                    };
                }
                screen.hold.clear();
                self.reset_chunks(screen);
                if interrupts {
                    let reason = "ordinary frame interrupted chunk assembly";
                    self.retry(session_id, screen, reason, &mut effects);
                }
                if frame.full {
                    self.accept_full(session_id, screen, frame, false, &mut effects);
                }
            } else if frame.full {
                self.accept_full(session_id, screen, frame, false, &mut effects);
            } else {
                self.accept_delta(session_id, screen, frame, &mut effects);
            }
        }
        self.run_effects(effects);
        PublishOutcome::Published {
            session_id: session_id.clone(),
        }
    }

    /// Feed one part of a chunked baseline into a session's replica.
    pub fn publish_chunk(
        self: &Arc<Self>,
        session_id: &SessionId,
        chunk: &mut PbCellGridChunk,
        received_at_ms: i64,
    ) -> PublishOutcome {
        let mut effects = Vec::new();
        let mut completed = false;
        {
            let mut sessions = self.locked_sessions();
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
            let Some(part) = chunk.part.as_option_mut() else {
                return PublishOutcome::NoExpectedStream {
                    session_id: session_id.clone(),
                };
            };
            if part.stream_id != expected.stream_id {
                return PublishOutcome::NoExpectedStream {
                    session_id: session_id.clone(),
                };
            }
            session_id.as_str().clone_into(&mut part.session_id);
            let received_at_ms = u64::try_from(received_at_ms).unwrap_or(0);
            stamp_terminal_snapshot_receipt(&mut screen.chunks, chunk, received_at_ms);
            let first_chunk = !screen.assembling();
            match screen.chunks.assembler.push(chunk, (self.clock)()) {
                Err(error) => {
                    screen.hold.clear();
                    self.reset_chunks(screen);
                    self.retry(session_id, screen, &error.reason, &mut effects);
                }
                Ok(assembly) => {
                    if first_chunk {
                        self.cancel_request_timer(screen, true);
                    }
                    self.arm_chunk_timer(session_id, screen);
                    if let CellGridChunkAssembly::Complete { mut frame, .. } = assembly {
                        self.reset_chunks(screen);
                        self.accept_full(session_id, screen, &mut frame, true, &mut effects);
                        completed = true;
                    }
                }
            }
        }
        self.run_effects(effects);
        // Held deltas fold only once the baseline's seeds went out: a seed
        // leases the version it serves, and folding first would supersede that
        // version before any socket could lease it.
        if completed {
            self.replay_held_deltas(session_id);
        }
        PublishOutcome::Published {
            session_id: session_id.clone(),
        }
    }

    /// Take a residency lease on one cache version for a snapshot cursor.
    pub(crate) fn acquire_source_lease(&self, session_id: &SessionId, generation: u64) -> bool {
        let mut sessions = self.locked_sessions();
        let Some(screen) = sessions.get_mut(session_id) else {
            return false;
        };
        self.locked_residency()
            .acquire_source_lease(&mut screen.charge, generation)
    }

    /// Give a cursor's lease back; the last one out un-pins its version.
    pub(crate) fn release_source_lease(&self, session_id: &SessionId, generation: u64) {
        let mut sessions = self.locked_sessions();
        if let Some(screen) = sessions.get_mut(session_id) {
            self.locked_residency()
                .release_source_lease(&mut screen.charge, generation);
        }
    }

    pub(crate) fn drop_cache(&self, screen: &mut SessionScreen) {
        self.locked_residency().drop_cache(&mut screen.charge);
        screen.source = None;
    }

    pub(crate) fn next_token(&self) -> u64 {
        self.tokens.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn locked_sessions(&self) -> MutexGuard<'_, HashMap<SessionId, SessionScreen>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn locked_sockets(&self) -> MutexGuard<'_, SocketIndex> {
        self.sockets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn locked_residency(&self) -> MutexGuard<'_, TerminalScreenResidency> {
        self.residency
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for ScreenHub {
    /// The sink and the sockets are trait objects; a log line needs how many
    /// sessions and sockets are held and what the pool currently costs.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sessions = self.locked_sessions().len();
        let sockets = self.locked_sockets().sockets.len();
        let (rows, spans) = self.locked_residency().usage();
        formatter
            .debug_struct("ScreenHub")
            .field("sessions", &sessions)
            .field("sockets", &sockets)
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
