//! The direct-carrier lane: one `Signalling` machine per worker, driven.
//!
//! Owned by `client::carriers`, held by `Store` as `direct`, and fed from
//! `handle_terminal` (view demand, carrier presence, promotion), `handle_event`
//! (the host's own observations) and `handle_sweep` (the deadlines). It is the
//! only thing in the crate that constructs a `Signalling`, which is why the
//! machine's own file could ship with a full set of rules and no runtime
//! evidence any of them ran: a state machine nobody instantiates is a
//! simulation.
//!
//! It owns no rule. Every decision is `super::signaling::Signalling::step`; the
//! two questions this file answers are the ones a fleet has more than one
//! answer to — which machine an observation belongs to, and what a machine's
//! effects become in the core's own `Effect` vocabulary.
//!
//! The second answer is why `Effect::Carrier` exists. `CarrierEffect` already
//! carries `Core(Effect)` so the machine cannot grow a second vocabulary for a
//! decision the core speaks; the peer-lifecycle arms have no `Effect` spelling,
//! and inventing one here would be exactly the fork that arm exists to prevent.
//! So the whole enum travels, and this file only unwraps the arm that is
//! already a decision the core made.

mod prewarm;

use std::collections::BTreeMap;

use crate::client::carriers::grant::DirectGrant;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::signaling_snapshot::SignallingSnapshot;
use crate::client::carriers::transport_trait::PeerSignalling;
use crate::client::carriers::{
    CarrierEffect, CarrierEnvironment, PeerPhase, PeerTelemetry, SignallingInput,
};
use crate::effect::Effect;

/// One machine per worker this document has wanted a direct carrier for.
#[derive(Debug)]
pub struct CarrierLane {
    machines: BTreeMap<String, Signalling>,
    environment: CarrierEnvironment,
    /// The last attempt id any machine minted. Ids are DOCUMENT-unique, not
    /// per worker: the host keys every open peer by its attempt id, so a second
    /// worker's attempt 1 would displace — and close — the first worker's.
    last_attempt_id: u64,
}

impl Default for CarrierLane {
    fn default() -> Self {
        Self::new()
    }
}

impl CarrierLane {
    /// A lane with no machines and no environment reported.
    ///
    /// The default environment is a document that cannot peer and holds no
    /// peers, which is what `ClientCore::in_memory` is: a host that has not
    /// told this lane what its browser can do gets the machine that will not
    /// allocate a peer, not the one that will.
    pub fn new() -> Self {
        Self {
            machines: BTreeMap::new(),
            environment: CarrierEnvironment {
                peers_allocated: 0,
                peer_transport_available: false,
                sync_generation: 0,
                stun_urls: None,
            },
            last_attempt_id: 0,
        }
    }

    /// What this document can currently do, as the host sees it.
    ///
    /// Re-declared rather than read once: peer availability is a property of
    /// the DOCUMENT (a secure context, a constructor) and the peer count moves
    /// as attempts open and close, so a value captured at construction would
    /// freeze both. Every machine is brought up to date before it is stepped,
    /// which is what makes the document-wide cap in `Signalling::start` mean
    /// the cap the browser is actually at.
    pub fn set_environment(&mut self, peer_transport_available: bool, sync_generation: u64) {
        self.environment.peer_transport_available = peer_transport_available;
        self.environment.sync_generation = sync_generation;
    }

    /// The STUN servers the coordinator advertised at identity time, which
    /// lets a machine open its transport while its grant is being minted.
    pub fn set_stun_urls(&mut self, stun_urls: Option<Vec<String>>) {
        self.environment.stun_urls = stun_urls;
    }

    /// How many WebRTC peers the host is holding, for the document-wide cap.
    pub fn note_peers_allocated(&mut self, peers_allocated: u32) {
        self.environment.peers_allocated = peers_allocated;
    }

    /// A view started or stopped wanting a session on this worker, at the
    /// host's `now_ms`.
    pub fn demand(
        &mut self,
        session_id: &str,
        worker_fp: &str,
        view_id: &str,
        active: bool,
        now_ms: u64,
        out: &mut Vec<Effect>,
    ) {
        self.observe(
            worker_fp,
            SignallingInput::Demand {
                session_id: session_id.to_owned(),
                view_id: view_id.to_owned(),
                active,
                now_ms,
            },
            out,
        );
    }

    /// The loopback probe learned which worker serves this page's own machine,
    /// or learned that none does.
    pub fn local_door_answered(
        &mut self,
        worker_fp: &str,
        serving_worker_fp: &str,
        out: &mut Vec<Effect>,
    ) {
        self.observe(
            worker_fp,
            SignallingInput::LocalDoorAnswered {
                worker_fp: serving_worker_fp.to_owned(),
            },
            out,
        );
    }

    /// A loopback carrier for this worker came up, or the one that was is gone.
    pub fn loopback_staged(&mut self, worker_fp: &str, staged: bool, out: &mut Vec<Effect>) {
        self.observe(
            worker_fp,
            SignallingInput::LoopbackCarrierStaged { staged },
            out,
        );
    }

    /// The coordinator minted a credential and the worker acknowledged it, at
    /// the host's `now_ms` — the instant a held offer goes to the coordinator.
    pub fn grant_minted(
        &mut self,
        grant: crate::client::carriers::grant::DirectGrant,
        now_ms: u64,
        out: &mut Vec<Effect>,
    ) {
        let worker_fp = grant.worker_fp.clone();
        if let Some(machine) = self.machines.get_mut(&worker_fp) {
            machine.advance_clock(now_ms);
        }
        self.observe(
            &worker_fp,
            SignallingInput::Grant(crate::client::carriers::grant::GrantInput::Minted(grant)),
            out,
        );
    }

    /// The mint request returned without a worker's acknowledgement.
    pub fn grant_refused(
        &mut self,
        worker_fp: &str,
        now_ms: u64,
        reason: &str,
        out: &mut Vec<Effect>,
    ) {
        self.observe(
            worker_fp,
            SignallingInput::Grant(crate::client::carriers::grant::GrantInput::Refused {
                now_ms,
                reason: reason.to_owned(),
            }),
            out,
        );
    }

    /// What the transport reported about one attempt, at the host's `now_ms`.
    pub fn transport_observed(
        &mut self,
        worker_fp: &str,
        input: SignallingInput,
        now_ms: u64,
        out: &mut Vec<Effect>,
    ) {
        if let Some(machine) = self.machines.get_mut(worker_fp) {
            machine.advance_clock(now_ms);
        }
        self.observe(worker_fp, input, out);
    }

    /// What the transport measured about the live peer, for the route
    /// diagnostic. Separate from `transport_observed` because a measurement is
    /// not an event: it moves the snapshot and never a phase.
    pub fn record_telemetry(&mut self, worker_fp: &str, telemetry: PeerTelemetry) {
        if let Some(machine) = self.machines.get_mut(worker_fp) {
            machine.set_telemetry(telemetry);
        }
    }

    /// The host's clock reached a retry this lane asked for.
    pub fn retry_due(&mut self, worker_fp: &str, now_ms: u64, out: &mut Vec<Effect>) {
        self.observe(worker_fp, SignallingInput::RetryDue { now_ms }, out);
    }

    /// One pass over every worker's deadlines.
    ///
    /// A sweep that reached no machine is not a sweep that failed: it is a
    /// document with no direct carrier, which is every document whose sessions
    /// are all on Sync and is not a condition to report.
    pub fn sweep(&mut self, now_ms: u64, out: &mut Vec<Effect>) {
        let fps: Vec<String> = self.machines.keys().cloned().collect();
        for worker_fp in fps {
            self.observe(&worker_fp, SignallingInput::Sweep { now_ms }, out);
        }
        self.prune();
    }

    /// This worker is gone: its machine, its grant, and its demand all go.
    pub fn retire(&mut self, worker_fp: &str, out: &mut Vec<Effect>) {
        if !self.machines.contains_key(worker_fp) {
            return;
        }
        let effects = self
            .machines
            .remove(worker_fp)
            .map_or_else(Vec::new, |mut machine| {
                machine.step(SignallingInput::WorkerRetired)
            });
        emit(effects, out);
    }

    /// What a host may observe about one worker.
    ///
    /// A worker with no machine is not an error and is not `None`: it is a
    /// worker this document has never wanted a direct carrier for, which the
    /// snapshot says by being idle with no grant and no demand.
    pub fn snapshot(&self, worker_fp: &str) -> SignallingSnapshot {
        self.machines.get(worker_fp).map_or_else(
            || SignallingSnapshot::idle(worker_fp.to_owned()),
            PeerSignalling::snapshot,
        )
    }

    /// The credential this worker's live attempt authenticates with.
    ///
    /// Read at the moment a host SPENDS the credential — the `Hello` on a
    /// WebRTC control lane — because the secret is deliberately absent from
    /// [`PeerAttempt`]: an attempt is traced, logged and carried inside a
    /// `Debug` print, and a secret that reaches any of those is a secret that
    /// outlives its grant. `None` is the core's own answer, so a host that asks
    /// about a grant it no longer holds sends nothing rather than sending a
    /// dead one.
    pub fn live_grant(&self, worker_fp: &str, now_ms: u64) -> Option<&DirectGrant> {
        self.machines
            .get(worker_fp)
            .and_then(|machine| machine.live_grant(now_ms))
    }

    /// Where one worker's attempt is, for a host that logs transitions.
    pub fn phase(&self, worker_fp: &str) -> PeerPhase {
        self.machines
            .get(worker_fp)
            .map_or(PeerPhase::Idle, PeerSignalling::phase)
    }

    /// The workers this lane holds a machine for.
    pub fn workers(&self) -> impl Iterator<Item = &str> {
        self.machines.keys().map(String::as_str)
    }

    /// Fold one observation into the named worker's machine.
    fn observe(&mut self, worker_fp: &str, input: SignallingInput, out: &mut Vec<Effect>) {
        if worker_fp.is_empty() {
            // A view with no replica has no worker, and a machine per empty
            // string would be a machine that answers for every such view.
            return;
        }
        let environment = self.environment.clone();
        let machine = self
            .machines
            .entry(worker_fp.to_owned())
            .or_insert_with(|| Signalling::new(worker_fp, environment.clone(), 0));
        machine.set_environment(environment);
        machine.next_attempt_id = machine.next_attempt_id.max(self.last_attempt_id);
        let effects = machine.step(input);
        self.last_attempt_id = machine.next_attempt_id;
        emit(effects, out);
    }

    /// Forget the machines no view wants, no pre-warm holds, and no carrier is
    /// held for.
    ///
    /// On the sweep rather than on the view close, because a close is also the
    /// moment a resize or a re-attach is arriving, and dropping the machine
    /// between them would discard a grant the next view still needs. A machine
    /// in cooldown, or holding a peer, is kept whatever the demand says.
    fn prune(&mut self) {
        self.machines.retain(|_, machine| {
            machine.active_views > 0
                || !machine.prewarm_sessions.is_empty()
                || machine.peer_held
                || machine.phase != PeerPhase::Idle
        });
    }
}

/// One machine's effects in the core's own vocabulary.
fn emit(effects: Vec<CarrierEffect>, out: &mut Vec<Effect>) {
    for effect in effects {
        match effect {
            CarrierEffect::Core(inner) => out.push(inner),
            lifecycle => out.push(Effect::Carrier(Box::new(lifecycle))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Demand on a worker with nothing to grant it: the machine comes up, asks
    /// for the credential, and opens nothing.
    ///
    /// The `RetryAt` beside the request is the loopback grace, not a fault: the
    /// probe has not answered, so `start` schedules the one re-check the
    /// unanswered state allows and returns. A page that turns out to share the
    /// worker's machine gets its fast path from that re-check.
    #[test]
    fn demand_asks_for_a_grant_and_opens_nothing() {
        let mut lane = CarrierLane::new();
        let mut out = Vec::new();
        lane.demand("session-a", "worker-a", "view-1", true, 0, &mut out);
        assert_eq!(
            out.first(),
            Some(&Effect::RequestDirectGrant {
                session_ids: vec!["session-a".to_owned()],
                worker_fp: "worker-a".to_owned(),
            }),
            "a demanded session must reach the coordinator; got {out:?}"
        );
        assert!(
            !opens_a_transport(&out),
            "an unanswered probe holds no transport open; got {out:?}"
        );
        assert!(
            out.iter()
                .any(|effect| matches!(effect, Effect::Carrier(inner)
                    if matches!(**inner, CarrierEffect::RetryAt { .. }))),
            "and the unanswered probe owes one grace re-check; got {out:?}"
        );
    }

    /// A page the worker itself serves must not allocate a peer, which is the
    /// whole rule `terminal-peer.spec.ts:62` is named for. The negative is the
    /// assertion: a settled SAME HOST answer must not even arm a re-check.
    #[test]
    fn a_same_host_answer_parks_the_machine_without_a_peer() {
        let mut lane = CarrierLane::new();
        let mut out = Vec::new();
        lane.set_environment(true, 0);
        lane.demand("session-a", "worker-a", "view-1", true, 0, &mut out);
        out.clear();
        lane.local_door_answered("worker-a", "worker-a", &mut out);
        assert!(
            out.is_empty(),
            "a settled SAME HOST probe owes nothing at all; got {out:?}"
        );
        assert_eq!(lane.phase("worker-a"), PeerPhase::Idle);
    }

    /// Whether any effect opened a transport, which is the only question the
    /// two tests above are about.
    fn opens_a_transport(effects: &[Effect]) -> bool {
        effects.iter().any(|effect| {
            matches!(effect, Effect::Carrier(inner)
                if matches!(**inner, CarrierEffect::OpenTransport { .. }))
        })
    }
}
