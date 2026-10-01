//! What a fault leaves behind: the sessions it hands to the other transport,
//! the hold-down and the backoff that decide when the worker is tried again,
//! and the machine state only a fault moves. Called by `faults` and by the
//! teardown block there; depends on the taxonomy in the parent and on
//! `CarrierEffect`.

use std::collections::BTreeSet;

use super::{CarrierFault, FallbackReason};
use crate::client::carriers::CarrierEffect;

/// What a fault owes its sessions: the other transport, and nothing else.
/// Nothing here re-opens, re-dials, or re-subscribes anything, and that absence
/// is the point rather than an omission. A fault costs a session its direct route
/// and nothing else (`protocol/spec/direct-terminal.md:29`): the PTY lives in
/// the worker and no client can recreate it, but a client that dropped the
/// session's Sync authority, re-dialled the socket, or re-subscribed a domain
/// would make the worker look like it had lost the session — so "the PTY is not
/// recreated" is observable HERE, as the absence of exactly those effects.
pub fn fallback_effects(
    fault: CarrierFault,
    staged: bool,
    sessions: &BTreeSet<String>,
) -> Vec<CarrierEffect> {
    let transport = fault.fallback(staged);
    let fallback = |session_id: &String| CarrierEffect::Fallback {
        session_id: session_id.clone(),
        transport,
    };
    sessions.iter().map(fallback).collect()
}

// The first retry delay, and the ceiling it grows to. The ceiling is the part
// that is a contract: a worker that keeps failing is still retried, and at a rate
// a fleet-sized browser tab can afford.
const RETRY_BASE_MS: u64 = 1_000;
const RETRY_MAX_MS: u64 = 30_000;

/// How long a peer's failure holds its worker before a new peer is allocated. v2
/// holds a worker down for 30 s once its peer has been ACTIVE
/// (`terminal-peer.ts:24,275`): a peer that dropped after serving sessions is far
/// likelier to drop again than one that never came up.
pub const PEER_HOLD_DOWN_MS: u64 = 30_000;

/// Bounded exponential backoff for the peer attempt loop: 1s, 2s, 4s, capped at
/// 30s — the shape v2 runs through `backoffDelayMs(failureCount - 1,
/// { baseMs: 1_000, maxMs: 30_000 })` (`terminal-peer.ts:277`), spelled out
/// because that helper has no v3 owner.
pub fn retry_delay_ms(attempt: u32) -> u64 {
    RETRY_BASE_MS
        .saturating_mul(1u64 << attempt.min(16))
        .min(RETRY_MAX_MS)
}

/// The parts of a machine's state that only a FAULT moves. Split out because
/// they are the same for every fault, and inline they would repeat the hold-down
/// and the backoff per call site — which is how two drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaultState {
    pub failure_count: u32,
    /// Until when a worker whose peer was ELECTED is held down.
    pub hold_down_until_ms: u64,
    pub retry_at_ms: u64,
    /// The coarse recorded reason, cleared on every successful transition.
    pub reason: Option<FallbackReason>,
    /// The host's own last failure detail, never a value that failed to match.
    pub last_detail: Option<String>,
}

impl FaultState {
    /// A promotion committed, so the failure run is over.
    pub fn cleared(&mut self) {
        self.failure_count = 0;
        self.last_detail = None;
    }

    /// The worker had an ELECTED peer, so hold it down before allocating another.
    pub fn hold_down_from_active(&mut self, now_ms: u64) {
        self.hold_down_until_ms = now_ms.saturating_add(PEER_HOLD_DOWN_MS);
    }

    /// Arm the retry. The core owns no timer, so this reports the INSTANT, not a
    /// delay: a host that cannot schedule one leaves the session on Sync.
    pub fn schedule(&mut self, at_ms: u64) -> Vec<CarrierEffect> {
        self.retry_at_ms = at_ms;
        vec![CarrierEffect::RetryAt { at_ms }]
    }

    /// Report a fault: every session it carried falls back to the other
    /// transport, the host logs which rule fired, and the retry is armed. The
    /// attempt's closure and the grant's revocation happen BEFORE this and are
    /// the caller's, because both need the machine's own state — which is why
    /// this half, the same for every fault, is the one a new member cannot skip.
    pub fn report(
        &mut self,
        fault: CarrierFault,
        worker_fp: &str,
        detail: &str,
        now_ms: u64,
        staged: bool,
        sessions: &BTreeSet<String>,
    ) -> Vec<CarrierEffect> {
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_detail = Some(detail.to_string());
        let mut out = fallback_effects(fault, staged, sessions);
        out.push(CarrierEffect::Faulted {
            worker_fp: worker_fp.to_string(),
            fault,
            detail: detail.to_string(),
        });
        if !fault.retryable() {
            return out;
        }
        let backoff = now_ms.saturating_add(retry_delay_ms(self.failure_count - 1));
        let at_ms = self.hold_down_until_ms.max(backoff);
        out.extend(self.schedule(at_ms));
        out
    }
}
