//! What a host may OBSERVE about one worker's peer attempt, and the interface
//! it observes it through: the accessors the carrier registry reads, and
//! `PeerSignalling`, the lane the route table drives. Every field here is a
//! projection of state `super::signaling` owns, and no transition lives in this
//! file — a host that learned to change an attempt through it would have two
//! writers for one state machine.

use std::collections::BTreeSet;

use crate::client::carriers::loopback::LoopbackProbe;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::transport_trait::PeerSignalling;
use crate::client::carriers::{CarrierEffect, PeerPhase, SignallingInput, SignallingSnapshot};
use crate::terminal::token::TerminalTransport;

impl Signalling {
    /// The gate that decides whether a peer is needed, and this machine's one
    /// view of a staged loopback carrier.
    pub fn loopback_probe(&self) -> &LoopbackProbe {
        &self.loopback
    }

    /// The sessions a view currently wants here.
    pub fn demanded_sessions(&self) -> &BTreeSet<String> {
        &self.demand
    }

    /// Where the attempt is.
    pub fn phase(&self) -> PeerPhase {
        self.phase
    }
}

impl PeerSignalling for Signalling {
    fn worker_fp(&self) -> &str {
        &self.worker_fp
    }

    fn snapshot(&self) -> SignallingSnapshot {
        SignallingSnapshot {
            worker_fp: self.worker_fp.clone(),
            phase: self.phase,
            fallback_reason: self.faults.reason,
            active_views: self.active_views,
            demanded_sessions: self.demand.clone(),
            has_carrier: self.peer_held,
            transport_held: self.peer_held.then_some(TerminalTransport::Peer),
            peers_allocated: self.env.peers_allocated,
            grant_phase: self.grant.phase(),
            sync_generation: self.env.sync_generation,
            last_failure_detail: self.faults.last_detail.clone(),
        }
    }

    fn phase(&self) -> PeerPhase {
        self.phase
    }

    fn step(&mut self, input: SignallingInput) -> Vec<CarrierEffect> {
        Signalling::step(self, input)
    }
}
