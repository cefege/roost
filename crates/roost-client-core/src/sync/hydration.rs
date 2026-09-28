//! The per-domain snapshot calls in flight, their deadline, their retry, and
//! the bootstrap probe that tells an unknown device from an unreachable
//! coordinator.
//!
//! Owned by `SyncState`; driven by `handle_sync::hydration` (trigger, settle,
//! sweep). Ported from `apps/web/src/store/sync-domain-hydration.ts`
//! (`registerDomainHydrator`'s run/scheduleRetry/trigger) and the subscribed
//! wait of `apps/web/src/store/sync-bootstrap.ts:196-220`.

use std::collections::BTreeMap;

use crate::sync::link::SyncDomain;

/// A hydration that never settles holds its domain un-ready forever; past this
/// deadline it is treated as a rejected snapshot and retried.
pub const SYNC_HYDRATION_DEADLINE_MS: u64 = 15_000;
/// How long bootstrap waits for `subscribed` before probing whether this
/// device is known at all.
pub const SYNC_SUBSCRIBED_WAIT_MS: u64 = 3_000;
/// The first hydration retry delay.
pub const SYNC_HYDRATION_RETRY_BASE_MS: u64 = 500;
/// The hydration retry ceiling.
pub const SYNC_HYDRATION_RETRY_MAX_MS: u64 = 10_000;

/// `min(500·2^attempt, 10s)`.
pub fn hydration_retry_delay_ms(attempt: u32) -> u64 {
    SYNC_HYDRATION_RETRY_BASE_MS
        .checked_shl(attempt.min(16))
        .unwrap_or(u64::MAX)
        .min(SYNC_HYDRATION_RETRY_MAX_MS)
}

/// One snapshot call and the exact generation it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HydrationTicket {
    /// The domain.
    pub domain: SyncDomain,
    /// The domain generation the snapshot must belong to.
    pub domain_generation: u64,
    /// The socket generation that asked.
    pub socket_generation: u64,
    /// When the call is abandoned and retried.
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Retry {
    attempt: u32,
    due_ms: Option<u64>,
    socket_generation: u64,
    domain_generation: u64,
}

/// Every domain's hydration bookkeeping for the live socket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hydrations {
    in_flight: BTreeMap<u64, HydrationTicket>,
    retries: BTreeMap<SyncDomain, Retry>,
    /// When the latest dial began, for the subscribed wait.
    dial_started_ms: Option<u64>,
    /// The probe call in flight, if any.
    probe_call_id: Option<u64>,
    /// Whether the probe already ran for the latest dial.
    probed: bool,
}

impl Hydrations {
    /// Record a snapshot call. A newer call for the same domain supersedes an
    /// older one, whose answer is then ignored.
    pub fn begin(&mut self, call_id: u64, ticket: HydrationTicket) {
        self.in_flight
            .retain(|_, existing| existing.domain != ticket.domain);
        self.in_flight.insert(call_id, ticket);
        let retry = self.retries.entry(ticket.domain).or_insert(Retry {
            attempt: 0,
            due_ms: None,
            socket_generation: ticket.socket_generation,
            domain_generation: ticket.domain_generation,
        });
        if retry.socket_generation != ticket.socket_generation
            || retry.domain_generation != ticket.domain_generation
        {
            *retry = Retry {
                attempt: 0,
                due_ms: None,
                socket_generation: ticket.socket_generation,
                domain_generation: ticket.domain_generation,
            };
        }
        retry.due_ms = None;
    }

    /// The ticket for an answer, removed. `None` for an answer that is not a
    /// hydration or was superseded.
    pub fn take(&mut self, call_id: u64) -> Option<HydrationTicket> {
        self.in_flight.remove(&call_id)
    }

    /// A snapshot applied: the next failure for this domain starts over.
    pub fn succeeded(&mut self, domain: SyncDomain) {
        self.retries.remove(&domain);
    }

    /// Schedule the retry for a failed ticket, returning its delay.
    pub fn schedule_retry(&mut self, ticket: &HydrationTicket, now_ms: u64) -> u64 {
        let retry = self.retries.entry(ticket.domain).or_insert(Retry {
            attempt: 0,
            due_ms: None,
            socket_generation: ticket.socket_generation,
            domain_generation: ticket.domain_generation,
        });
        let delay = hydration_retry_delay_ms(retry.attempt);
        retry.attempt = retry.attempt.saturating_add(1);
        retry.due_ms = Some(now_ms.saturating_add(delay));
        delay
    }

    /// The retries now due, each with the generation it was scheduled for.
    pub fn take_due_retries(&mut self, now_ms: u64) -> Vec<(SyncDomain, u64, u64)> {
        let mut due = Vec::new();
        for (domain, retry) in &mut self.retries {
            if retry.due_ms.is_some_and(|at| at <= now_ms) {
                retry.due_ms = None;
                due.push((*domain, retry.socket_generation, retry.domain_generation));
            }
        }
        due
    }

    /// The calls past their deadline, removed.
    pub fn take_expired(&mut self, now_ms: u64) -> Vec<HydrationTicket> {
        let expired: Vec<u64> = self
            .in_flight
            .iter()
            .filter(|(_, ticket)| ticket.deadline_ms <= now_ms)
            .map(|(call_id, _)| *call_id)
            .collect();
        expired
            .into_iter()
            .filter_map(|call_id| self.in_flight.remove(&call_id))
            .collect()
    }

    /// Whether a call for this exact generation is already running.
    pub fn is_running(&self, domain: SyncDomain, domain_generation: u64) -> bool {
        self.in_flight
            .values()
            .any(|ticket| ticket.domain == domain && ticket.domain_generation == domain_generation)
    }

    /// The socket changed: every call and retry belongs to the old one.
    pub fn clear_for_new_socket(&mut self) {
        self.in_flight.clear();
        self.retries.clear();
    }

    /// A dial began; the subscribed wait restarts from `now_ms`.
    pub fn note_dial_started(&mut self, now_ms: u64) {
        self.dial_started_ms = Some(now_ms);
        self.probed = false;
    }

    /// `subscribed` arrived; no probe is owed for this dial.
    pub fn note_subscribed(&mut self) {
        self.dial_started_ms = None;
    }

    /// Whether the subscribed wait for the latest dial has run out, taking it.
    pub fn take_probe_due(&mut self, now_ms: u64) -> bool {
        let due = !self.probed
            && self.probe_call_id.is_none()
            && self
                .dial_started_ms
                .is_some_and(|at| now_ms.saturating_sub(at) >= SYNC_SUBSCRIBED_WAIT_MS);
        if due {
            self.probed = true;
        }
        due
    }

    /// Record the probe call.
    pub fn begin_probe(&mut self, call_id: u64) {
        self.probe_call_id = Some(call_id);
    }

    /// Whether `call_id` is the probe, clearing it.
    pub fn take_probe(&mut self, call_id: u64) -> bool {
        if self.probe_call_id == Some(call_id) {
            self.probe_call_id = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(domain: SyncDomain, generation: u64) -> HydrationTicket {
        HydrationTicket {
            domain,
            domain_generation: generation,
            socket_generation: 1,
            deadline_ms: 15_000,
        }
    }

    #[test]
    fn the_retry_delay_doubles_from_half_a_second_to_ten() {
        let delays: Vec<u64> = (0..7).map(hydration_retry_delay_ms).collect();
        assert_eq!(delays, [500, 1_000, 2_000, 4_000, 8_000, 10_000, 10_000]);
    }

    #[test]
    fn a_newer_call_supersedes_the_older_one_for_its_domain() {
        let mut hydrations = Hydrations::default();
        hydrations.begin(1, ticket(SyncDomain::Workers, 1));
        hydrations.begin(2, ticket(SyncDomain::Workers, 2));
        assert_eq!(hydrations.take(1), None);
        assert_eq!(hydrations.take(2).map(|t| t.domain_generation), Some(2));
    }

    #[test]
    fn a_failed_call_retries_with_growing_delay_and_success_resets_it() {
        let mut hydrations = Hydrations::default();
        let failed = ticket(SyncDomain::Tasks, 3);
        hydrations.begin(1, failed);
        assert_eq!(hydrations.schedule_retry(&failed, 0), 500);
        assert!(hydrations.take_due_retries(499).is_empty());
        assert_eq!(hydrations.take_due_retries(500), [(SyncDomain::Tasks, 1, 3)]);
        assert_eq!(hydrations.schedule_retry(&failed, 500), 1_000);
        hydrations.succeeded(SyncDomain::Tasks);
        assert_eq!(hydrations.schedule_retry(&failed, 0), 500);
    }

    #[test]
    fn a_call_past_its_deadline_expires_once() {
        let mut hydrations = Hydrations::default();
        hydrations.begin(7, ticket(SyncDomain::Mcp, 1));
        assert!(hydrations.take_expired(14_999).is_empty());
        assert_eq!(hydrations.take_expired(15_000).len(), 1);
        assert!(hydrations.take_expired(99_999).is_empty());
    }

    #[test]
    fn the_probe_runs_once_per_dial_after_the_subscribed_wait() {
        let mut hydrations = Hydrations::default();
        hydrations.note_dial_started(1_000);
        assert!(!hydrations.take_probe_due(3_999));
        assert!(hydrations.take_probe_due(4_000));
        assert!(!hydrations.take_probe_due(9_000));
        hydrations.note_dial_started(10_000);
        hydrations.note_subscribed();
        assert!(!hydrations.take_probe_due(20_000));
    }
}
