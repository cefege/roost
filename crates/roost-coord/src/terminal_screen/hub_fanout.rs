//! The screen hub's socket half: which sockets watch which session, seeding a
//! socket from the replica, and carrying out what an admission decided once
//! every hub lock is released.
//!
//! Ports the socket fan-out of `apps/coord/src/terminal/screen/terminal-screen-hub.ts`
//! (`registerSocket`, `setWatching`, `seedSocket`, `resyncSocket`,
//! `forEachWatcher`). Called by the Sync socket's view sink and its rebaseline
//! hub; the effects are produced by `replica`, `replica_admission` and
//! `snapshot_controller`.

use std::sync::Arc;

use roost_proto::FirehoseFrame;
use roost_protocol::wire::SessionId;

use crate::sync_ws::terminal::TerminalDeltaOutcome;
use crate::sync_ws::terminal::snapshot::TerminalSnapshotSource;
use crate::terminal_screen::hub_contract::TerminalScreenSocketSink;
use crate::terminal_screen::hub_state::{
    ScreenCheckpoint, SocketRegistration, Watcher, resident_cache_supersedes_checkpoint,
};
use crate::terminal_screen::replica::ScreenHub;
use crate::terminal_screen::snapshot_source::ResidentSnapshotSource;

/// One thing an admission owes the outside world, performed after the hub's
/// locks are released so a socket that re-enters the hub cannot deadlock it.
pub(crate) enum ScreenEffect {
    Begin {
        watcher: Watcher,
        session_id: SessionId,
        stream_id: String,
    },
    Seed {
        watcher: Watcher,
        session_id: SessionId,
        stream_id: String,
        source: Arc<ResidentSnapshotSource>,
    },
    Delta {
        watcher: Watcher,
        session_id: SessionId,
        stream_id: String,
        frame: Arc<FirehoseFrame>,
        /// The cache version the delta produced, which a socket that refuses
        /// it is seeded from instead.
        generation: u64,
    },
    RequestSnapshot {
        session_id: SessionId,
        stream_id: String,
    },
    RequestFreshStream {
        session_id: SessionId,
        stream_id: String,
        reason: String,
    },
    Unavailable {
        session_id: SessionId,
        reason: String,
    },
    FullAccepted {
        session_id: SessionId,
        stream_id: String,
    },
}

/// What a socket asking for a rebaseline must do on its own lane.
pub struct RebaselinePlan {
    /// The stream the replica expects; the lane restarts on it if different.
    pub stream_id: String,
    /// A resident full to install, or `None` while a source full is requested.
    pub source: Option<Arc<dyn TerminalSnapshotSource>>,
}

impl std::fmt::Debug for RebaselinePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RebaselinePlan")
            .field("stream_id", &self.stream_id)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl ScreenHub {
    /// Admit a socket, replacing any earlier registration under its id.
    pub fn register_socket(
        self: &Arc<Self>,
        socket_id: &str,
        sink: Arc<dyn TerminalScreenSocketSink>,
    ) {
        self.unregister_socket(socket_id);
        let token = self.next_token();
        let mut sockets = self.locked_sockets();
        // The replaced registration's drop callbacks ran with the index
        // unlocked, and one may have installed a newer registration under this
        // id: that one wins, and overwriting it would orphan its watches.
        if sockets.sockets.contains_key(socket_id) {
            tracing::debug!(
                socket_id,
                "terminal screen socket re-registered during its own retirement"
            );
            return;
        }
        sockets.sockets.insert(
            socket_id.to_owned(),
            SocketRegistration {
                sink,
                watched: std::collections::BTreeSet::new(),
                token,
            },
        );
        drop(sockets);
        tracing::debug!(socket_id, "terminal screen socket registered");
    }

    /// A socket is gone: every session it watched loses its lane on it.
    pub fn unregister_socket(self: &Arc<Self>, socket_id: &str) {
        let detached = self.locked_sockets().detach_socket(socket_id);
        let Some((sink, watched)) = detached else {
            return;
        };
        tracing::debug!(
            socket_id,
            watched = watched.len(),
            "terminal screen socket unregistered"
        );
        for session_id in &watched {
            sink.drop_terminal_session(session_id);
        }
    }

    /// Start or stop a socket watching a session. A socket that starts is put
    /// on the stream the replica expects; one that stops loses its lane.
    pub fn set_watching(self: &Arc<Self>, socket_id: &str, session_id: &SessionId, watching: bool) {
        let begin = {
            let sessions = self.locked_sessions();
            let mut sockets = self.locked_sockets();
            if !watching {
                if !sockets.detach(socket_id, session_id) {
                    return;
                }
                let sink = sockets
                    .sockets
                    .get(socket_id)
                    .map(|socket| Arc::clone(&socket.sink));
                drop(sockets);
                drop(sessions);
                if let Some(sink) = sink {
                    sink.drop_terminal_session(session_id);
                }
                return;
            }
            if !sockets.attach(socket_id, session_id) {
                return;
            }
            let expected = sessions
                .get(session_id)
                .and_then(|screen| screen.expected.as_ref())
                .map(|expected| expected.stream_id.clone());
            expected.zip(sockets.watcher(socket_id, session_id))
        };
        if let Some((stream_id, watcher)) = begin {
            watcher.sink.begin_terminal_stream(session_id, &stream_id);
        }
    }

    /// Serve a watching socket the replica's current baseline; `false` when
    /// there is none to serve or the socket did not take it.
    pub fn seed_socket(self: &Arc<Self>, socket_id: &str, session_id: &SessionId) -> bool {
        let mut effects = Vec::new();
        let seed = {
            let mut sessions = self.locked_sessions();
            let Some(watcher) = self.locked_sockets().watcher(socket_id, session_id) else {
                return false;
            };
            let Some(screen) = sessions.get_mut(session_id) else {
                return false;
            };
            let Some(stream_id) = screen
                .expected
                .as_ref()
                .map(|expected| expected.stream_id.clone())
            else {
                return false;
            };
            if !screen.charge.current().is_some_and(|cache| cache.valid) {
                return false;
            }
            self.seed_source(session_id, screen, &mut effects)
                .map(|source| (watcher, stream_id, source))
        };
        self.run_effects(effects);
        let Some((watcher, stream_id, source)) = seed else {
            return false;
        };
        watcher
            .sink
            .replace_terminal_snapshot(session_id, &stream_id, source)
    }

    /// Serve a socket forward from its checkpoint, seeding it from the replica
    /// when the resident baseline already covers what the checkpoint lacks and
    /// asking the worker for a source full otherwise.
    pub fn resync_socket(
        self: &Arc<Self>,
        socket_id: &str,
        session_id: &SessionId,
        checkpoint: Option<&ScreenCheckpoint>,
    ) -> bool {
        let Some(watcher) = self.locked_sockets().watcher(socket_id, session_id) else {
            return false;
        };
        let Some(plan) = self.rebaseline_plan(socket_id, session_id, checkpoint) else {
            return false;
        };
        watcher
            .sink
            .begin_terminal_stream(session_id, &plan.stream_id);
        plan.source.is_some_and(|source| {
            watcher
                .sink
                .replace_terminal_snapshot(session_id, &plan.stream_id, source)
        })
    }

    /// Decide a resync without touching the socket, for a socket that is
    /// asking from inside its own lock and performs the plan itself. `None`
    /// when the socket does not watch the session or nothing is expected.
    pub fn rebaseline_plan(
        self: &Arc<Self>,
        socket_id: &str,
        session_id: &SessionId,
        checkpoint: Option<&ScreenCheckpoint>,
    ) -> Option<RebaselinePlan> {
        let mut effects = Vec::new();
        let plan = {
            let mut sessions = self.locked_sessions();
            self.locked_sockets().watcher(socket_id, session_id)?;
            let screen = sessions.get_mut(session_id)?;
            let stream_id = screen.expected.as_ref()?.stream_id.clone();
            let servable = !screen.resync_latched
                && screen.charge.current().is_some_and(|cache| {
                    cache.valid && resident_cache_supersedes_checkpoint(cache, checkpoint)
                });
            let source = if servable {
                self.seed_source(session_id, screen, &mut effects)
            } else {
                let reason = "browser checkpoint requires source baseline";
                self.retry(session_id, screen, reason, &mut effects);
                None
            };
            RebaselinePlan {
                stream_id,
                source: source.map(|source| source as Arc<dyn TerminalSnapshotSource>),
            }
        };
        self.run_effects(effects);
        Some(plan)
    }

    /// Carry out what an admission decided, with every hub lock released.
    ///
    /// A socket effect reaches its watcher only if that registration still
    /// watches the session when its turn comes: an earlier sink in the same
    /// fan-out may have unregistered it (v2 `forEachWatcher` re-reads the
    /// index per callback).
    pub(crate) fn run_effects(self: &Arc<Self>, effects: Vec<ScreenEffect>) {
        for effect in effects {
            match effect {
                ScreenEffect::Begin {
                    watcher,
                    session_id,
                    stream_id,
                } => {
                    if self.still_watching(&watcher, &session_id) {
                        watcher.sink.begin_terminal_stream(&session_id, &stream_id);
                    }
                }
                ScreenEffect::Seed {
                    watcher,
                    session_id,
                    stream_id,
                    source,
                } => {
                    if self.still_watching(&watcher, &session_id) {
                        watcher
                            .sink
                            .replace_terminal_snapshot(&session_id, &stream_id, source);
                    }
                }
                ScreenEffect::Delta {
                    watcher,
                    session_id,
                    stream_id,
                    frame,
                    generation,
                } => {
                    if !self.still_watching(&watcher, &session_id) {
                        continue;
                    }
                    let outcome =
                        watcher
                            .sink
                            .enqueue_terminal_delta(&session_id, &stream_id, &frame);
                    if outcome == TerminalDeltaOutcome::NeedsSnapshot {
                        self.reseed_after_refused_delta(
                            &watcher,
                            &session_id,
                            &stream_id,
                            generation,
                        );
                    }
                }
                ScreenEffect::RequestSnapshot {
                    session_id,
                    stream_id,
                } => {
                    self.sink.request_snapshot(&session_id, &stream_id);
                }
                ScreenEffect::RequestFreshStream {
                    session_id,
                    stream_id,
                    reason,
                } => {
                    self.sink
                        .request_fresh_stream(&session_id, &stream_id, &reason);
                }
                ScreenEffect::Unavailable { session_id, reason } => {
                    self.sink.unavailable(&session_id, &reason);
                }
                ScreenEffect::FullAccepted {
                    session_id,
                    stream_id,
                } => {
                    self.sink.full_accepted(&session_id, &stream_id);
                }
            }
        }
    }

    /// Whether this exact registration still watches the session.
    fn still_watching(&self, watcher: &Watcher, session_id: &SessionId) -> bool {
        self.locked_sockets()
            .watcher(&watcher.socket_id, session_id)
            .is_some_and(|current| current.token == watcher.token)
    }

    /// A socket dropped a delta and nothing is on its way: seed it from the
    /// cache that delta produced, if that registration still watches and that
    /// version is still current. A newer version's own fan-out reaches it
    /// otherwise.
    fn reseed_after_refused_delta(
        self: &Arc<Self>,
        watcher: &Watcher,
        session_id: &SessionId,
        stream_id: &str,
        generation: u64,
    ) {
        let mut effects = Vec::new();
        let source = {
            let mut sessions = self.locked_sessions();
            let registered = self
                .locked_sockets()
                .watcher(&watcher.socket_id, session_id)
                .is_some_and(|current| current.token == watcher.token);
            let screen = sessions.get_mut(session_id);
            match screen {
                Some(screen)
                    if registered
                        && screen.charge.current().map(|cache| cache.generation)
                            == Some(generation) =>
                {
                    self.seed_source(session_id, screen, &mut effects)
                }
                _ => None,
            }
        };
        self.run_effects(effects);
        if let Some(source) = source {
            watcher
                .sink
                .replace_terminal_snapshot(session_id, stream_id, source);
        }
    }
}
