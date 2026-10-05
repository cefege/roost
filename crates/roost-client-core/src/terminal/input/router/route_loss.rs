//! A lane whose input route the worker no longer honours: the Sync socket that
//! carried its acknowledged claim closed, or the worker refused a Sync batch
//! because another connection holds the route. Either way every epoch-less or
//! stale batch after it is refused as `terminal input route changed`, so the
//! lane is BLOCKED and Sync claims the route back
//! (`handle_input::reclaim_lost_route`); a refused batch waits for that claim.
//! Owned by `InputRouter`; driven by `handle_sync::lifecycle` and
//! `handle_input`. Ports the route-retirement half of v2
//! `terminal-input-route-claim.ts` (`retireTerminalInputRouteState`).

use crate::terminal::input::InputPhase;
use crate::terminal::token::TerminalTransport;

use super::InputRouter;

impl InputRouter {
    /// The Sync socket of `socket_generation` closed: block every sending lane
    /// whose route epoch the worker acknowledged over it, and forget the epoch.
    ///
    /// The worker heard that claim from a connection that is gone. On the next
    /// socket the epoch fences nothing, and an epoch-less batch is refused for
    /// as long as the worker remembers the old route — which, with nothing to
    /// claim it back, is for good. Returns the sessions blocked.
    pub fn block_routes_on_sync_socket(&mut self, socket_generation: u64) -> Vec<String> {
        let blocked: Vec<String> = self
            .lanes
            .iter()
            .filter(|(_, lane)| {
                lane.phase == InputPhase::Sending
                    && lane.route_epoch_token.as_ref().is_some_and(|token| {
                        token.transport == TerminalTransport::Sync
                            && token.socket_generation == socket_generation
                    })
            })
            .map(|(session_id, _)| session_id.clone())
            .collect();
        for session_id in &blocked {
            self.forget_route(session_id);
        }
        blocked
    }

    /// The worker refused `input_seq`'s Sync batch before writing it because
    /// another connection holds the route. Put the batch back as UNSENT and
    /// block the lane, in that order (`set_phase(Blocked)` refuses unsent
    /// batches, and this one is still marked started while the lane blocks).
    /// Refused when the lane's route is a direct carrier's — then the refusal
    /// is a straggler from before the promotion, which is the fence working.
    /// Returns whether it re-queued.
    pub fn requeue_moved_sync_batch(&mut self, session_id: &str, input_seq: u64) -> bool {
        let Some(lane) = self.lanes.get(session_id) else {
            return false;
        };
        let routed_direct = lane
            .route_epoch_token
            .as_ref()
            .is_some_and(|token| token.transport != TerminalTransport::Sync);
        let refused_on_sync = lane.pending.iter().any(|pending| {
            pending.input_seq == input_seq
                && pending.started
                && pending
                    .fence
                    .as_ref()
                    .is_some_and(|fence| fence.token.transport == TerminalTransport::Sync)
        });
        if routed_direct || !refused_on_sync || lane.phase == InputPhase::Closed {
            return false;
        }
        if lane.phase == InputPhase::Sending {
            self.forget_route(session_id);
        }
        let Some(pending) = self.lanes.get_mut(session_id).and_then(|lane| {
            lane.pending
                .iter_mut()
                .find(|pending| pending.input_seq == input_seq)
        }) else {
            return false;
        };
        pending.started = false;
        pending.fence = None;
        pending.started_at_ms = 0;
        true
    }

    /// The worker epoch the lane's last acknowledged claim named, or empty.
    pub fn acknowledged_worker_epoch(&self, session_id: &str) -> &str {
        self.lane(session_id)
            .map_or("", |lane| lane.claims.worker_epoch.as_str())
    }

    /// Drop the lane's epoch and block it. `set_phase` refuses anything it was
    /// still holding unsent, and a sending lane holds nothing unsent.
    fn forget_route(&mut self, session_id: &str) {
        if let Some(lane) = self.lanes.get_mut(session_id) {
            lane.route_epoch.clear();
            lane.route_epoch_token = None;
        }
        let _ = self.set_phase(session_id, InputPhase::Blocked);
    }
}
