//! The content-free transport probes one direct carrier has sent, and what
//! their answers prove: which requests are still owed, how long the last one
//! took, and whether the carrier answered recently enough to count as live.
//!
//! Owned by `client::carriers`, held by a host's carrier and driven by that
//! host's tick. Holds no timer: every deadline is a comparison against the clock
//! the caller passes. Ports v2 `client/carriers/terminal-peer-probe-state.ts`.

use std::collections::BTreeMap;

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_PROBE_DEADLINE_MS, TERMINAL_PEER_PROBE_QUALIFICATION_MS,
};

use crate::sync::inbound::TransportProbeResult;

/// What a carrier's answered probes measured, for its telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProbeReading {
    /// When the newest probe was answered, on the caller's clock.
    pub last_probe_at_ms: Option<u64>,
    /// The newest answered probe's round trip.
    pub rtt_ms: Option<u64>,
    /// Whether the carrier answered its newest probe and did so inside the
    /// qualification window: a carrier that stopped answering is not live.
    pub liveness_qualified: bool,
}

/// One carrier's outstanding probes and the proof its answers earned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportProbeState {
    worker_fp: String,
    worker_epoch: String,
    /// Request id → when it went out.
    pending: BTreeMap<String, u64>,
    last_probe_at_ms: Option<u64>,
    rtt_ms: Option<u64>,
    qualified: bool,
}

impl TransportProbeState {
    /// The state for a carrier whose worker proved this fingerprint and epoch.
    pub fn new(worker_fp: impl Into<String>, worker_epoch: impl Into<String>) -> Self {
        Self {
            worker_fp: worker_fp.into(),
            worker_epoch: worker_epoch.into(),
            pending: BTreeMap::new(),
            last_probe_at_ms: None,
            rtt_ms: None,
            qualified: false,
        }
    }

    /// Record a probe about to go out. `false` refuses an empty or reused id,
    /// because an answer could not say which of two probes it settles.
    pub fn start(&mut self, request_id: &str, now_ms: u64) -> bool {
        if request_id.is_empty() || self.pending.contains_key(request_id) {
            return false;
        }
        self.pending.insert(request_id.to_owned(), now_ms);
        true
    }

    /// The carrier did not take the probe's bytes: the probe failed now, and
    /// whatever liveness the carrier had earned is no longer proof.
    pub fn refuse(&mut self, request_id: &str) {
        if self.pending.remove(request_id).is_some() {
            self.qualified = false;
        }
    }

    /// Whether `result` comes from this carrier's worker process — the
    /// fingerprint and epoch it proved — whatever probe it names.
    pub fn answers_this_worker(&self, result: &TransportProbeResult) -> bool {
        result.worker_fp == self.worker_fp && result.worker_epoch == self.worker_epoch
    }

    /// Settle one answer. `false` when it answers no probe of this carrier's —
    /// another worker, another process epoch, or an id nothing is waiting on.
    pub fn resolve(&mut self, result: &TransportProbeResult, now_ms: u64) -> bool {
        if !self.answers_this_worker(result) {
            return false;
        }
        let Some(started_ms) = self.pending.remove(&result.request_id) else {
            return false;
        };
        self.last_probe_at_ms = Some(now_ms);
        self.rtt_ms = Some(now_ms.saturating_sub(started_ms));
        self.qualified = true;
        true
    }

    /// Every probe whose deadline has passed, removed. Any lapse withdraws the
    /// carrier's qualification, as v2's timeout does.
    pub fn expire(&mut self, now_ms: u64) -> Vec<String> {
        let lapsed: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, started_ms)| {
                now_ms.saturating_sub(**started_ms) >= TERMINAL_PEER_PROBE_DEADLINE_MS
            })
            .map(|(request_id, _)| request_id.clone())
            .collect();
        for request_id in &lapsed {
            self.pending.remove(request_id);
        }
        if !lapsed.is_empty() {
            self.qualified = false;
        }
        lapsed
    }

    /// Start a new proof episode: nothing earned before counts, and nothing
    /// outstanding is waited on.
    pub fn require_fresh(&mut self) {
        self.qualified = false;
        self.pending.clear();
    }

    /// What the answers measured, read against `now_ms`.
    pub fn reading(&self, now_ms: u64) -> ProbeReading {
        ProbeReading {
            last_probe_at_ms: self.last_probe_at_ms,
            rtt_ms: self.rtt_ms,
            liveness_qualified: self.qualified
                && self.last_probe_at_ms.is_some_and(|answered_ms| {
                    now_ms.saturating_sub(answered_ms) <= TERMINAL_PEER_PROBE_QUALIFICATION_MS
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(request_id: &str, worker_epoch: &str) -> TransportProbeResult {
        TransportProbeResult {
            request_id: request_id.to_owned(),
            worker_fp: "worker-a".to_owned(),
            worker_epoch: worker_epoch.to_owned(),
        }
    }

    #[test]
    fn an_answer_qualifies_the_carrier_until_the_window_passes() {
        let mut probes = TransportProbeState::new("worker-a", "epoch-1");
        assert!(probes.start("probe-1", 1_000));
        assert!(probes.resolve(&answer("probe-1", "epoch-1"), 1_040));
        let reading = probes.reading(1_040);
        assert_eq!(reading.rtt_ms, Some(40));
        assert!(reading.liveness_qualified);
        assert!(
            !probes
                .reading(1_040 + TERMINAL_PEER_PROBE_QUALIFICATION_MS + 1)
                .liveness_qualified,
            "an answer older than the window is no longer proof"
        );
    }

    #[test]
    fn another_process_epoch_answers_nothing() {
        let mut probes = TransportProbeState::new("worker-a", "epoch-1");
        assert!(probes.start("probe-1", 0));
        assert!(!probes.resolve(&answer("probe-1", "epoch-2"), 10));
        assert!(!probes.reading(10).liveness_qualified);
    }

    #[test]
    fn a_lapsed_probe_withdraws_qualification() {
        let mut probes = TransportProbeState::new("worker-a", "epoch-1");
        assert!(probes.start("probe-1", 0));
        assert!(probes.resolve(&answer("probe-1", "epoch-1"), 10));
        assert!(probes.start("probe-2", 100));
        assert!(
            probes
                .expire(100 + TERMINAL_PEER_PROBE_DEADLINE_MS - 1)
                .is_empty()
        );
        assert_eq!(
            probes.expire(100 + TERMINAL_PEER_PROBE_DEADLINE_MS),
            vec!["probe-2".to_owned()]
        );
        assert!(!probes.reading(200).liveness_qualified);
        assert!(
            !probes.resolve(&answer("probe-2", "epoch-1"), 4_000),
            "a lapsed probe's late answer settles nothing"
        );
    }
}
