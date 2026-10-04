//! One attempt's clocks: what is still owed by the browser, and what the
//! protocol says ends an attempt that cannot pay it.
//!
//! Owned by `platform::carriers`, held beside [`super::PeerCarrier`] and asked by
//! `pump::peer_lane`'s tick. Target-independent on purpose: every deadline below
//! is a comparison against the host's own clock, and a native test decides them
//! all without a browser.
//!
//! The core owns no timer, so every window here is the PROTOCOL's own number and
//! not a host-chosen one: ICE gathering and the `Hello` handshake. A host that
//! picked its own would be a second set of limits, and two sets of limits is
//! how a peer that is alive gets retired and a peer that is dead gets waited on.
//! The one host number, [`GATHERING_SETTLE_AFTER_REFLEXIVE_MS`], only reads the
//! offer EARLIER than the protocol's bound, never later.
//! The heartbeat that watches an authenticated peer is `super::heartbeat`.

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_HELLO_DEADLINE_MS, TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS,
};

/// How long gathering continues once the browser has a server-reflexive
/// candidate. One candidate the worker can reach is enough to connect, and a
/// browser whose other interfaces never answer STUN otherwise holds the offer
/// for the protocol's whole gathering bound.
pub const GATHERING_SETTLE_AFTER_REFLEXIVE_MS: u64 = 200;

/// What an attempt's own clock says has run out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDeadline {
    /// ICE gathering ran past the protocol's bound and the offer is read
    /// whatever it holds.
    ///
    /// A deadline and not a fault: the offer's own candidate count is what
    /// decides, so a browser that gathered slowly still gets a carrier while a
    /// browser that gathered nothing is refused by the core's own rule.
    GatheringLapsed,
    /// The worker did not prove its tuple inside the handshake window.
    HelloUnanswered,
}

/// One attempt's clock state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerLife {
    worker_fp: String,
    gathering_due_ms: Option<u64>,
    hello_due_ms: Option<u64>,
    authenticated: bool,
}

impl PeerLife {
    /// The clock for an attempt that has just opened its transport.
    pub fn opened(worker_fp: String, now_ms: u64) -> Self {
        Self {
            worker_fp,
            gathering_due_ms: Some(now_ms.saturating_add(TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS)),
            hello_due_ms: None,
            authenticated: false,
        }
    }

    /// The worker this attempt reaches.
    pub fn worker_fp(&self) -> &str {
        &self.worker_fp
    }

    /// Whether the worker has proved its tuple.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// ICE gathering finished inside its own bound, so the gathering deadline is
    /// discharged and only the core's negotiation deadline remains.
    pub fn gathering_settled(&mut self) {
        self.gathering_due_ms = None;
    }

    /// A server-reflexive candidate arrived, so the offer is read after a short
    /// settle instead of at the protocol's bound. `true` when this call moved
    /// the deadline, which is once per attempt; a settled attempt is untouched.
    pub fn reflexive_candidate_arrived(&mut self, now_ms: u64) -> bool {
        let Some(due_ms) = self.gathering_due_ms else {
            return false;
        };
        let settled_ms = now_ms.saturating_add(GATHERING_SETTLE_AFTER_REFLEXIVE_MS);
        if settled_ms >= due_ms {
            return false;
        }
        self.gathering_due_ms = Some(settled_ms);
        true
    }

    /// Whether the offer is still owed: neither gathering nor its deadline has
    /// produced it. Both can fire for one attempt, and the offer is read once.
    pub fn gathering_pending(&self) -> bool {
        self.gathering_due_ms.is_some()
    }

    /// The control lane opened and the `Hello` went out on it.
    ///
    /// `false` when a `Hello` already went out for this attempt: the handshake is
    /// spent once, and a control lane that reports open twice must not spend the
    /// credential twice.
    pub fn hello_sent(&mut self, now_ms: u64) -> bool {
        if self.hello_due_ms.is_some() {
            return false;
        }
        self.hello_due_ms = Some(now_ms.saturating_add(TERMINAL_PEER_HELLO_DEADLINE_MS));
        true
    }

    /// The worker proved its tuple, which discharges the handshake window.
    pub fn authenticated(&mut self) {
        self.authenticated = true;
        self.hello_due_ms = None;
    }

    /// Every deadline this attempt has run out, in the order they are checked.
    pub fn lapsed(&self, now_ms: u64) -> Vec<PeerDeadline> {
        let mut lapsed = Vec::new();
        if self.gathering_due_ms.is_some_and(|due_ms| now_ms >= due_ms) {
            lapsed.push(PeerDeadline::GatheringLapsed);
        }
        if self.hello_due_ms.is_some_and(|due_ms| now_ms >= due_ms) {
            lapsed.push(PeerDeadline::HelloUnanswered);
        }
        lapsed
    }
}

#[cfg(test)]
mod tests {
    use super::{GATHERING_SETTLE_AFTER_REFLEXIVE_MS, PeerDeadline, PeerLife};

    #[test]
    fn a_reflexive_candidate_reads_the_offer_after_the_settle_not_the_bound() {
        let mut life = PeerLife::opened("worker".to_owned(), 1_000);
        assert!(life.lapsed(1_400).is_empty());
        assert!(life.reflexive_candidate_arrived(1_400));
        let due = 1_400 + GATHERING_SETTLE_AFTER_REFLEXIVE_MS;
        assert!(life.lapsed(due - 1).is_empty());
        assert_eq!(life.lapsed(due), vec![PeerDeadline::GatheringLapsed]);
        // A second candidate never pushes the read later.
        assert!(!life.reflexive_candidate_arrived(1_550));
        assert_eq!(life.lapsed(due), vec![PeerDeadline::GatheringLapsed]);
    }

    #[test]
    fn a_settled_attempt_ignores_a_late_reflexive_candidate() {
        let mut life = PeerLife::opened("worker".to_owned(), 0);
        life.gathering_settled();
        assert!(!life.reflexive_candidate_arrived(100));
        assert!(!life.gathering_pending());
        assert!(life.lapsed(10_000).is_empty());
    }
}
