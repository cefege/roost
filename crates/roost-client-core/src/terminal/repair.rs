//! One latched repair per gap, generation-scoped, sent at most once a heartbeat.
//!
//! A refused delta, an invalid full, a chunk rejection, a stalled partial, and a
//! frame whose session does not match the replica all mean the same thing: the
//! replica's cursor is not trustworthy, and the only thing that repairs it is a
//! complete authoritative full. This file owns that conclusion and nothing else
//! — not the fold (that is `frame_fold`), not the socket (that is `sync`).
//!
//! Ported from `apps/web/src/store/terminal-stream-repair.ts:requestTerminalResync`
//! and `sendLatchedTerminalResync`; the reasons are in
//! `docs/phase4-client-contract.md` §6.5.
//!
//! The escalation past a re-request is deliberately NOT here. v2's proof
//! challenge ladder and the coordinator's two-attempt `requestFreshStream`
//! escalation are authority-side (`docs/FAILURE-INDEX.md:1085` is explicit that
//! the coordinator is the only party that knows which stream it expects). A
//! client-side escalation would give the client an opinion about a stream it did
//! not mint.

use roost_protocol::viewport::TERMINAL_VIEW_HEARTBEAT_MS;

use crate::terminal::token::TerminalToken;

/// The repair latch for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairLatch {
    /// A gap is outstanding. Set once, no matter how many refusals follow.
    latched: bool,
    /// When it was first latched, for the incident record.
    latched_at_ms: u64,
    /// The generation the latch belongs to. A gap on generation A is not sent on
    /// generation B, and a frame accepted on A never repairs B.
    token: Option<TerminalToken>,
    /// The generation key of the last request actually sent, so the heartbeat
    /// rate limit is per generation rather than per session.
    sent_key: Option<String>,
    /// When that request was sent.
    sent_at_ms: u64,
    /// The first refusal reason, for the incident log. Later refusals do not
    /// overwrite it: the first is the one that explains the gap. Owned rather
    /// than `&'static` because a decode failure's diagnosis is built at runtime,
    /// and interning it would mean leaking one string per distinct message.
    reason: Option<String>,
}

impl RepairLatch {
    /// A session with no outstanding gap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a gap is outstanding.
    pub fn is_latched(&self) -> bool {
        self.latched
    }

    /// The generation the outstanding gap belongs to.
    pub fn token(&self) -> Option<&TerminalToken> {
        self.token.as_ref()
    }

    /// When the gap was first observed.
    pub fn latched_at_ms(&self) -> u64 {
        self.latched_at_ms
    }

    /// The first refusal reason recorded for this gap.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Record a refusal. Returns true when this call is the one that latched.
    ///
    /// A second refusal while latched is the SAME gap, not a second one: the
    /// frames that follow a refused delta are the rest of the same gap, and
    /// queueing a request for each of them is how a client turns one lost
    /// baseline into a request storm.
    pub fn latch(&mut self, reason: &str, token: &TerminalToken, now_ms: u64) -> bool {
        if self.latched {
            return false;
        }
        self.latched = true;
        self.latched_at_ms = now_ms;
        self.token = Some(token.clone());
        self.reason = Some(reason.to_string());
        true
    }

    /// Whether a request should go out now, and to which view.
    ///
    /// Three conditions, all of them load-bearing:
    ///
    /// - a gap is outstanding, and
    /// - the latch's generation is STILL the session's current generation, and
    /// - the last request for THIS generation key is at least one heartbeat old.
    ///
    /// The third is keyed on the generation rather than on the session, because a
    /// generation change re-arms it: the new socket has never been asked, and
    /// suppressing its first request because the old socket asked recently is
    /// how a repaired session stays blank.
    pub fn should_send(&self, current: Option<&TerminalToken>, now_ms: u64) -> bool {
        if !self.latched {
            return false;
        }
        let (Some(latched), Some(current)) = (self.token.as_ref(), current) else {
            return false;
        };
        if latched != current {
            return false;
        }
        match &self.sent_key {
            Some(key) if *key == current.key() => {
                now_ms.saturating_sub(self.sent_at_ms) >= TERMINAL_VIEW_HEARTBEAT_MS
            }
            _ => true,
        }
    }

    /// Record that a request went out on `token`.
    pub fn record_sent(&mut self, token: &TerminalToken, now_ms: u64) {
        self.sent_key = Some(token.key());
        self.sent_at_ms = now_ms;
    }

    /// Clear the latch, because a complete authoritative full was accepted.
    ///
    /// ONLY a full clears it. An accepted delta proves the lane, not the gap: the
    /// sequence continued across the hole, so the hole is still there and the
    /// next full is still owed.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::RepairLatch;
    use crate::terminal::token::TerminalToken;

    fn token(generation: u64) -> TerminalToken {
        TerminalToken::sync(generation, "sock", "epoch", 7)
    }

    #[test]
    fn one_gap_latches_once() {
        let mut latch = RepairLatch::new();
        let first = token(1);
        assert!(latch.latch("delta_unfollowed", &first, 1_000));
        assert!(!latch.latch("invalid_full", &first, 1_001));
        assert_eq!(latch.reason(), Some("delta_unfollowed"));
    }

    #[test]
    fn a_generation_change_re_arms_the_first_request() {
        let mut latch = RepairLatch::new();
        let first = token(1);
        latch.latch("delta_unfollowed", &first, 1_000);
        latch.record_sent(&first, 1_000);
        // Same generation, inside the heartbeat: suppressed.
        assert!(!latch.should_send(Some(&first), 1_001));
        // A gap latched on generation 1 is NOT generation 2's gap to ask about —
        // see `a_stale_generation_sends_nothing`.
        assert!(!latch.should_send(Some(&token(2)), 1_001));
        // The re-arm is a NEW latch on the new generation: its `sent_key` names
        // generation 1, so the heartbeat rate limit does not apply to it and the
        // new socket's first request goes out immediately.
        let mut second_generation = RepairLatch::new();
        let second = token(2);
        second_generation.latch("delta_unfollowed", &second, 1_000);
        second_generation.record_sent(&first, 1_000);
        assert!(second_generation.should_send(Some(&second), 1_001));
    }

    #[test]
    fn a_stale_generation_sends_nothing() {
        let mut latch = RepairLatch::new();
        let latched = token(1);
        latch.latch("delta_unfollowed", &latched, 1_000);
        let current = token(2);
        // The latch belongs to generation 1, which is no longer current: the gap
        // is not this socket's gap to ask about.
        assert!(!latch.should_send(Some(&current), 9_999));
        assert!(!latch.should_send(None, 9_999));
    }

    #[test]
    fn only_a_full_clears_the_latch() {
        let mut latch = RepairLatch::new();
        let current = token(1);
        latch.latch("delta_unfollowed", &current, 1_000);
        latch.record_sent(&current, 1_000);
        assert!(latch.is_latched());
        latch.clear();
        assert!(!latch.is_latched());
        assert!(!latch.should_send(Some(&current), 9_999));
    }
}
