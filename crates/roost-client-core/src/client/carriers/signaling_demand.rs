//! What a worker's machine is asked to hold a peer for — the views that want
//! its sessions, and the pre-warm that wants a peer ready before any does — and
//! how long that asking waited for a direct route. An `impl Signalling` block
//! beside `super::signaling`, called from its `step` and from the teardown
//! paths in `super::faults`; it owns the one measurement the route diagnostic
//! and the `terminal` log report as `time_to_direct_ms`.

use std::collections::BTreeSet;

use roost_protocol::terminal_peer::peer::TERMINAL_PEER_MAX_SESSIONS_PER_GRANT;

use crate::client::carriers::faults::{CarrierFault, FaultFallback};
use crate::client::carriers::grant::GrantInput;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::{CarrierEffect, PeerPhase};
use crate::terminal::token::TerminalToken;

/// When a worker's views began waiting for a direct route, and whether a
/// pre-warm was already preparing their peer then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectWait {
    started_ms: u64,
    prewarmed: bool,
}

impl Signalling {
    /// A view started or stopped wanting this session here.
    ///
    /// The first view is when the wait for a direct route starts, and the last
    /// one leaving ends it unmeasured: no view is waiting any more.
    pub(crate) fn demand(
        &mut self,
        session_id: String,
        active: bool,
        now_ms: u64,
    ) -> Vec<CarrierEffect> {
        self.advance_clock(now_ms);
        if !active {
            self.active_views = self.active_views.saturating_sub(1);
            if self.active_views == 0 {
                self.direct_wait = None;
            }
            self.demand.remove(&session_id);
            return self.replace_granted_demand();
        }
        if self.active_views == 0 {
            self.direct_wait = Some(DirectWait {
                started_ms: self.now_ms,
                prewarmed: !self.prewarm_sessions.is_empty(),
            });
        }
        self.active_views += 1;
        self.demand.insert(session_id);
        let mut out = self.replace_granted_demand();
        out.extend(self.start(self.now_ms));
        out
    }

    /// Hold a grant and a peer ready for `session_ids` though no view wants
    /// one; an empty set stops pre-warming and keeps whatever peer it brought
    /// up, as a view leaving does.
    pub(crate) fn prewarm(
        &mut self,
        session_ids: BTreeSet<String>,
        now_ms: u64,
    ) -> Vec<CarrierEffect> {
        self.advance_clock(now_ms);
        if self.prewarm_sessions == session_ids {
            return Vec::new();
        }
        let was_prewarmed = !self.prewarm_sessions.is_empty();
        self.prewarm_sessions = session_ids;
        match (was_prewarmed, self.prewarm_sessions.is_empty()) {
            (false, false) => tracing::info!(
                target: "carriers",
                worker_fp = %self.worker_fp,
                sessions = self.prewarm_sessions.len(),
                "direct peer pre-warm started"
            ),
            (true, true) => tracing::info!(
                target: "carriers",
                worker_fp = %self.worker_fp,
                "direct peer pre-warm stopped"
            ),
            _ => tracing::debug!(
                target: "carriers",
                worker_fp = %self.worker_fp,
                sessions = self.prewarm_sessions.len(),
                "direct peer pre-warm scope changed"
            ),
        }
        let mut out = self.replace_granted_demand();
        out.extend(self.start(self.now_ms));
        out
    }

    /// Stop pre-warming, and close the peer when no view wants it: the
    /// selection handed its slot under the document's cap to another worker,
    /// and a slot that stays held is a worker some view can never reach.
    pub(crate) fn release_prewarm(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        let mut out = self.prewarm(BTreeSet::new(), now_ms);
        if self.active_views > 0 || self.attempt.is_none() {
            return out;
        }
        out.extend(self.close_open_attempt("terminal peer pre-warm released"));
        self.peer_held = false;
        self.park_keeping_terminal_reason(PeerPhase::Idle);
        tracing::info!(
            target: "carriers",
            worker_fp = %self.worker_fp,
            "pre-warmed peer released for another worker"
        );
        out
    }

    /// A promotion committed on a route `token` names.
    ///
    /// Only the FIRST promotion on this machine's peer is the moment its views
    /// reached a direct route; every later one is another session joining a
    /// peer that was already elected, and logging each would count one
    /// election once per session.
    pub(crate) fn promotion_committed(&mut self, token: &TerminalToken, now_ms: u64) {
        self.advance_clock(now_ms);
        if !self.holds(token) {
            return;
        }
        self.faults.cleared();
        if self.phase == PeerPhase::Active {
            return;
        }
        self.set_phase(PeerPhase::Active, None);
        let wait = self.direct_wait.take();
        self.time_to_direct_ms = wait.map(|wait| self.now_ms.saturating_sub(wait.started_ms));
        tracing::info!(
            target: "terminal",
            worker_fp = %self.worker_fp,
            transport = token.transport.as_str(),
            time_to_direct_ms = self.time_to_direct_ms,
            prewarmed = wait.is_some_and(|wait| wait.prewarmed),
            "direct route promoted"
        );
    }

    /// The open attempt ended on `fault`, and whatever it carried is handed to
    /// the fallback.
    ///
    /// Read BEFORE the caller moves the phase: a peer that was ELECTED puts its
    /// views back on the wait from this instant, because that is when they left
    /// the direct route — and no pre-warm prepared what they wait for next. An
    /// attempt that never was elected leaves the wait where it started, since
    /// its views never stopped waiting.
    pub(crate) fn fell_back(&mut self, fault: CarrierFault) {
        let was_direct = self.phase == PeerPhase::Active;
        if was_direct && self.active_views > 0 {
            self.direct_wait = Some(DirectWait {
                started_ms: self.now_ms,
                prewarmed: false,
            });
        }
        self.time_to_direct_ms = None;
        let fallback = match fault.fallback(self.loopback.has_staged_carrier()) {
            FaultFallback::Loopback => "loopback",
            FaultFallback::Sync => "sync",
        };
        tracing::info!(
            target: "terminal",
            worker_fp = %self.worker_fp,
            reason = fault.as_str(),
            fallback,
            was_direct,
            active_views = self.active_views,
            "direct route fell back"
        );
    }

    /// Tell the grant which sessions to name: every one a view wants, then the
    /// pre-warmed ones while the coordinator's per-grant bound leaves room. A
    /// view's session is never the one left out — a grant the coordinator
    /// refuses for its size would cost the views their direct route too.
    fn replace_granted_demand(&mut self) -> Vec<CarrierEffect> {
        let mut session_ids = self.demand.clone();
        for session_id in &self.prewarm_sessions {
            if session_ids.len() >= TERMINAL_PEER_MAX_SESSIONS_PER_GRANT {
                break;
            }
            session_ids.insert(session_id.clone());
        }
        self.grant.step(GrantInput::DemandReplaced { session_ids })
    }

    /// Move the machine's clock to the host's reading, never backwards: a
    /// deadline judged against an earlier instant than one already seen would
    /// fire late, and a wait measured against one would read short.
    fn advance_clock(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
    }
}
