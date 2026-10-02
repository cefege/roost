//! The input route claim: the epoch a worker writes a session's input under,
//! the hold that keeps fresh input unsent while it is asked for, and the drain
//! that keeps an old route's batch from crossing the new one.
//!
//! Owned by `InputRouter`; driven by `handle_sync::promotion`, which decides
//! WHEN to claim and what a settled claim promotes or releases. Ported from
//! `apps/web/src/store/transport/terminal-input-route-claim.ts` and the
//! hold/drain half of `terminal-input-router.ts`.

use crate::sync::inbound::InputRouteResult;
use crate::terminal::input::{
    InputOutcome, InputPhase, MAX_TERMINAL_INPUT_ROUTE_REVISION, PendingInput,
};
use crate::terminal::token::TerminalToken;

use super::InputRouter;

/// How long a claim waits for its answer (v2 `awaitRouteClaim`).
pub const ROUTE_CLAIM_TIMEOUT_MS: u64 = 8_000;

/// How long a promotion waits for the old route's input to settle (v2
/// `INPUT_HANDOFF_DRAIN_MS`).
pub const INPUT_HANDOFF_DRAIN_MS: u64 = 10_000;

/// How many times a Sync fallback claim is attempted, and how far apart (v2
/// `FALLBACK_CLAIM_ATTEMPTS`, `FALLBACK_CLAIM_RETRY_MS`).
pub const FALLBACK_CLAIM_ATTEMPTS: u32 = 120;
/// The gap between two fallback attempts.
pub const FALLBACK_CLAIM_RETRY_MS: u64 = 250;

/// One claim this document sent and has not had answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteClaim {
    /// The id the answer is matched on.
    pub request_id: String,
    /// The session.
    pub session_id: String,
    /// The revision claimed; a worker refuses one at or below its latest.
    pub revision: u64,
    /// The route the claim went out on, and the one its epoch is good for.
    pub token: TerminalToken,
    /// The worker process the claim names.
    pub worker_epoch: String,
    /// The staged attempt the claim gates, or `None` for a Sync fallback.
    pub attempt_id: Option<u64>,
    /// When it was sent, for the answer deadline.
    pub sent_at_ms: u64,
    /// A `stale_route_revision` answer is retried once, above the latest.
    pub retried: bool,
}

/// A promotion holding a session's input while it drains and claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionHold {
    /// The staged attempt being promoted.
    pub attempt_id: u64,
    /// The candidate's route.
    pub token: TerminalToken,
    /// When the hold began, for the drain deadline.
    pub held_at_ms: u64,
    /// A claim went out, so the worker may already have moved the route and
    /// abandoning must reclaim it on Sync rather than just release the hold.
    pub claim_sent: bool,
}

/// A Sync fallback reclaiming a session's input after its direct route went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackRecovery {
    /// The worker the session lives on, for the grant's epoch.
    pub worker_fp: String,
    /// The epoch to claim with when no live grant names a newer one.
    pub worker_epoch: String,
    /// Attempts made so far.
    pub attempts: u32,
    /// When the next attempt is due.
    pub next_at_ms: u64,
}

/// One session's claim state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteClaimState {
    /// A claim has been made for this session in this document, so a batch
    /// without an acknowledged epoch is never written on a fresh route.
    pub required: bool,
    /// The claim awaiting its answer.
    pub in_flight: Option<RouteClaim>,
    /// The promotion holding the lane, if one is.
    pub promotion: Option<PromotionHold>,
    /// The Sync recovery holding the lane, if one is.
    pub fallback: Option<FallbackRecovery>,
}

/// What one answer did to its claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimSettlement {
    /// The epoch is installed for the claim's route.
    Accepted(RouteClaim),
    /// Refused as stale; the claim is re-issued above the worker's latest.
    Retry(RouteClaim),
    /// Refused for good, with the reason.
    Refused {
        /// The claim.
        claim: RouteClaim,
        /// Why.
        reason: String,
    },
}

impl InputRouter {
    /// Whether fresh input on this session is held unsent.
    pub fn is_holding(&self, session_id: &str) -> bool {
        self.lane(session_id)
            .is_some_and(|lane| matches!(lane.phase, InputPhase::Holding | InputPhase::Claiming))
    }

    /// The claim state of one session.
    pub fn claim_state(&self, session_id: &str) -> Option<&RouteClaimState> {
        self.lane(session_id).map(|lane| &lane.claims)
    }

    /// Hold the lane for promoting `attempt_id` onto `token` (v2
    /// `holdTerminalInput` at the head of `promote`). A promotion supersedes a
    /// fallback still reclaiming Sync, as v2 retires it on commit.
    pub fn hold_for_promotion(
        &mut self,
        session_id: &str,
        attempt_id: u64,
        token: &TerminalToken,
        now_ms: u64,
    ) {
        let lane = self.lane_mut(session_id);
        if lane.phase == InputPhase::Closed {
            return;
        }
        lane.phase = InputPhase::Holding;
        lane.claims.fallback = None;
        lane.claims.in_flight = None;
        lane.claims.promotion = Some(PromotionHold {
            attempt_id,
            token: token.clone(),
            held_at_ms: now_ms,
            claim_sent: false,
        });
    }

    /// Hold the lane until Sync is claimed back (v2 `TerminalPeerFallbackClaims`).
    pub fn begin_fallback(
        &mut self,
        session_id: &str,
        worker_fp: &str,
        worker_epoch: &str,
        now_ms: u64,
    ) {
        let lane = self.lane_mut(session_id);
        if lane.phase == InputPhase::Closed {
            return;
        }
        lane.phase = InputPhase::Holding;
        lane.claims.promotion = None;
        lane.claims.in_flight = None;
        lane.claims.fallback = Some(FallbackRecovery {
            worker_fp: worker_fp.to_owned(),
            worker_epoch: worker_epoch.to_owned(),
            attempts: 0,
            next_at_ms: now_ms,
        });
    }

    /// Whether a batch already handed to `token` is still unanswered: the
    /// drain a promotion waits out before it claims (v2 `drain`).
    pub fn has_unsettled_on(&self, session_id: &str, token: &TerminalToken) -> bool {
        self.lane(session_id).is_some_and(|lane| {
            lane.pending.iter().any(|pending| {
                pending.started
                    && pending
                        .fence
                        .as_ref()
                        .is_some_and(|fence| &fence.token == token)
            })
        })
    }

    /// Mint and record one claim on `token`. Refused only when the revision
    /// space is spent, which blocks the lane as v2 does.
    pub fn begin_route_claim(
        &mut self,
        session_id: &str,
        token: &TerminalToken,
        worker_epoch: &str,
        attempt_id: Option<u64>,
        now_ms: u64,
    ) -> Result<RouteClaim, &'static str> {
        self.next_claim_id += 1;
        // Unique per document; the worker matches an answer on the id, the
        // revision AND the connection, so a reload's restarted count never
        // collides with this one's.
        let request_id = format!("route-claim-{}", self.next_claim_id);
        let lane = self.lane_mut(session_id);
        lane.claims.required = true;
        lane.route_epoch.clear();
        lane.route_epoch_token = None;
        if lane.route_revision >= MAX_TERMINAL_INPUT_ROUTE_REVISION {
            lane.phase = InputPhase::Blocked;
            return Err("terminal input route revision exhausted");
        }
        lane.route_revision += 1;
        lane.phase = InputPhase::Claiming;
        let claim = RouteClaim {
            request_id,
            session_id: session_id.to_owned(),
            revision: lane.route_revision,
            token: token.clone(),
            worker_epoch: worker_epoch.to_owned(),
            attempt_id,
            sent_at_ms: now_ms,
            retried: false,
        };
        if let Some(promotion) = &mut lane.claims.promotion {
            promotion.claim_sent = true;
        }
        lane.claims.in_flight = Some(claim.clone());
        Ok(claim)
    }

    /// Apply one answer that arrived on `token`. `None` when it answers no
    /// claim in flight, which is a superseded claim's late reply.
    pub fn settle_route_claim(
        &mut self,
        token: &TerminalToken,
        result: &InputRouteResult,
        now_ms: u64,
    ) -> Option<ClaimSettlement> {
        let lane = self.lanes.get_mut(&result.session_id)?;
        let matches =
            lane.claims.in_flight.as_ref().is_some_and(|claim| {
                claim.request_id == result.request_id && &claim.token == token
            });
        if !matches {
            return None;
        }
        let claim = lane.claims.in_flight.take()?;
        if result.revision != claim.revision || result.worker_epoch != claim.worker_epoch {
            lane.phase = InputPhase::Holding;
            let reason = "terminal input route claim response was invalid".to_owned();
            return Some(ClaimSettlement::Refused { claim, reason });
        }
        if result.accepted && !result.input_route_epoch.is_empty() {
            lane.route_epoch = result.input_route_epoch.clone();
            lane.route_epoch_token = Some(claim.token.clone());
            lane.phase = InputPhase::Holding;
            return Some(ClaimSettlement::Accepted(claim));
        }
        let stale = result.reason == "stale_route_revision" && !claim.retried;
        if !stale
            || result.latest_revision < claim.revision
            || result.latest_revision >= MAX_TERMINAL_INPUT_ROUTE_REVISION
        {
            lane.phase = InputPhase::Holding;
            let reason = if !stale {
                non_empty_or(&result.reason, "terminal input route claim was rejected")
            } else {
                "terminal input route revision is unavailable".to_owned()
            };
            return Some(ClaimSettlement::Refused { claim, reason });
        }
        lane.route_revision = result.latest_revision + 1;
        self.next_claim_id += 1;
        let retry = RouteClaim {
            request_id: format!("route-claim-{}", self.next_claim_id),
            revision: lane.route_revision,
            sent_at_ms: now_ms,
            retried: true,
            ..claim
        };
        lane.claims.in_flight = Some(retry.clone());
        Some(ClaimSettlement::Retry(retry))
    }

    /// Take every claim whose answer is overdue (v2: "was not confirmed").
    pub fn expire_route_claims(&mut self, now_ms: u64) -> Vec<RouteClaim> {
        let mut expired = Vec::new();
        for lane in self.lanes.values_mut() {
            let overdue = lane.claims.in_flight.as_ref().is_some_and(|claim| {
                now_ms.saturating_sub(claim.sent_at_ms) >= ROUTE_CLAIM_TIMEOUT_MS
            });
            if overdue && let Some(claim) = lane.claims.in_flight.take() {
                lane.phase = InputPhase::Holding;
                expired.push(claim);
            }
        }
        expired
    }

    /// End whatever holds the lane, and hand back the batches it held for the
    /// caller to send on the route now in charge.
    pub fn release_held(&mut self, session_id: &str) -> Vec<PendingInput> {
        let Some(lane) = self.lanes.get_mut(session_id) else {
            return Vec::new();
        };
        if lane.phase == InputPhase::Closed {
            return Vec::new();
        }
        lane.phase = InputPhase::Sending;
        lane.claims.promotion = None;
        lane.claims.fallback = None;
        lane.claims.in_flight = None;
        lane.pending
            .iter()
            .filter(|pending| !pending.started)
            .cloned()
            .collect()
    }

    /// Give up on the route: drop every claim and refuse what is held (v2
    /// `hold.release()` with no destination). A closed lane stays closed.
    pub fn block_route(&mut self, session_id: &str) -> Vec<InputOutcome> {
        let Some(lane) = self.lanes.get_mut(session_id) else {
            return Vec::new();
        };
        if lane.phase == InputPhase::Closed {
            return Vec::new();
        }
        lane.claims.promotion = None;
        lane.claims.fallback = None;
        lane.claims.in_flight = None;
        self.set_phase(session_id, InputPhase::Blocked)
    }

    /// Schedule the next fallback attempt; `false` once they are spent, which
    /// blocks the lane and refuses what it held (v2 `hold.release()`).
    pub fn defer_fallback(&mut self, session_id: &str, now_ms: u64) -> bool {
        let Some(lane) = self.lanes.get_mut(session_id) else {
            return false;
        };
        let Some(fallback) = &mut lane.claims.fallback else {
            return false;
        };
        fallback.attempts += 1;
        fallback.next_at_ms = now_ms.saturating_add(FALLBACK_CLAIM_RETRY_MS);
        if fallback.attempts < FALLBACK_CLAIM_ATTEMPTS {
            return true;
        }
        lane.claims.fallback = None;
        false
    }

    /// Every session with a fallback due now.
    pub fn due_fallbacks(&self, now_ms: u64) -> Vec<(String, FallbackRecovery)> {
        self.lanes
            .iter()
            .filter_map(|(session_id, lane)| {
                let fallback = lane.claims.fallback.as_ref()?;
                (lane.claims.in_flight.is_none() && fallback.next_at_ms <= now_ms)
                    .then(|| (session_id.clone(), fallback.clone()))
            })
            .collect()
    }

    /// Every session a promotion is holding.
    pub fn held_promotions(&self) -> Vec<(String, PromotionHold)> {
        self.lanes
            .iter()
            .filter_map(|(session_id, lane)| {
                lane.claims
                    .promotion
                    .clone()
                    .map(|promotion| (session_id.clone(), promotion))
            })
            .collect()
    }
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}
