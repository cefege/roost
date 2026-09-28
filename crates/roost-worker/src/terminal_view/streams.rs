//! Per-session terminal stream authority for the worker's view owner: it
//! minimizes the registry's live viewer geometry, mints every stream id, drives
//! `SessionManager::apply_terminal_stream_state`, and classifies the outcome.
//! Desires coalesce per session, so a dragged pane resize cannot queue one keeper
//! resize per intermediate width. Membership itself lives in the registry.
//! Ports `apps/worker/src/terminal/view/terminal-view-owner-streams.ts`.

use roost_proto::TerminalViewStatus;
use roost_protocol::terminal_view::truncate_view_reason;
use roost_protocol::viewport::{TerminalGeometry, minimum_terminal_geometry};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::TerminalStreamFailureKind;

use crate::session::ids::mint_uuid;
use crate::session::terminal_state::WorkerStreamResult;

use super::deferred::{Deferred, StreamApply, Work};
use super::state::OwnerState;

/// Why a session is not paintable right now, which decides what a rejoining
/// view is told: redrive the stream and let the next decision answer, or
/// replay UNAVAILABLE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnavailablePolicy {
    Heartbeat,
    Never,
}

impl UnavailablePolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Heartbeat => "heartbeat",
            Self::Never => "never",
        }
    }
}

/// One desired stream state waiting for the control lane.
#[derive(Debug, Clone, Copy)]
pub(super) struct Desire {
    geometry: Option<TerminalGeometry>,
    retry: u8,
}

/// The stream this owner holds for one session (v2 `TerminalStreamState` plus
/// the coalescing slots).
#[derive(Debug, Clone)]
pub(super) struct StreamSession {
    pub(super) effective: Option<TerminalGeometry>,
    pub(super) stream_id: String,
    pub(super) unavailable: bool,
    pub(super) unavailable_reason: String,
    policy: UnavailablePolicy,
    /// Stream id of the apply currently awaiting the control lane.
    in_flight: Option<String>,
    latest: Option<Desire>,
    incarnation: u64,
}

impl StreamSession {
    pub(super) fn policy_is_heartbeat(&self) -> bool {
        self.policy == UnavailablePolicy::Heartbeat
    }
}

impl OwnerState {
    /// Re-minimize one session. `true` when this changed the stream, which
    /// means the decision was already broadcast to every live view.
    pub(super) fn recompute(
        &mut self,
        session_id: &SessionId,
        now_ms: u64,
        work: &mut Work,
    ) -> bool {
        let set = self.registry.geometry_set(session_id, now_ms);
        self.stream_entry(session_id);
        // A session whose every viewer is parked HOLDS its last geometry: park
        // absorbs reconnect wobble, so a solo viewer's socket blip must not
        // re-mint the stream or resize the PTY. Losing membership entirely is
        // what disables it.
        if set.live.is_empty() && set.retained > 0 {
            self.mark_projection(session_id, work);
            return false;
        }
        let effective = match minimum_terminal_geometry(&set.live) {
            Ok(effective) => effective,
            Err(error) => {
                tracing::error!(%session_id, %error, "terminal view geometry could not be minimized");
                return false;
            }
        };
        let unchanged = self
            .streams
            .get(session_id)
            .is_some_and(|stream| stream.effective == effective);
        if unchanged {
            self.mark_projection(session_id, work);
            return false;
        }
        if let Some(stream) = self.streams.get_mut(session_id) {
            stream.effective = effective;
        }
        self.desire(session_id, effective, 0, work);
        self.mark_projection(session_id, work);
        true
    }

    /// v2 `redrive`: a heartbeat-policy session tries its geometry again.
    pub(super) fn redrive(&mut self, session_id: &SessionId, work: &mut Work) {
        let Some(stream) = self.streams.get(session_id) else {
            return;
        };
        if let Some(effective) = stream.effective
            && stream.policy != UnavailablePolicy::Never
        {
            self.desire(session_id, Some(effective), 0, work);
        }
    }

    /// The session closed: its stream identity and geometry go with it.
    pub(super) fn close_stream(&mut self, session_id: &SessionId, work: &mut Work) {
        if self.streams.remove(session_id).is_none() {
            return;
        }
        self.mark_projection(session_id, work);
    }

    /// An apply resolved: classify it, then drive whatever desire queued behind it.
    pub(super) fn finish_apply(
        &mut self,
        apply: &StreamApply,
        result: &WorkerStreamResult,
        work: &mut Work,
    ) {
        let current = |state: &Self| {
            state
                .streams
                .get(&apply.session_id)
                .is_some_and(|stream| stream.incarnation == apply.incarnation)
        };
        if !current(self) {
            return;
        }
        self.classify(apply, result, work);
        if !current(self) {
            return;
        }
        let Some(stream) = self.streams.get_mut(&apply.session_id) else {
            return;
        };
        if stream.in_flight.as_deref() == Some(apply.stream_id.as_str()) {
            stream.in_flight = None;
        }
        if stream.latest.is_some() {
            self.drive(&apply.session_id, work);
        }
    }

    /// Whether `stream_id` is still the stream this session is desiring.
    pub(super) fn stream_is_current(&self, session_id: &SessionId, stream_id: &str) -> bool {
        !self.disposed
            && self
                .streams
                .get(session_id)
                .is_some_and(|stream| stream.stream_id == stream_id)
    }

    fn stream_entry(&mut self, session_id: &SessionId) {
        if self.streams.contains_key(session_id) {
            return;
        }
        self.incarnations += 1;
        self.streams.insert(
            session_id.clone(),
            StreamSession {
                effective: None,
                stream_id: String::new(),
                unavailable: false,
                unavailable_reason: String::new(),
                policy: UnavailablePolicy::Heartbeat,
                in_flight: None,
                latest: None,
                incarnation: self.incarnations,
            },
        );
    }

    fn desire(
        &mut self,
        session_id: &SessionId,
        geometry: Option<TerminalGeometry>,
        retry: u8,
        work: &mut Work,
    ) {
        let stream_id = match mint_uuid() {
            Ok(stream_id) => stream_id,
            Err(error) => {
                self.unavailable(session_id, &error.to_string(), UnavailablePolicy::Never);
                return;
            }
        };
        let Some(stream) = self.streams.get_mut(session_id) else {
            return;
        };
        stream.stream_id = stream_id;
        stream.unavailable = false;
        stream.unavailable_reason.clear();
        stream.policy = UnavailablePolicy::Heartbeat;
        stream.latest = Some(Desire { geometry, retry });
        tracing::info!(
            %session_id,
            stream_id = %stream.stream_id,
            enabled = geometry.is_some(),
            cols = geometry.map_or(0, |geometry| geometry.cols),
            rows = geometry.map_or(0, |geometry| geometry.rows),
            retry,
            "terminal view stream desired"
        );
        // Every live view learns this stream BEFORE any of its cells exist: the
        // apply below is what installs the stream, and it runs after this.
        self.broadcast(session_id, TerminalViewStatus::Accepted, "");
        self.drive(session_id, work);
    }

    fn drive(&mut self, session_id: &SessionId, work: &mut Work) {
        let Some(stream) = self.streams.get_mut(session_id) else {
            return;
        };
        if stream.in_flight.is_some() {
            return;
        }
        let Some(desire) = stream.latest.take() else {
            return;
        };
        stream.in_flight = Some(stream.stream_id.clone());
        work.push(Deferred::Apply(StreamApply {
            session_id: session_id.clone(),
            stream_id: stream.stream_id.clone(),
            incarnation: stream.incarnation,
            geometry: desire.geometry,
            retry: desire.retry,
        }));
    }

    fn classify(&mut self, apply: &StreamApply, result: &WorkerStreamResult, work: &mut Work) {
        let Some(stream) = self.streams.get(&apply.session_id) else {
            return;
        };
        if stream.stream_id != apply.stream_id {
            return;
        }
        let effective = stream.effective;
        let (failure, reason) = match result {
            WorkerStreamResult::Committed { .. } => return,
            WorkerStreamResult::Rejected {
                failure, reason, ..
            }
            | WorkerStreamResult::Ambiguous {
                failure, reason, ..
            } => (*failure, reason.as_str()),
        };
        // One retry, and only for a failure that provably never wrote: the
        // admission lane refuses while another transaction owns the channel.
        if failure == TerminalStreamFailureKind::RetryablePreWrite && apply.retry == 0 {
            self.desire(&apply.session_id, effective, 1, work);
            return;
        }
        // A trap the keeper boundary caused is the one failure the worker can
        // repair itself: the next apply re-proves the core from keeper history.
        // One attempt per trap, driven by the trap and never by a timer; if it
        // fails the verdict stays fail-closed.
        if failure == TerminalStreamFailureKind::CoreFailed
            && apply.retry == 0
            && effective.is_some()
        {
            self.desire(&apply.session_id, effective, 1, work);
            return;
        }
        let message = if reason.is_empty() {
            "terminal stream is unavailable"
        } else {
            reason
        };
        let policy = if failure == TerminalStreamFailureKind::RetryablePreWrite {
            UnavailablePolicy::Heartbeat
        } else {
            UnavailablePolicy::Never
        };
        let message = message.to_owned();
        self.unavailable(&apply.session_id, &message, policy);
    }

    fn unavailable(&mut self, session_id: &SessionId, message: &str, policy: UnavailablePolicy) {
        let Some(stream) = self.streams.get_mut(session_id) else {
            return;
        };
        if stream.effective.is_none() {
            return;
        }
        stream.unavailable = true;
        stream.unavailable_reason = truncate_view_reason(message);
        stream.policy = policy;
        tracing::warn!(
            %session_id,
            stream_id = %stream.stream_id,
            policy = policy.as_str(),
            reason = %stream.unavailable_reason,
            "terminal view stream unavailable"
        );
        let reason = stream.unavailable_reason.clone();
        self.broadcast(session_id, TerminalViewStatus::Unavailable, &reason);
    }
}
