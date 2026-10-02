//! The control-lane arms: the `subscribed` barrier, domain resets, input
//! results, route and probe answers, and the relocation notice.
//!
//! Called by `decode::map_arm` after the meta rule has passed. The `subscribed`
//! and `domain_reset` checks are v2's `handleSubscribed` / `handleDomainReset`
//! (`apps/web/src/store/sync-inbound.ts:85-148`), whose throws close the link.

use std::collections::BTreeSet;

use roost_proto::{
    CoordinatorRelocationFrame, InputAccepted, InputAmbiguous, InputRejected, SyncDomainResetFrame,
    SyncSubscribedFrame, TerminalInputRouteResult, TerminalTransportProbeResult,
};

use super::known_domain;
use crate::sync::inbound::{
    CoordinatorRelocation, InputRouteResult, SyncFrame, TransportProbeResult,
};
use crate::sync::link::SyncDomain;
use crate::terminal::input::InputOutcome;

/// The barrier: a socket identity, a process epoch, and exactly one positive
/// generation for every domain. Anything less is "duplicate or malformed
/// subscribed control" / "invalid subscribed domain generation" /
/// "incomplete subscribed domain generations" in v2.
pub(super) fn subscribed(value: SyncSubscribedFrame) -> Result<SyncFrame, String> {
    if value.socket_id.is_empty() || value.process_epoch.is_empty() {
        return Err("subscribed names no socket id or process epoch".to_owned());
    }
    let mut seen = BTreeSet::new();
    let mut domains = Vec::with_capacity(value.generations.len());
    for entry in &value.generations {
        let wire_value = entry.domain.to_i32();
        let Some(domain) = known_domain(wire_value) else {
            return Err(format!("subscribed names unknown domain {wire_value}"));
        };
        if entry.generation == 0 || !seen.insert(domain) {
            return Err(format!(
                "invalid subscribed generation for {}",
                domain.as_str()
            ));
        }
        domains.push((domain, entry.generation, entry.subscribed));
    }
    if seen.len() != SyncDomain::ALL.len() {
        return Err(format!(
            "incomplete subscribed domain generations: {} of {}",
            seen.len(),
            SyncDomain::ALL.len()
        ));
    }
    Ok(SyncFrame::Subscribed {
        socket_id: value.socket_id,
        process_epoch: value.process_epoch,
        domains,
    })
}

/// A domain reset names a known domain and a positive generation.
pub(super) fn domain_reset(value: SyncDomainResetFrame) -> Result<SyncFrame, String> {
    let wire_value = value.domain.to_i32();
    let Some(domain) = known_domain(wire_value) else {
        return Err(format!("domain_reset names unknown domain {wire_value}"));
    };
    if value.generation == 0 {
        return Err(format!(
            "domain_reset for {} has generation 0",
            domain.as_str()
        ));
    }
    Ok(SyncFrame::DomainReset {
        domain,
        generation: value.generation,
        reason: value.reason,
        subscribed: value.subscribed,
    })
}

/// The worker wrote the batch.
pub(super) fn input_accepted(value: InputAccepted) -> SyncFrame {
    SyncFrame::InputResult {
        session_id: value.session_id,
        input_seq: value.input_seq,
        outcome: InputOutcome::Accepted {
            input_seq: value.input_seq,
            written_bytes: value.written_bytes,
        },
        generation: value.domain_generation,
    }
}

/// The worker refused the batch.
pub(super) fn input_rejected(value: InputRejected) -> SyncFrame {
    SyncFrame::InputResult {
        session_id: value.session_id,
        input_seq: value.input_seq,
        outcome: InputOutcome::Rejected {
            input_seq: value.input_seq,
            reason: value.reason,
        },
        generation: value.domain_generation,
    }
}

/// Nobody can say whether the worker wrote the batch.
pub(super) fn input_ambiguous(value: InputAmbiguous) -> SyncFrame {
    SyncFrame::InputResult {
        session_id: value.session_id,
        input_seq: value.input_seq,
        outcome: InputOutcome::Ambiguous {
            input_seq: value.input_seq,
            written_bytes: value.written_bytes,
            reason: value.reason,
        },
        generation: value.domain_generation,
    }
}

/// The answer to a route claim.
pub(super) fn input_route_result(value: TerminalInputRouteResult) -> SyncFrame {
    SyncFrame::InputRouteResult {
        result: input_route_result_of(value),
    }
}

/// The answer to a route claim, in the client's vocabulary. Shared with the
/// direct carrier, whose worker answers a claim in the same message.
pub(crate) fn input_route_result_of(value: TerminalInputRouteResult) -> InputRouteResult {
    InputRouteResult {
        request_id: value.request_id,
        session_id: value.session_id,
        revision: value.revision,
        accepted: value.accepted,
        latest_revision: value.latest_revision,
        input_route_epoch: value.input_route_epoch,
        worker_epoch: value.worker_epoch,
        reason: value.reason,
    }
}

/// The answer to a transport probe.
pub(super) fn transport_probe_result(value: TerminalTransportProbeResult) -> SyncFrame {
    SyncFrame::TransportProbeResult {
        result: TransportProbeResult {
            request_id: value.request_id,
            worker_fp: value.worker_fp,
            worker_epoch: value.worker_epoch,
        },
    }
}

/// A relocation notice, decoded so the fold can close the link with its
/// target in the reason.
pub(super) fn coordinator_relocation(value: CoordinatorRelocationFrame) -> SyncFrame {
    SyncFrame::CoordinatorRelocation {
        relocation: CoordinatorRelocation {
            handoff_id: value.handoff_id,
            source_url: value.source_url,
            target_url: value.target_url,
        },
    }
}
