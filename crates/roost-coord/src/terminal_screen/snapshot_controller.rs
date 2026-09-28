//! The repair ladder: baseline deadlines, chunk stall deadlines, the two
//! snapshot requests and the fresh-stream escalation, and the one planned
//! source every socket is seeded from.
//!
//! Ports `apps/coord/src/terminal/screen/terminal-screen-snapshot-controller.ts`.
//! Every deadline carries the token and repair generation it was armed with,
//! so a stale callback can neither replace a newer stream nor reopen a
//! completed repair.

use std::sync::Arc;

use roost_protocol::cell::frame_chunk_validation::CELL_GRID_CHUNK_STALL_MS;
use roost_protocol::wire::SessionId;

use crate::terminal_screen::hub_fanout::ScreenEffect;
use crate::terminal_screen::hub_state::SessionScreen;
use crate::terminal_screen::replica::ScreenHub;
use crate::terminal_screen::snapshot_source::ResidentSnapshotSource;

/// How long a stream, a snapshot request or a chunk transfer may go without
/// its first byte before the next rung of the ladder.
pub const TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS: u64 = CELL_GRID_CHUNK_STALL_MS;

/// Snapshot requests one repair may send before it asks for a fresh stream.
const SNAPSHOT_REQUEST_ATTEMPTS: u32 = 2;

impl ScreenHub {
    /// Latch one repair; a replica already latched asks for nothing more.
    pub(crate) fn latch(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        reason: &str,
        effects: &mut Vec<ScreenEffect>,
    ) {
        self.request_resync(session_id, screen, reason, false, effects);
    }

    /// Ask for a repair even when latched, unless a chunked baseline is
    /// already on its way.
    pub(crate) fn retry(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        reason: &str,
        effects: &mut Vec<ScreenEffect>,
    ) {
        if screen.assembling() {
            return;
        }
        self.request_resync(session_id, screen, reason, true, effects);
    }

    /// The planned source for the session's current cache, planned once per
    /// cache version. A cache that cannot be planned is reported unavailable.
    pub(crate) fn seed_source(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        effects: &mut Vec<ScreenEffect>,
    ) -> Option<Arc<ResidentSnapshotSource>> {
        let cache = screen.charge.current()?;
        if let Some(source) = &screen.source
            && source.generation() == cache.generation
        {
            return Some(Arc::clone(source));
        }
        match ResidentSnapshotSource::plan(self, session_id, cache) {
            Ok(source) => {
                let source = Arc::new(source);
                screen.source = Some(Arc::clone(&source));
                Some(source)
            }
            Err(reason) => {
                tracing::error!(session_id = %session_id, reason = %reason, "terminal.snapshot_encode_failed");
                effects.push(ScreenEffect::Unavailable {
                    session_id: session_id.clone(),
                    reason,
                });
                None
            }
        }
    }

    /// Arm the first-byte deadline of a newly expected stream, handing a
    /// stream that never installs a baseline to the same ladder a lost one
    /// uses.
    pub(crate) fn arm_baseline_timer(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
    ) {
        self.cancel_baseline_timer(screen);
        let Some(expected) = &screen.expected else {
            return;
        };
        let token = self.next_token();
        let generation = screen.repair.generation;
        let stream_id = expected.stream_id.clone();
        screen.repair.baseline_timer = Some(token);
        let hub = Arc::downgrade(self);
        let session_id = session_id.clone();
        let delay = TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
        self.timers.schedule(
            delay,
            Box::new(move || {
                if let Some(hub) = hub.upgrade() {
                    hub.on_baseline_deadline(&session_id, token, generation, &stream_id);
                }
            }),
        );
    }

    /// Arm the stall deadline of a chunked baseline; a chunk IS the baseline
    /// arriving, so it replaces the first-byte deadline.
    pub(crate) fn arm_chunk_timer(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
    ) {
        self.cancel_baseline_timer(screen);
        let token = self.next_token();
        let generation = screen.repair.generation;
        screen.chunks.timer = Some(token);
        screen.chunks.timer_generation = Some(generation);
        let hub = Arc::downgrade(self);
        let session_id = session_id.clone();
        self.timers.schedule(
            CELL_GRID_CHUNK_STALL_MS,
            Box::new(move || {
                if let Some(hub) = hub.upgrade() {
                    hub.on_chunk_deadline(&session_id, token, generation);
                }
            }),
        );
    }

    pub(crate) fn reset_chunks(&self, screen: &mut SessionScreen) {
        screen.chunks.timer = None;
        screen.chunks.timer_generation = None;
        screen.chunks.assembler.reset();
        screen.chunks.snapshot_coord_recv_ms = None;
    }

    pub(crate) fn cancel_request_timer(&self, screen: &mut SessionScreen, reset_attempt: bool) {
        screen.repair.request_timer = None;
        if reset_attempt {
            screen.repair.request_attempt = 0;
        }
    }

    pub(crate) fn cancel_baseline_timer(&self, screen: &mut SessionScreen) {
        screen.repair.baseline_timer = None;
    }

    /// A baseline landed: every deadline of this repair is done.
    pub(crate) fn complete_repair(&self, screen: &mut SessionScreen) {
        self.cancel_request_timer(screen, true);
        self.cancel_baseline_timer(screen);
        self.reset_chunks(screen);
    }

    /// Forget every deadline, and on a stream change advance the generation so
    /// any still in flight finds nothing to act on.
    pub(crate) fn reset_repair(&self, screen: &mut SessionScreen, advance_generation: bool) {
        self.complete_repair(screen);
        if advance_generation {
            screen.repair.generation += 1;
        }
    }

    fn request_resync(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        reason: &str,
        retry: bool,
        effects: &mut Vec<ScreenEffect>,
    ) {
        let Some(expected) = &screen.expected else {
            return;
        };
        if !retry && screen.resync_latched {
            return;
        }
        screen.resync_latched = true;
        tracing::info!(session_id = %session_id, stream_id = %expected.stream_id, reason, "terminal.screen_resync");
        self.start_snapshot_request(session_id, screen, reason, effects);
    }

    fn start_snapshot_request(
        self: &Arc<Self>,
        session_id: &SessionId,
        screen: &mut SessionScreen,
        reason: &str,
        effects: &mut Vec<ScreenEffect>,
    ) {
        let Some(expected) = &screen.expected else {
            return;
        };
        if screen.repair.request_timer.is_some()
            || screen.repair.request_attempt >= SNAPSHOT_REQUEST_ATTEMPTS
        {
            return;
        }
        let stream_id = expected.stream_id.clone();
        let generation = screen.repair.generation;
        screen.repair.request_attempt += 1;
        let attempt = screen.repair.request_attempt;
        let token = self.next_token();
        screen.repair.request_timer = Some(token);
        // One first-byte deadline per session: this request escalates on its own.
        self.cancel_baseline_timer(screen);
        let hub = Arc::downgrade(self);
        let deadline_session = session_id.clone();
        let deadline_stream = stream_id.clone();
        let reason = reason.to_owned();
        let delay = TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS;
        self.timers.schedule(
            delay,
            Box::new(move || {
                if let Some(hub) = hub.upgrade() {
                    hub.on_request_deadline(
                        &deadline_session,
                        token,
                        generation,
                        &deadline_stream,
                        attempt,
                        &reason,
                    );
                }
            }),
        );
        effects.push(ScreenEffect::RequestSnapshot {
            session_id: session_id.clone(),
            stream_id,
        });
    }

    fn on_request_deadline(
        self: &Arc<Self>,
        session_id: &SessionId,
        token: u64,
        generation: u64,
        stream_id: &str,
        attempt: u32,
        reason: &str,
    ) {
        let mut effects = Vec::new();
        {
            let mut sessions = self.locked_sessions();
            let Some(screen) = sessions.get_mut(session_id) else {
                return;
            };
            if screen.repair.request_timer != Some(token) {
                return;
            }
            screen.repair.request_timer = None;
            if screen.repair.generation != generation
                || screen
                    .expected
                    .as_ref()
                    .map(|expected| expected.stream_id.as_str())
                    != Some(stream_id)
            {
                return;
            }
            if attempt == 1 {
                let reason = "terminal snapshot request produced no bytes";
                self.start_snapshot_request(session_id, screen, reason, &mut effects);
            } else {
                tracing::warn!(session_id = %session_id, stream_id, "terminal snapshot repair timed out twice; a fresh stream is owed");
                effects.push(ScreenEffect::RequestFreshStream {
                    session_id: session_id.clone(),
                    stream_id: stream_id.to_owned(),
                    reason: format!("terminal snapshot repair timed out: {reason}"),
                });
            }
        }
        self.run_effects(effects);
    }

    fn on_baseline_deadline(
        self: &Arc<Self>,
        session_id: &SessionId,
        token: u64,
        generation: u64,
        stream_id: &str,
    ) {
        let mut effects = Vec::new();
        {
            let mut sessions = self.locked_sessions();
            let Some(screen) = sessions.get_mut(session_id) else {
                return;
            };
            if screen.repair.baseline_timer != Some(token) {
                return;
            }
            screen.repair.baseline_timer = None;
            if screen.repair.generation != generation
                || screen
                    .expected
                    .as_ref()
                    .map(|expected| expected.stream_id.as_str())
                    != Some(stream_id)
                || screen.charge.current().is_some_and(|cache| cache.valid)
                || screen.assembling()
            {
                return;
            }
            tracing::warn!(session_id = %session_id, stream_id, "terminal-screen baseline_timeout");
            let reason = "terminal stream baseline never arrived";
            self.latch(session_id, screen, reason, &mut effects);
        }
        self.run_effects(effects);
    }

    fn on_chunk_deadline(self: &Arc<Self>, session_id: &SessionId, token: u64, generation: u64) {
        let mut effects = Vec::new();
        {
            let mut sessions = self.locked_sessions();
            let Some(screen) = sessions.get_mut(session_id) else {
                return;
            };
            if screen.chunks.timer != Some(token)
                || screen.chunks.timer_generation != Some(generation)
            {
                return;
            }
            screen.chunks.timer = None;
            screen.chunks.timer_generation = None;
            if !screen.chunks.assembler.expire((self.clock)()) {
                return;
            }
            screen.chunks.snapshot_coord_recv_ms = None;
            screen.hold.clear();
            let reason = "terminal snapshot chunk transfer stalled";
            self.retry(session_id, screen, reason, &mut effects);
        }
        self.run_effects(effects);
    }
}
