//! One authenticated peer's heartbeat: the content-free transport probe it
//! sends on the control lane while its route is live, the misses that end it,
//! and the telemetry its answers earn.
//!
//! Owned by `platform::carriers`, held beside [`super::PeerCarrier`]'s lanes and
//! driven by `pump::peer_lane`'s tick. Target-independent: every decision is a
//! comparison against the clock the tick passes. Ports v2 `TerminalPeerOwner`'s
//! `heartbeat` (`store/transport/terminal-peer.ts`): one probe at a time, the
//! next one interval after the last settled. Unlike v2, the peer closes on its
//! THIRD consecutive miss, and an answer that arrives after its probe lapsed
//! still forgives the misses: a Wi-Fi link that holds packets for seconds and
//! then delivers them is slow, not dead, and tearing the peer down there costs
//! a re-negotiation and a re-baseline. A browser's `getStats` cannot stand in
//! for it: a selected candidate pair outlives the worker process behind it by
//! the ICE consent window, so a restarted worker's peer kept reading as live.

use roost_client_core::client::carriers::{CandidateType, ProbeReading, TransportProbeState};
use roost_client_core::sync::inbound::TransportProbeResult;
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_HEARTBEAT_INTERVAL_MS, TERMINAL_PEER_PROBE_DEADLINE_MS,
};

/// Consecutive missed heartbeats that close the peer. Two are tolerated: with
/// the probe deadline and interval that is roughly nineteen seconds in which
/// the worker answered nothing at all, not even late.
pub const HEARTBEAT_MISS_LIMIT: u32 = 3;

/// What one lapsed heartbeat probe means for the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatMiss {
    /// Missed, and tolerated.
    Tolerated,
    /// The limit is reached and the peer must close.
    Exhausted,
}

/// One peer's heartbeat state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerHeartbeat {
    probes: TransportProbeState,
    running: bool,
    next_due_ms: Option<u64>,
    in_flight: Option<String>,
    /// Heartbeat probes that lapsed since the last answer; a late answer to
    /// one of them still proves the worker is there.
    lapsed_unanswered: Vec<String>,
    misses: u32,
    candidate_type: CandidateType,
    stats_started_ms: Option<u64>,
    stats_settled_ms: Option<u64>,
    published_qualified: bool,
}

impl PeerHeartbeat {
    /// The heartbeat for a peer whose worker proved this fingerprint and epoch.
    pub fn new(worker_fp: &str, worker_epoch: &str) -> Self {
        Self {
            probes: TransportProbeState::new(worker_fp, worker_epoch),
            running: false,
            next_due_ms: None,
            in_flight: None,
            lapsed_unanswered: Vec::new(),
            misses: 0,
            candidate_type: CandidateType::None,
            stats_started_ms: None,
            stats_settled_ms: None,
            published_qualified: false,
        }
    }

    /// Whether the heartbeat runs: the peer serves an elected route, some view
    /// wants it, and the page is visible. Starting it probes at once and earns
    /// liveness afresh, because proof from before a pause says nothing about
    /// the peer now; stopping it leaves an outstanding probe to settle.
    pub fn set_running(&mut self, running: bool, now_ms: u64) {
        if running && !self.running {
            self.probes.require_fresh();
            self.in_flight = None;
            self.lapsed_unanswered.clear();
            self.next_due_ms = Some(now_ms);
        }
        if !running {
            self.next_due_ms = None;
        }
        self.running = running;
    }

    /// Whether a heartbeat probe should go out now.
    pub fn is_due(&self, now_ms: u64) -> bool {
        self.running
            && self.in_flight.is_none()
            && self.next_due_ms.is_some_and(|due_ms| now_ms >= due_ms)
    }

    /// The heartbeat probe `request_id` is going out. `false` when the id
    /// cannot be correlated, and nothing was recorded.
    pub fn probe_sent(&mut self, request_id: &str, now_ms: u64) -> bool {
        if !self.probes.start(request_id, now_ms) {
            return false;
        }
        self.in_flight = Some(request_id.to_owned());
        self.next_due_ms = None;
        true
    }

    /// The control lane refused the heartbeat probe's bytes, which is a miss now.
    pub fn probe_refused(&mut self, now_ms: u64) -> HeartbeatMiss {
        if let Some(request_id) = self.in_flight.take() {
            self.probes.refuse(&request_id);
        }
        self.missed(now_ms)
    }

    /// Settle one probe answer. `false` when it answers nothing this peer sent.
    ///
    /// An answer to a probe that already lapsed earns no liveness — the
    /// qualification still needs an answer inside the deadline — but it does
    /// forgive the misses, because the worker behind the peer is answering.
    pub fn answered(&mut self, result: &TransportProbeResult, now_ms: u64) -> bool {
        if !self.probes.resolve(result, now_ms) {
            return self.forgive_late_answer(result);
        }
        if self.in_flight.as_deref() == Some(result.request_id.as_str()) {
            self.in_flight = None;
            self.misses = 0;
            self.lapsed_unanswered.clear();
            self.schedule_next(now_ms);
        }
        true
    }

    /// The heartbeat probe's deadline passed unanswered, when it did.
    pub fn lapsed(&mut self, now_ms: u64) -> Option<HeartbeatMiss> {
        let lapsed = self.probes.expire(now_ms);
        let in_flight = self.in_flight.as_ref()?;
        if !lapsed.contains(in_flight) {
            return None;
        }
        if let Some(request_id) = self.in_flight.take() {
            self.lapsed_unanswered.push(request_id);
        }
        Some(self.missed(now_ms))
    }

    /// A stats read for the candidate kind may start: one interval has passed
    /// since the last settled, and none is outstanding unless it never settled
    /// inside a probe deadline.
    pub fn begin_stats_read(&mut self, now_ms: u64) -> bool {
        let outstanding = self.stats_started_ms.is_some_and(|started_ms| {
            now_ms.saturating_sub(started_ms) < TERMINAL_PEER_PROBE_DEADLINE_MS
        });
        let rested = self.stats_settled_ms.is_none_or(|settled_ms| {
            now_ms.saturating_sub(settled_ms) >= TERMINAL_PEER_HEARTBEAT_INTERVAL_MS
        });
        if outstanding || !rested {
            return false;
        }
        self.stats_started_ms = Some(now_ms);
        true
    }

    /// The browser named the selected pair's candidate kind.
    pub fn stats_arrived(&mut self, candidate_type: CandidateType, now_ms: u64) {
        self.stats_started_ms = None;
        self.stats_settled_ms = Some(now_ms);
        if candidate_type != CandidateType::None {
            self.candidate_type = candidate_type;
        }
    }

    /// The candidate kind the browser last named.
    pub fn candidate_type(&self) -> CandidateType {
        self.candidate_type
    }

    /// What the answers measured, read against `now_ms`.
    pub fn reading(&self, now_ms: u64) -> ProbeReading {
        self.probes.reading(now_ms)
    }

    /// Whether the qualification a reader holds is no longer true at `now_ms`.
    pub fn qualification_changed(&self, now_ms: u64) -> bool {
        self.reading(now_ms).liveness_qualified != self.published_qualified
    }

    /// Record the qualification the telemetry now carries.
    pub fn mark_published(&mut self, qualified: bool) {
        self.published_qualified = qualified;
    }

    fn missed(&mut self, now_ms: u64) -> HeartbeatMiss {
        self.misses = self.misses.saturating_add(1);
        self.schedule_next(now_ms);
        if self.misses >= HEARTBEAT_MISS_LIMIT {
            HeartbeatMiss::Exhausted
        } else {
            HeartbeatMiss::Tolerated
        }
    }

    /// Forgiving clears every lapsed id, so the list never holds more than the
    /// misses one episode can count.
    fn forgive_late_answer(&mut self, result: &TransportProbeResult) -> bool {
        if !self.probes.answers_this_worker(result)
            || !self.lapsed_unanswered.contains(&result.request_id)
        {
            return false;
        }
        self.lapsed_unanswered.clear();
        self.misses = 0;
        true
    }

    fn schedule_next(&mut self, now_ms: u64) {
        if self.running {
            self.next_due_ms = Some(now_ms.saturating_add(TERMINAL_PEER_HEARTBEAT_INTERVAL_MS));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(request_id: &str) -> TransportProbeResult {
        TransportProbeResult {
            request_id: request_id.to_owned(),
            worker_fp: "worker-a".to_owned(),
            worker_epoch: "epoch-1".to_owned(),
        }
    }

    #[test]
    fn a_running_heartbeat_probes_at_once_then_one_interval_after_each_answer() {
        let mut heartbeat = PeerHeartbeat::new("worker-a", "epoch-1");
        assert!(
            !heartbeat.is_due(0),
            "a peer serving no route is not probed"
        );
        heartbeat.set_running(true, 100);
        assert!(heartbeat.is_due(100));
        assert!(heartbeat.probe_sent("probe-1", 100));
        assert!(!heartbeat.is_due(100), "one probe at a time");
        assert!(heartbeat.answered(&answer("probe-1"), 130));
        assert!(heartbeat.reading(130).liveness_qualified);
        assert!(!heartbeat.is_due(130 + TERMINAL_PEER_HEARTBEAT_INTERVAL_MS - 1));
        assert!(heartbeat.is_due(130 + TERMINAL_PEER_HEARTBEAT_INTERVAL_MS));
    }

    #[test]
    fn the_third_consecutive_miss_closes_the_peer_and_an_answer_forgives() {
        let mut heartbeat = PeerHeartbeat::new("worker-a", "epoch-1");
        heartbeat.set_running(true, 0);
        assert!(heartbeat.probe_sent("probe-1", 0));
        let first_deadline = TERMINAL_PEER_PROBE_DEADLINE_MS;
        assert_eq!(heartbeat.lapsed(first_deadline - 1), None);
        assert_eq!(
            heartbeat.lapsed(first_deadline),
            Some(HeartbeatMiss::Tolerated)
        );
        let second_at = first_deadline + TERMINAL_PEER_HEARTBEAT_INTERVAL_MS;
        assert!(heartbeat.is_due(second_at));
        assert!(heartbeat.probe_sent("probe-2", second_at));
        assert!(heartbeat.answered(&answer("probe-2"), second_at + 5));

        let mut probe_at = second_at + 5 + TERMINAL_PEER_HEARTBEAT_INTERVAL_MS;
        for (index, expected) in [
            HeartbeatMiss::Tolerated,
            HeartbeatMiss::Tolerated,
            HeartbeatMiss::Exhausted,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(heartbeat.probe_sent(&format!("probe-{}", index + 3), probe_at));
            assert_eq!(
                heartbeat.lapsed(probe_at + TERMINAL_PEER_PROBE_DEADLINE_MS),
                Some(expected),
                "miss {} after the answer that reset the count",
                index + 1
            );
            probe_at += TERMINAL_PEER_PROBE_DEADLINE_MS + TERMINAL_PEER_HEARTBEAT_INTERVAL_MS;
        }
        assert!(!heartbeat.reading(probe_at).liveness_qualified);
    }

    #[test]
    fn a_late_answer_forgives_the_misses_but_earns_no_liveness() {
        let mut heartbeat = PeerHeartbeat::new("worker-a", "epoch-1");
        heartbeat.set_running(true, 0);
        assert!(heartbeat.probe_sent("probe-1", 0));
        let first_deadline = TERMINAL_PEER_PROBE_DEADLINE_MS;
        assert_eq!(
            heartbeat.lapsed(first_deadline),
            Some(HeartbeatMiss::Tolerated)
        );
        let second_at = first_deadline + TERMINAL_PEER_HEARTBEAT_INTERVAL_MS;
        assert!(heartbeat.probe_sent("probe-2", second_at));
        let second_deadline = second_at + TERMINAL_PEER_PROBE_DEADLINE_MS;
        assert_eq!(
            heartbeat.lapsed(second_deadline),
            Some(HeartbeatMiss::Tolerated)
        );

        let late_at = second_deadline + 1_000;
        let mut stranger = answer("probe-1");
        stranger.worker_epoch = "epoch-2".to_owned();
        assert!(
            !heartbeat.answered(&stranger, late_at),
            "a restarted worker's answer forgives nothing"
        );
        assert!(heartbeat.answered(&answer("probe-1"), late_at));
        assert!(
            !heartbeat.reading(late_at).liveness_qualified,
            "an answer past its deadline is not proof of a live route"
        );
        assert!(
            !heartbeat.answered(&answer("probe-1"), late_at),
            "one late answer forgives once"
        );

        let third_at = second_deadline + TERMINAL_PEER_HEARTBEAT_INTERVAL_MS;
        assert!(heartbeat.probe_sent("probe-3", third_at));
        assert_eq!(
            heartbeat.lapsed(third_at + TERMINAL_PEER_PROBE_DEADLINE_MS),
            Some(HeartbeatMiss::Tolerated),
            "the late answer reset the count, so this is the first miss again"
        );
    }
}
