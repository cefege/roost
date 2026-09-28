//! One Sync v2 socket as the terminal screen hub and the view hub reach it:
//! the per-socket sinks over the shared link, and the rebaseline hub the
//! socket's own lanes ask from inside the link lock.
//!
//! Ports the `registerSocket` sink closures, `requestTerminalRebaseline` and
//! `setOnLiveViewExpired` of `apps/coord/src/sync/sync-ws-handler.ts:109-113,160-179,266-297`.
//! `socket_open` builds one per v2 socket; `driver` settles the rebaselines.

use std::sync::{Arc, Weak};

use roost_proto::FirehoseFrame;
use roost_protocol::terminal_view::TerminalViewSink;
use roost_protocol::wire::SessionId;

use crate::services::CoordServices;
use crate::sync_ws::driver::{Delivery, LinkClose, LinkState, SyncLink, now_ms};
use crate::sync_ws::terminal::TerminalDeltaOutcome;
use crate::sync_ws::terminal::snapshot::{TerminalSnapshotHub, TerminalSnapshotSource};
use crate::terminal_screen::hub_contract::TerminalScreenSocketSink;
use crate::terminal_screen::hub_fanout::RebaselinePlan;
use crate::terminal_screen::hub_state::ScreenCheckpoint;
use crate::terminal_screen::replica::ScreenHub;
use crate::terminal_view::{SocketRegistration, SocketScope};

/// The close a socket gets when one of its live views stopped heartbeating
/// (`sync-ws-handler.ts:173`).
pub const TERMINAL_VIEW_LEASE_EXPIRED: LinkClose = LinkClose {
    code: 1013,
    reason: "terminal view lease expired",
};

/// Rebaselines one settle performs before leaving the rest for the next turn:
/// installing a full pumps its lane, which may ask again.
const REBASELINE_ROUNDS: usize = 4;

/// The screen hub a socket's lanes ask for a rebaseline. The answer is
/// deferred: the lane asks with the socket's lock held, so the plan is carried
/// out by [`LinkState::settle_screen_rebaselines`] once the lane returns.
pub struct SocketScreenHub {
    screens: Arc<ScreenHub>,
    deferred: Vec<(SessionId, RebaselinePlan)>,
}

impl std::fmt::Debug for SocketScreenHub {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SocketScreenHub")
            .field("deferred", &self.deferred.len())
            .finish()
    }
}

impl SocketScreenHub {
    /// The process's screen hub, as one socket asks it.
    #[must_use]
    pub fn new(screens: Arc<ScreenHub>) -> Self {
        Self {
            screens,
            deferred: Vec::new(),
        }
    }

    /// Whether a plan is waiting to be carried out.
    #[must_use]
    pub fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }
}

impl TerminalSnapshotHub for SocketScreenHub {
    /// Always `false`: the full, when the replica holds one, is installed by
    /// the settle that follows, and the lane stays ready until then.
    fn request_rebaseline(&mut self, socket_id: &str, session_id: &str) -> bool {
        let Ok(session_id) = SessionId::try_from(session_id) else {
            return false;
        };
        if let Some(plan) = self.screens.rebaseline_plan(socket_id, &session_id, None) {
            self.deferred.push((session_id, plan));
        }
        false
    }
}

impl LinkState {
    /// Carry out the rebaselines this socket's lanes asked for; `true` when a
    /// full was installed and the caller owes another flush turn.
    pub(in crate::sync_ws) fn settle_screen_rebaselines(&mut self, now_ms: u64) -> bool {
        let mut installed = false;
        for _ in 0..REBASELINE_ROUNDS {
            if !self.screen.has_deferred() {
                break;
            }
            let deferred = std::mem::take(&mut self.screen.deferred);
            let Delivery::V2(session) = &mut self.delivery else {
                return installed;
            };
            for (session_id, plan) in deferred {
                session.begin_terminal_stream(session_id.as_str(), &plan.stream_id);
                let Some(source) = plan.source else {
                    continue;
                };
                let Some(snapshot_id) = mint_snapshot_id() else {
                    continue;
                };
                installed |= session.replace_terminal_snapshot(
                    session_id.as_str(),
                    &plan.stream_id,
                    &*source,
                    &snapshot_id,
                    now_ms,
                    &mut self.screen,
                );
            }
        }
        installed
    }
}

/// One v2 socket as a screen watcher, a view holder and a live scope.
pub struct SyncScreenSocket {
    socket_id: String,
    link: Weak<SyncLink>,
    screens: Arc<ScreenHub>,
}

impl SyncScreenSocket {
    /// The sinks of `link`, which is `socket_id`. Weak: the hubs outlive a
    /// socket, and a closed socket's registrations are released, not kept.
    #[must_use]
    pub fn new(socket_id: &str, link: &Arc<SyncLink>, screens: Arc<ScreenHub>) -> Arc<Self> {
        Arc::new(Self {
            socket_id: socket_id.to_owned(),
            link: Arc::downgrade(link),
            screens,
        })
    }

    /// Run `apply` on this socket's v2 session with the link locked, settle
    /// what its lanes asked for, and wake the socket task. `fallback` answers
    /// for a socket that is gone or closing.
    fn on_session<R>(&self, fallback: R, apply: impl FnOnce(&mut LinkState, u64) -> R) -> R {
        let Some(link) = self.link.upgrade() else {
            return fallback;
        };
        let mut result = None;
        link.deliver_with(|state| {
            let now_ms = now_ms();
            if matches!(state.delivery, Delivery::V2(_)) {
                result = Some(apply(state, now_ms));
                state.settle_screen_rebaselines(now_ms);
            }
            None
        });
        result.unwrap_or(fallback)
    }
}

impl TerminalScreenSocketSink for SyncScreenSocket {
    fn begin_terminal_stream(&self, session_id: &SessionId, stream_id: &str) -> bool {
        self.on_session(false, |state, _| match &mut state.delivery {
            Delivery::V2(session) => session.begin_terminal_stream(session_id.as_str(), stream_id),
            Delivery::V1(_) => false,
        })
    }

    fn replace_terminal_snapshot(
        &self,
        session_id: &SessionId,
        stream_id: &str,
        source: Arc<dyn TerminalSnapshotSource>,
    ) -> bool {
        let Some(snapshot_id) = mint_snapshot_id() else {
            return false;
        };
        self.on_session(false, |state, now_ms| {
            let LinkState {
                delivery, screen, ..
            } = state;
            match delivery {
                Delivery::V2(session) => session.replace_terminal_snapshot(
                    session_id.as_str(),
                    stream_id,
                    &*source,
                    &snapshot_id,
                    now_ms,
                    screen,
                ),
                Delivery::V1(_) => false,
            }
        })
    }

    fn enqueue_terminal_delta(
        &self,
        session_id: &SessionId,
        stream_id: &str,
        frame: &FirehoseFrame,
    ) -> TerminalDeltaOutcome {
        self.on_session(TerminalDeltaOutcome::Handled, |state, now_ms| {
            let LinkState {
                delivery, screen, ..
            } = state;
            match delivery {
                Delivery::V2(session) => session.enqueue_terminal_delta(
                    session_id.as_str(),
                    stream_id,
                    frame,
                    now_ms,
                    screen,
                ),
                Delivery::V1(_) => TerminalDeltaOutcome::Handled,
            }
        })
    }

    fn drop_terminal_session(&self, session_id: &SessionId) {
        self.on_session((), |state, _| {
            if let Delivery::V2(session) = &mut state.delivery {
                session.drop_terminal_session(session_id.as_str());
            }
        });
    }
}

impl TerminalViewSink for SyncScreenSocket {
    /// A view-state that does not fit even after the lane shed its own
    /// material closes the socket: the client cannot rebuild a lost decision.
    fn enqueue_terminal_state(&self, _socket_id: &str, frame: FirehoseFrame, session_id: &str) {
        self.on_session((), |state, now_ms| {
            let LinkState {
                delivery, screen, ..
            } = &mut *state;
            let Delivery::V2(session) = delivery else {
                return;
            };
            let outcome = session.enqueue_terminal_state(&frame, session_id, now_ms, screen);
            if let Err(close) = outcome {
                state.close_for_session(close, "terminal_view_state", now_ms);
            }
        });
    }

    fn set_watching(&self, socket_id: &str, session_id: &SessionId, watching: bool) {
        self.screens.set_watching(socket_id, session_id, watching);
    }

    fn seed_socket(&self, socket_id: &str, session_id: &SessionId) -> bool {
        self.screens.seed_socket(socket_id, session_id)
    }

    fn resync_socket(&self, socket_id: &str, session_id: &SessionId, grid_epoch: &str, seq: u64) {
        let checkpoint = ScreenCheckpoint {
            grid_epoch: grid_epoch.to_owned(),
            seq,
        };
        self.screens
            .resync_socket(socket_id, session_id, Some(&checkpoint));
    }

    /// Closes the owning socket `1013`, once: the first close decided wins, and
    /// a second expiry for the same socket finds it already closing.
    fn live_view_expired(&self, socket_id: &str, view_id: &str, session_id: &SessionId) {
        let Some(link) = self.link.upgrade() else {
            return;
        };
        link.deliver_with(|state| {
            tracing::warn!(event = "sync-ws", action = "terminal_view_lease_expired", socket_id, view_id, session_id = %session_id);
            state.decide_close(TERMINAL_VIEW_LEASE_EXPIRED, "terminal_view_lease_expired", "terminal_view", now_ms());
            None
        });
    }

    fn expect_stream(&self, session_id: &SessionId, stream_id: &str, cols: u32, rows: u32) {
        self.screens
            .expect_stream(session_id, stream_id, cols, rows);
    }

    fn expected_stream_id(&self, session_id: &SessionId) -> Option<String> {
        self.screens.expected_stream_id(session_id)
    }

    fn invalidate(&self, session_id: &SessionId, reason: &str) {
        self.screens.invalidate(session_id, reason);
    }
}

impl SocketScope for SyncScreenSocket {
    fn allows_session(&self, session_id: &str) -> bool {
        self.link
            .upgrade()
            .is_some_and(|link| link.lock().index.session_ids.contains(session_id))
    }
}

impl std::fmt::Debug for SyncScreenSocket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SyncScreenSocket")
            .field("socket_id", &self.socket_id)
            .finish_non_exhaustive()
    }
}

/// Register a v2 socket with the screen hub (as a watcher) and the view hub
/// (as a view holder with a live scope), as v2's `open` does
/// (`sync-ws-handler.ts:266`).
pub(in crate::sync_ws) fn register_terminal_socket(
    link: &Arc<SyncLink>,
    socket_id: &str,
    viewer_key: Option<String>,
    caller_fp: &str,
    services: &CoordServices,
) {
    let screens = Arc::clone(services.byte_hub.screens());
    let socket = SyncScreenSocket::new(socket_id, link, Arc::clone(&screens));
    screens.register_socket(
        socket_id,
        Arc::clone(&socket) as Arc<dyn TerminalScreenSocketSink>,
    );
    let session_ids = link.lock().index.session_ids.clone();
    services.views.register_socket(
        &SocketRegistration {
            socket_id: socket_id.to_owned(),
            viewer_key,
            caller_fingerprint: caller_fp.to_owned(),
            session_ids,
            sink: Arc::clone(&socket) as Arc<dyn TerminalViewSink>,
        },
        now_ms(),
    );
    services
        .views
        .register_socket_scope(socket_id, socket as Arc<dyn SocketScope>);
    tracing::debug!(
        event = "sync-ws",
        action = "terminal_socket_registered",
        socket_id
    );
}

/// Release a v2 socket's view and screen registrations, views first: its
/// views park and its owners hear of the close before its lanes are dropped.
pub(in crate::sync_ws) fn release_terminal_socket(socket_id: &str, services: &CoordServices) {
    services.views.close_socket(socket_id, now_ms());
    services.byte_hub.screens().unregister_socket(socket_id);
}

/// A fresh snapshot id for one recipient's cursor, as v2's `randomUUID()`.
fn mint_snapshot_id() -> Option<String> {
    match crate::coord_core::ids::draw::<16>() {
        Ok(bytes) => Some(crate::coord_core::ids::render_v4(bytes)),
        Err(error) => {
            tracing::warn!(error = %error, "a terminal snapshot id could not be drawn; the full was not installed");
            None
        }
    }
}
