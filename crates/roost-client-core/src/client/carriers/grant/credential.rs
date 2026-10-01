//! The credential itself: the fields a coordinator authorized, whether it is
//! still usable, and the conversion from the coordinator's own answer shape.
//! Called by `grant` and by every caller that admits a carrier; depends on
//! `client::local::LocalTerminalGrant` and on the token's transport vocabulary.

use std::collections::BTreeSet;

use crate::terminal::token::TerminalTransport;

/// One coordinator-authorized, memory-only, worker-acknowledged credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectGrant {
    /// The coordinator's id for this grant.
    pub grant_id: String,
    /// The secret. It exists only in this document's memory, which is why it is
    /// never written, never logged, and never part of a connection id.
    pub secret: String,
    /// The worker whose loopback door or peer this grant opens.
    pub worker_fp: String,
    /// The worker PROCESS the grant was minted against. Empty means the worker
    /// reported none, and a grant without one cannot open a peer, because the
    /// offer is bound to a process epoch.
    pub worker_epoch: String,
    /// The tab the grant names.
    pub tab_id: String,
    /// The device the grant names.
    pub device_fingerprint: String,
    /// The exact sessions the grant admits. Never widened after admission.
    pub session_ids: BTreeSet<String>,
    /// Whether the worker offers the WebRTC peer carrier at all.
    pub peer_supported: bool,
    /// Whether the worker implements `terminal-input-route-v1`.
    pub input_route_supported: bool,
    /// Opportunistic address discovery only. Empty disables it, and it is never
    /// a relay guarantee.
    pub stun_urls: Vec<String>,
    /// The host's clock value at which this grant is dead. Zero is refused: an
    /// unbounded credential is not a time-bounded one.
    pub expires_at_ms: u64,
}

impl DirectGrant {
    /// Whether the grant's own deadline has passed.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms != 0 && now_ms >= self.expires_at_ms
    }

    /// Whether this grant can open a carrier for `transport`.
    ///
    /// The peer path additionally requires a worker process epoch, because the
    /// coordinator binds the offer to one
    /// (`protocol/spec/direct-terminal.md:25`).
    pub fn admits(&self, transport: TerminalTransport) -> bool {
        if self.grant_id.is_empty() || self.secret.is_empty() || self.worker_fp.is_empty() {
            return false;
        }
        match transport {
            TerminalTransport::Loopback => true,
            TerminalTransport::Peer => self.peer_supported && !self.worker_epoch.is_empty(),
            TerminalTransport::Sync => false,
        }
    }

    /// The same credential as `crate::client::local::LocalTerminalGrant`, in
    /// the shape the election reads.
    ///
    /// A CONVERSION and not a second answer: both are built from one
    /// coordinator reply, and deriving this one from the other is what stops
    /// the loopback carrier and the peer machine from disagreeing about which
    /// worker epoch, scope, or deadline a secret was minted under.
    pub fn from_local(grant: &crate::client::local::LocalTerminalGrant) -> Self {
        Self {
            grant_id: grant.grant_id.clone(),
            secret: grant.secret.expose().to_owned(),
            worker_fp: grant.worker_fp.clone(),
            worker_epoch: grant.worker_epoch.clone(),
            tab_id: grant.tab_id.clone(),
            device_fingerprint: grant.device_fingerprint.clone(),
            session_ids: grant.session_ids.clone(),
            peer_supported: grant.peer_supported,
            input_route_supported: grant.input_route_supported,
            stun_urls: grant.stun_urls.clone(),
            expires_at_ms: grant.expires_at_ms,
        }
    }
}
