//! Applying a membership decision: recompute what moved, then answer.
//!
//! Split out of `mod.rs` for the size cap. Every method here runs AFTER the
//! registry guard is dropped, which is the whole point: the recompute reads the
//! registry, so a decision that recomputed under its own lock would deadlock.

use std::collections::HashMap;
use std::sync::MutexGuard;

use roost_protocol::viewport::{TerminalGeometry, minimum_terminal_geometry};
use roost_protocol::wire::{SessionId, WorkerFp};

use super::registry::{MembershipOutcome, ViewRegistry};
use super::sink::{PendingReply, SinkCall, view_state_frame};
use super::TerminalViewHub;

impl TerminalViewHub {
    /// Recompute a session's effective geometry, reporting the size it now runs
    /// at. Empty input HOLDS the last effective geometry rather than re-minting
    /// one: a link that flaps must not tear a stream down and rebuild it a
    /// second later.
    pub(super) fn recompute(
        &self,
        session_id: &SessionId,
        now_ms: u64,
    ) -> Option<TerminalGeometry> {
        let live = self.locked().geometry_set(session_id, now_ms).live;
        let candidate = minimum_terminal_geometry(&live).ok().flatten();
        let mut effective = self.locked_effective();
        match candidate {
            Some(geometry) => {
                if effective.get(session_id) != Some(&geometry) {
                    effective.insert(session_id.clone(), geometry);
                    tracing::info!(
                        %session_id,
                        cols = geometry.cols,
                        rows = geometry.rows,
                        constraining = live.len(),
                        "terminal view effective geometry recomputed"
                    );
                }
                Some(geometry)
            }
            None => effective.get(session_id).copied(),
        }
    }

    /// Apply one membership decision: recompute what moved, then answer.
    pub(super) fn settle(&self, outcome: MembershipOutcome, now_ms: u64) {
        if outcome.is_empty() {
            return;
        }
        for session_id in &outcome.changed {
            self.recompute(session_id, now_ms);
        }
        self.deliver(&outcome.replies, &outcome.calls, now_ms);
    }

    /// Perform the effects the membership machine decided.
    pub(super) fn deliver(
        &self,
        replies: &[PendingReply],
        calls: &[SinkCall],
        now_ms: u64,
    ) {
        for call in calls {
            let Some(socket) = self.locked().socket(call_socket(call)) else {
                continue;
            };
            match call {
                SinkCall::Watching {
                    session_id,
                    watching,
                    ..
                } => socket.sink.set_watching(&socket.id, session_id, *watching),
                SinkCall::Resync {
                    session_id,
                    grid_epoch,
                    seq,
                    ..
                } => socket
                    .sink
                    .resync_socket(&socket.id, session_id, grid_epoch, *seq),
                SinkCall::LiveViewExpired {
                    view_id,
                    session_id,
                    ..
                } => {
                    tracing::warn!(
                        socket_id = %socket.id,
                        view_id = %view_id,
                        %session_id,
                        "a terminal view lease lapsed; closing the socket that stopped heartbeating"
                    );
                    socket.sink.live_view_expired(&socket.id, view_id, session_id);
                }
            }
        }
        for reply in replies {
            let Some(socket) = self.locked().socket(reply.socket_id()) else {
                continue;
            };
            let frame = match reply {
                PendingReply::Command {
                    view_id,
                    session_id,
                    revision,
                    active,
                    status,
                    reason,
                    ..
                } => view_state_frame(view_id, session_id, *revision, *active, "", *status, 0, 0, reason),
                PendingReply::View {
                    view_id,
                    session_id,
                    revision,
                    status,
                    reason,
                    ..
                } => {
                    let geometry = SessionId::try_from(session_id.clone())
                        .ok()
                        .and_then(|id| self.session_geometry(&id, now_ms));
                    let (cols, rows) = geometry.map_or((0, 0), |geometry| {
                        (geometry.cols, geometry.rows)
                    });
                    view_state_frame(
                        view_id,
                        session_id,
                        *revision,
                        true,
                        "",
                        *status,
                        cols,
                        rows,
                        reason,
                    )
                }
            };
            socket
                .sink
                .enqueue_terminal_state(&socket.id, frame, reply.session_id());
        }
    }

    /// The owner-mode worker that owns a session's views, if one does.
    pub(super) fn owner_of(&self, session_id: &str) -> Option<WorkerFp> {
        let session_id = SessionId::try_from(session_id).ok()?;
        self.owners.owner_for_session(&session_id)
    }

    pub(super) fn locked(&self) -> MutexGuard<'_, ViewRegistry> {
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn locked_effective(&self) -> MutexGuard<'_, HashMap<SessionId, TerminalGeometry>> {
        self.effective
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The socket a host effect is about.
fn call_socket(call: &SinkCall) -> &str {
    match call {
        SinkCall::Watching { socket_id, .. }
        | SinkCall::Resync { socket_id, .. }
        | SinkCall::LiveViewExpired { socket_id, .. } => socket_id,
    }
}
