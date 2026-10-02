//! The staged attempt: what a candidate is preparing, when it may be promoted,
//! and what has to be released when it is not.
//!
//! Split from the registration rules because the two are read at different
//! moments and fail differently. Registering asks "may this socket exist";
//! staging asks "has this socket earned the route", and the answer arrives in
//! frames, view-state results and host-minted ids rather than in a handshake.
//!
//! Contract: `protocol/spec/direct-terminal.md`; the reasons are in
//! `docs/phase4-client-contract.md` §8.

use super::RouteRegistry;
use crate::terminal::routes::{
    CancelledCandidate, DirectCarrier, PromotionCandidate, PromotionRefusal, SessionRoute,
};
use crate::terminal::session::TerminalSession;
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// What registering one connection did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConnectionRegistration {
    /// Whether the connection was admitted into one of the worker's slots.
    pub accepted: bool,
    /// Staged attempts cancelled because this registration displaced the
    /// connection they were folding on. Each carries the wire ids that
    /// connection's worker is still holding for this document.
    pub cancelled: Vec<CancelledCandidate>,
}

impl ConnectionRegistration {
    /// Admitted, displacing nothing.
    pub fn admitted() -> Self {
        Self {
            accepted: true,
            cancelled: Vec::new(),
        }
    }

    /// Refused, and the host should close the socket.
    pub fn refused() -> Self {
        Self {
            accepted: false,
            cancelled: Vec::new(),
        }
    }
}

impl RouteRegistry {
    /// Record the id the candidate published for one pane.
    pub fn mark_view_wire_id(
        &mut self,
        session_id: &str,
        logical_view_id: &str,
        wire_view_id: String,
    ) -> bool {
        match self
            .candidates
            .get_mut(session_id)
            .and_then(|candidate| candidate.prospective_views.get_mut(logical_view_id))
        {
            Some(prospective) => {
                prospective.wire_view_id = Some(wire_view_id);
                true
            }
            None => false,
        }
    }

    /// Any registered connection whose grant still admits this session.
    ///
    /// Active before candidate, so a restage prefers the carrier already serving
    /// routes — otherwise a document with a working loopback and a peer still
    /// negotiating would restage onto the peer and hand it the session.
    pub fn admitted_carrier(&self, session_id: &str) -> Option<DirectCarrier> {
        self.connections
            .values()
            .filter_map(|slots| {
                slots
                    .active
                    .iter()
                    .chain(slots.candidate.iter())
                    .find(|carrier| carrier.allows_session(session_id))
                    .cloned()
            })
            .next()
    }
    /// Record a staged candidate and the replica it is folding into. Returns
    /// false when a newer attempt has already been staged, so a slow fold cannot
    /// replace it.
    pub fn stage(&mut self, candidate: PromotionCandidate, replica: TerminalSession) -> bool {
        if self
            .candidates
            .get(&candidate.session_id)
            .is_some_and(|existing| existing.attempt_id > candidate.attempt_id)
        {
            return false;
        }
        let session_id = candidate.session_id.clone();
        self.staged.insert(session_id.clone(), replica);
        self.candidates.insert(session_id, candidate);
        true
    }

    /// The replica a staged candidate is folding into, if one is staged.
    pub fn staged_replica_mut(&mut self, session_id: &str) -> Option<&mut TerminalSession> {
        self.staged.get_mut(session_id)
    }

    /// Record whether a staged candidate now holds a complete validated baseline.
    ///
    /// Metadata only, so the caller can update it after a fold without moving the
    /// replica — which is what `stage` would do, and a replica is not movable
    /// without giving up the borrow the fold is still inside.
    pub fn mark_candidate_baseline(&mut self, session_id: &str, baseline_ready: bool) {
        if let Some(candidate) = self.candidates.get_mut(session_id) {
            candidate.baseline_ready = baseline_ready;
        }
    }

    /// Record that the authority acknowledged one of the candidate's own views.
    pub fn mark_view_acknowledged(&mut self, session_id: &str, logical_view_id: &str) {
        if let Some(candidate) = self.candidates.get_mut(session_id)
            && let Some(prospective) = candidate.prospective_views.get_mut(logical_view_id)
        {
            prospective.acknowledged = true;
        }
    }

    /// Whether a staged candidate is still something its connection could
    /// promote: registered, presenting the prepared token, and still admitted.
    ///
    /// The mint answer is the one place outside `promote` that acts on a
    /// candidate, so it needs the same connection fences without claiming the
    /// route.
    pub fn candidate_is_admissible(&self, session_id: &str) -> bool {
        let Some(candidate) = self.candidates.get(session_id) else {
            return false;
        };
        let Some(worker_fp) = candidate.token.worker_fp.as_deref() else {
            return false;
        };
        let Some(slots) = self.connections.get(worker_fp) else {
            return false;
        };
        slots
            .candidate
            .iter()
            .chain(slots.active.iter())
            .any(|carrier| {
                carrier.connection_id == candidate.connection_id
                    && carrier.presents(&candidate.token)
                    && carrier.allows_session(session_id)
            })
    }

    /// Whether a staged candidate may be promoted now, without promoting it.
    ///
    /// Refused unless ALL of the following hold, and every one of them is a way
    /// a promotion has gone wrong:
    ///
    /// - the attempt is still the current one, so a slow fold cannot win;
    /// - the candidate has a COMPLETE validated baseline, which is the whole
    ///   reason it was folded separately;
    /// - every view the candidate published has been acknowledged, so no pane is
    ///   left with a wire id the authority never confirmed;
    /// - the connection still presents exactly the prepared token;
    /// - the connection is still in one of its worker's slots;
    /// - the grant still admits this session;
    /// - a live view still wants this session on that worker;
    /// - if another connection already holds the active slot, this one is
    ///   loopback — the preference order made enforceable rather than hoped for.
    ///
    /// Asked before the input route is claimed as well as at the commit: a
    /// candidate that could never be promoted must not move the worker's route.
    pub fn promotable(
        &self,
        session_id: &str,
        attempt_id: u64,
        token: &TerminalToken,
    ) -> Result<String, PromotionRefusal> {
        let candidate = self
            .candidates
            .get(session_id)
            .ok_or(PromotionRefusal::NoCandidate)?;
        if candidate.attempt_id != attempt_id {
            return Err(PromotionRefusal::AttemptMovedOn);
        }
        if !candidate.baseline_ready {
            return Err(PromotionRefusal::BaselineIncomplete);
        }
        if candidate
            .prospective_views
            .values()
            .any(|view| !view.acknowledged)
        {
            return Err(PromotionRefusal::ViewUnacknowledged);
        }
        if &candidate.token != token {
            return Err(PromotionRefusal::TokenChanged);
        }
        let Some(worker_fp) = token.worker_fp.as_deref() else {
            return Err(PromotionRefusal::TokenChanged);
        };
        let Some(slots) = self.connections.get(worker_fp) else {
            return Err(PromotionRefusal::ConnectionGone);
        };
        let Some(carrier) = slots.candidate.as_ref().or(slots.active.as_ref()) else {
            return Err(PromotionRefusal::ConnectionGone);
        };
        if carrier.connection_id != candidate.connection_id || !carrier.presents(token) {
            return Err(PromotionRefusal::TokenChanged);
        }
        if !carrier.allows_session(session_id) {
            return Err(PromotionRefusal::GrantDoesNotAdmit);
        }
        if !self.has_view_demand(worker_fp, session_id) {
            return Err(PromotionRefusal::NoViewDemand);
        }
        let displaces_active = slots
            .active
            .as_ref()
            .is_some_and(|active| active.connection_id != carrier.connection_id);
        if displaces_active && carrier.transport != TerminalTransport::Loopback {
            return Err(PromotionRefusal::LoopbackPreferred);
        }
        Ok(carrier.connection_id.clone())
    }

    /// Promote a staged candidate to the session's route, and hand back the
    /// replica that becomes canonical. Refused for every reason `promotable`
    /// names.
    pub fn promote(
        &mut self,
        session_id: &str,
        attempt_id: u64,
        token: &TerminalToken,
    ) -> Result<TerminalSession, PromotionRefusal> {
        let connection_id = self.promotable(session_id, attempt_id, token)?;
        let Some(worker_fp) = token.worker_fp.clone() else {
            return Err(PromotionRefusal::TokenChanged);
        };
        self.candidates.remove(session_id);
        let promoted = self
            .staged
            .remove(session_id)
            .ok_or(PromotionRefusal::NoCandidate)?;
        self.routes.insert(
            session_id.to_string(),
            SessionRoute {
                connection_id,
                token: token.clone(),
            },
        );
        self.promote_candidate_if_possible(&worker_fp);
        Ok(promoted)
    }

    /// Abandon one session's staged attempt, and report the wire ids the worker
    /// is still holding for it.
    ///
    /// The caller releases those ids on the returned token and NOTHING else: a
    /// candidate that never minted has nothing to release, and one that minted
    /// two has two ids the authority is holding leases against.
    pub fn cancel_candidate(&mut self, session_id: &str) -> Option<CancelledCandidate> {
        let candidate = self.candidates.remove(session_id)?;
        self.staged.remove(session_id);
        let minted = candidate
            .prospective_views
            .values()
            .filter_map(|view| {
                view.wire_view_id
                    .clone()
                    .map(|wire_view_id| (wire_view_id, view.candidate_revision))
            })
            .collect();
        Some(CancelledCandidate {
            session_id: candidate.session_id,
            token: candidate.token,
            minted,
        })
    }

    /// Abandon every attempt staged on one connection.
    pub(crate) fn cancel_candidates_for_connection(
        &mut self,
        connection_id: &str,
    ) -> Vec<CancelledCandidate> {
        let sessions: Vec<String> = self
            .candidates
            .iter()
            .filter(|(_, candidate)| candidate.connection_id == connection_id)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        sessions
            .iter()
            .filter_map(|session_id| self.cancel_candidate(session_id))
            .collect()
    }

    /// Abandon every attempt staged on one worker.
    pub fn cancel_candidates_for_worker(&mut self, worker_fp: &str) -> Vec<CancelledCandidate> {
        let sessions: Vec<String> = self
            .candidates
            .iter()
            .filter(|(_, candidate)| candidate.token.worker_fp.as_deref() == Some(worker_fp))
            .map(|(session_id, _)| session_id.clone())
            .collect();
        sessions
            .iter()
            .filter_map(|session_id| self.cancel_candidate(session_id))
            .collect()
    }

    /// Every staged attempt still short of a promotable baseline, with the
    /// instant it began, for the baseline deadline sweep.
    ///
    /// One that has its baseline and every view answered is past that deadline's
    /// question: it is draining and claiming the input route, which has its own
    /// deadlines, and cancelling it there would strand the claim mid-flight.
    pub fn attempts_awaiting_baseline(&self) -> Vec<(String, u64, u64)> {
        self.candidates
            .iter()
            .filter(|(_, candidate)| {
                !candidate.baseline_ready
                    || candidate
                        .prospective_views
                        .values()
                        .any(|view| !view.acknowledged)
            })
            .map(|(session_id, candidate)| {
                (
                    session_id.clone(),
                    candidate.attempt_id,
                    candidate.staged_at_ms,
                )
            })
            .collect()
    }

    /// The token a session's staged attempt is folding on, if one is staged.
    pub fn staged_token(&self, session_id: &str) -> Option<TerminalToken> {
        self.candidates
            .get(session_id)
            .map(|candidate| candidate.token.clone())
    }
}
