//! The opening half of a peer attempt: opening the transport, reading its
//! offer, and adopting a grant minted while it gathered. An `impl Signalling`
//! block beside `super::signaling`, whose `start` and `step` call it; the
//! grantless open exists so ICE gathering overlaps the mint's round trip, and
//! the offer is held here until the worker has the grant installed.

use std::collections::BTreeSet;

use crate::client::carriers::faults::sdp_is_usable;
use crate::client::carriers::grant::DirectGrant;
use crate::client::carriers::signaling::Signalling;
use crate::client::carriers::{CarrierEffect, CarrierFault, PeerAttempt, PeerPhase};
use crate::terminal::token::TerminalTransport;

impl Signalling {
    /// Mint a peer attempt and ask the host to open it: from the live grant
    /// when there is one, otherwise grantless on the advertised STUN servers,
    /// which `start` only allows while the first mint is in flight.
    pub(crate) fn open_peer(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        let granted = self.grant.live_grant(now_ms).cloned();
        if let Some(grant) = &granted
            && !grant.admits(TerminalTransport::Peer)
        {
            let fault = CarrierFault::Disabled;
            self.set_phase(PeerPhase::Disabled, Some(fault.reason()));
            return self.report(fault, "this grant cannot open a peer carrier");
        }
        self.next_attempt_id += 1;
        let mut attempt = PeerAttempt {
            attempt_id: self.next_attempt_id,
            worker_fp: self.worker_fp.clone(),
            worker_epoch: String::new(),
            transport: TerminalTransport::Peer,
            // Minted by the host as it opens the transport (`crypto.randomUUID`,
            // v2 `createTerminalDirectRequestId`) and adopted from the offer: this
            // crate holds no entropy, and the coordinator refuses a non-UUID.
            peer_id: String::new(),
            grant_id: String::new(),
            tab_id: String::new(),
            device_fingerprint: String::new(),
            stun_urls: self.env.stun_urls.clone().unwrap_or_default(),
            session_ids: BTreeSet::new(),
        };
        if let Some(grant) = &granted {
            attempt.stun_urls = grant.stun_urls.clone();
            adopt_grant_fields(&mut attempt, grant);
        }
        self.attempt = Some(attempt.clone());
        self.held_offer = None;
        self.attempt_started_ms = now_ms;
        self.set_phase(PeerPhase::Gathering, None);
        vec![CarrierEffect::OpenTransport { attempt }]
    }

    /// The transport produced a local offer, under the peer id it minted. An
    /// attempt still waiting on its grant holds the offer for `adopt_grant`.
    pub(crate) fn offer(
        &mut self,
        attempt_id: u64,
        peer_id: String,
        offer_sdp: String,
    ) -> Vec<CarrierEffect> {
        let Some(open) = self
            .attempt
            .as_mut()
            .filter(|open| open.attempt_id == attempt_id)
        else {
            return Vec::new();
        };
        open.peer_id = peer_id;
        let grantless = open.grant_id.is_empty();
        if !sdp_is_usable(&offer_sdp) {
            let detail = "terminal peer offer has no usable candidates";
            return self.fault(attempt_id, CarrierFault::InvalidOffer, detail);
        }
        if grantless {
            self.held_offer = Some(offer_sdp);
            self.set_phase(PeerPhase::AwaitingGrant, None);
            return Vec::new();
        }
        self.negotiate(offer_sdp)
    }

    /// A mint landed: a grantless attempt takes its tuple, and an offer read
    /// before it goes to the coordinator now. A grant that lands before
    /// gathering finished is adopted silently, and the later offer negotiates.
    pub(crate) fn adopt_grant(&mut self, now_ms: u64) -> Vec<CarrierEffect> {
        if !self.holds_grantless_attempt() {
            return Vec::new();
        }
        let Some(grant) = self.grant.live_grant(now_ms).cloned() else {
            return Vec::new();
        };
        if !grant.admits(TerminalTransport::Peer) {
            // `start`, which the mint runs next, parks the machine `Disabled`
            // and reports the fault once, as it does for a granted open.
            return self.close_open_attempt("this grant cannot open a peer carrier");
        }
        if let Some(open) = self.attempt.as_mut() {
            adopt_grant_fields(open, &grant);
        }
        match self.held_offer.take() {
            Some(offer_sdp) => self.negotiate(offer_sdp),
            None => Vec::new(),
        }
    }

    /// Whether the open attempt is still gathering ahead of its grant.
    pub(crate) fn holds_grantless_attempt(&self) -> bool {
        self.attempt
            .as_ref()
            .is_some_and(|open| open.grant_id.is_empty())
    }

    /// Send the open attempt's offer to the coordinator.
    fn negotiate(&mut self, offer_sdp: String) -> Vec<CarrierEffect> {
        let Some(attempt) = self.attempt.clone() else {
            return Vec::new();
        };
        self.set_phase(PeerPhase::Negotiating, None);
        vec![CarrierEffect::NegotiateOffer { attempt, offer_sdp }]
    }
}

/// Copy a grant's tuple into an attempt. The scope is the GRANT's, as v2 checks
/// a `Ready` against `options.grant.sessionIds`: the worker proves the scope it
/// was told to install, which is the mint's and not this page's demand.
fn adopt_grant_fields(attempt: &mut PeerAttempt, grant: &DirectGrant) {
    attempt.worker_epoch = grant.worker_epoch.clone();
    attempt.grant_id = grant.grant_id.clone();
    attempt.tab_id = grant.tab_id.clone();
    attempt.device_fingerprint = grant.device_fingerprint.clone();
    attempt.session_ids = grant.session_ids.clone();
}
