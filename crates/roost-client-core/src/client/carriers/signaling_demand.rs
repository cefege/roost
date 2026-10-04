//! What a worker's machine is asked to hold a peer for, and how long that
//! asking waited for a direct route. An `impl Signalling` block beside
//! `super::signaling`, called from its `step` and from the teardown paths in
//! `super::faults`; it owns the one measurement the route diagnostic and the
//! `terminal` log report as `time_to_direct_ms`.

use crate::client::carriers::faults::{CarrierFault, FaultFallback};
use crate::client::carriers::grant::GrantInput;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::{CarrierEffect, PeerPhase};
use crate::terminal::token::TerminalToken;

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
                self.direct_wait_started_ms = None;
            }
            self.demand.remove(&session_id);
            self.grant.step(GrantInput::DemandRemoved { session_id });
            return Vec::new();
        }
        if self.active_views == 0 {
            self.direct_wait_started_ms = Some(self.now_ms);
        }
        self.active_views += 1;
        self.demand.insert(session_id.clone());
        let mut out = self.grant.step(GrantInput::DemandAdded { session_id });
        out.extend(self.start(self.now_ms));
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
        self.time_to_direct_ms = self
            .direct_wait_started_ms
            .take()
            .map(|started_ms| self.now_ms.saturating_sub(started_ms));
        tracing::info!(
            target: "terminal",
            worker_fp = %self.worker_fp,
            transport = token.transport.as_str(),
            time_to_direct_ms = self.time_to_direct_ms,
            "direct route promoted"
        );
    }

    /// The open attempt ended on `fault`, and whatever it carried is handed to
    /// the fallback.
    ///
    /// Read BEFORE the caller moves the phase: a peer that was ELECTED puts its
    /// views back on the wait from this instant, because that is when they left
    /// the direct route. An attempt that never was elected leaves the wait
    /// where it started, since its views never stopped waiting.
    pub(crate) fn fell_back(&mut self, fault: CarrierFault) {
        let was_direct = self.phase == PeerPhase::Active;
        if was_direct && self.active_views > 0 {
            self.direct_wait_started_ms = Some(self.now_ms);
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

    /// Move the machine's clock to the host's reading, never backwards: a
    /// deadline judged against an earlier instant than one already seen would
    /// fire late, and a wait measured against one would read short.
    fn advance_clock(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
    }
}
