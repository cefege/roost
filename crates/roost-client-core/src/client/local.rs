//! The local terminal loopback: a browser on a worker's own machine talking to
//! that worker directly, with the coordinator off the data path. This module is
//! the subtree's VOCABULARY; each submodule below owns one rule set.
//!
//! One subtree, four questions, asked in this order. `bootstrap` asks the
//! serving origin who it is; `discovery` decides which worker's door this browser
//! can reach; `grants` decides whether the coordinator has authorized the
//! sessions in hand; `door` turns an origin plus a grant into an admitted
//! connection; and `outbound` decides what is pushed out over it and over Sync.
//! The types here are what those five speak: the grant exchange, the session
//! facts a mint is decided from, and the refreshed-grant publication a host has
//! to be told about. Splitting them from the rules is this repo's module shape —
//! `terminal::input` and `terminal::routes` both keep their values at the root
//! and their state machines in a submodule.
//!
//! It composes four owners and re-implements none of them. The Sync socket is
//! `client::sync` and `sync::SyncState`; the direct-carrier table and the
//! promotion rules are `terminal::routes`; the admitted batch, the hold, and the
//! never-replay rule are `terminal::input`; the coordinator call and the close
//! code are `client::rpc` and `client::sync::close`. Every I/O is a value in and
//! a value out, with `now_ms` passed down rather than read, so the whole subtree
//! is provable with no browser and no clock.
//!
//! What it does NOT own: the attachment carrier, a different direct path with its
//! own packets and peer negotiation; and the socket. A host opens one per the URL
//! `door` names and reports what it sees as the frames `door::admit_ready` judges.

pub mod bootstrap;
pub mod discovery;
pub mod door;
pub mod grants;
pub mod outbound;

use std::collections::{BTreeMap, BTreeSet};

pub use door::{GrantRefusal, GrantSecret};

/// One coordinator-minted grant, and everything it authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTerminalGrant {
    /// The one worker this grant may reach.
    pub worker_fp: String,
    pub grant_id: String,
    /// The bearer secret. Never rendered by `Debug`.
    pub secret: GrantSecret,
    /// The exact sessions this grant names. Never "all".
    pub session_ids: BTreeSet<String>,
    pub tab_id: String,
    pub device_fingerprint: String,
    /// The worker's process epoch, empty when the worker reports none.
    pub worker_epoch: String,
    pub peer_supported: bool,
    /// STUN servers, non-empty only when `peer_supported`.
    pub stun_urls: Vec<String>,
    pub input_route_supported: bool,
    /// When the worker stops accepting this grant, from the coordinator's TTL.
    pub expires_at_ms: u64,
}

impl LocalTerminalGrant {
    /// Build a grant from a coordinator answer, or refuse the answer.
    ///
    /// An answer with no grant id or no secret is a REFUSAL, not a pending state.
    pub fn from_answer(
        answer: GrantMintAnswer,
        worker_fp: impl Into<String>,
        session_ids: BTreeSet<String>,
        tab_id: impl Into<String>,
        device_fingerprint: impl Into<String>,
        now_ms: u64,
    ) -> Option<Self> {
        if answer.grant_id.is_empty() || answer.secret.is_empty() {
            return None;
        }
        let (peer_supported, stun_urls) = (answer.peer_supported, answer.stun_urls);
        Some(Self {
            worker_fp: worker_fp.into(),
            grant_id: answer.grant_id,
            secret: GrantSecret::new(answer.secret),
            session_ids,
            tab_id: tab_id.into(),
            device_fingerprint: device_fingerprint.into(),
            worker_epoch: answer.worker_epoch,
            peer_supported,
            stun_urls: if peer_supported {
                stun_urls
            } else {
                Vec::new()
            },
            input_route_supported: answer.input_route_supported,
            expires_at_ms: now_ms.saturating_add(answer.ttl_ms),
        })
    }

    /// Whether `now_ms` is still inside this grant's lifetime. Strictly less
    /// than the deadline: at the deadline the worker has stopped accepting it.
    pub fn is_live(&self, now_ms: u64) -> bool {
        now_ms < self.expires_at_ms
    }

    /// Whether this grant still authorizes `session_id` at `now_ms`. The two
    /// rules are independent, which is why a test breaks them one at a time.
    pub fn admits(&self, session_id: &str, now_ms: u64) -> Result<(), GrantRefusal> {
        if !self.is_live(now_ms) {
            return Err(GrantRefusal::Expired);
        }
        if !self.session_ids.contains(session_id) {
            return Err(GrantRefusal::OutOfScope);
        }
        Ok(())
    }
}

/// What to ask the coordinator for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantMintRequest {
    /// The one worker this grant may reach.
    pub worker_fp: String,
    /// The exact sessions, sorted, capped at the per-grant maximum.
    pub session_ids: Vec<String>,
    /// The tab the grant is bound to.
    pub tab_id: String,
}

/// What the coordinator answers to a mint request.
///
/// An empty `grant_id` or `secret` is a REFUSAL, not a pending state: only a
/// worker's acknowledgement reveals the secret, so an answer without one means
/// nothing was installed anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantMintAnswer {
    /// The grant's identity.
    pub grant_id: String,
    /// The bearer secret, which is never rendered once wrapped.
    pub secret: String,
    /// How long the worker will accept it, counted from when it arrives.
    pub ttl_ms: u64,
    /// The worker's process epoch, empty when the worker reports none.
    pub worker_epoch: String,
    /// Whether this worker can negotiate a WebRTC peer at all.
    pub peer_supported: bool,
    /// STUN servers to try. Meaningless unless `peer_supported`.
    pub stun_urls: Vec<String>,
    /// Whether the worker implements `terminal-input-route-v1`.
    pub input_route_supported: bool,
}

/// Why a mint was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRefreshReason {
    /// A view started wanting a session this worker owns.
    DemandAdded,
    /// The renewal interval fired.
    Renewal,
    /// Sync came back up, so what the coordinator refused while it was down is
    /// worth asking for again.
    SyncConnected,
    /// A door was discovered, so what Sync carries can be carried directly.
    DoorDiscovered,
    /// A peer attempt failed and a new attempt is starting.
    PeerRetry,
}

/// What one refresh decided. A host performs the mint and reports the answer
/// back, which answers in the same vocabulary, so a second mint can be owed from
/// inside the first one's completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantRefresh {
    /// Ask the coordinator for exactly this, now.
    Mint(GrantMintRequest),
    /// Nothing to ask for; this is the grant that stands, or `None` if none does.
    Standing(Option<LocalTerminalGrant>),
    /// A mint completed and was installed.
    Installed(LocalTerminalGrant),
    /// A mint completed and was thrown away: the auth generation moved, the
    /// worker was retired, or the demand it was for is gone.
    Discarded,
}

/// What a host should now believe about one worker.
///
/// `None` is a real answer, not an absent one: a consumer has to be told that a
/// worker's grant is GONE, or it keeps dialling with a credential the worker has
/// already revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantPublication {
    /// The worker this is about.
    pub worker_fp: String,
    /// Its grant, or `None` when there is none any more.
    pub grant: Option<LocalTerminalGrant>,
}

/// The two facts a mint needs about a session — two, because those are the only
/// two its rule reads: a closed session and a session on another worker are both
/// reasons to leave it out of the grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSessionFact {
    /// The worker that owns the session.
    pub worker_fp: String,
    /// Whether the session's PTY is open.
    pub open: bool,
}

/// Where a grant owner reads a session from, so the core reads its own store and
/// a test reads a table, and neither grows a second session row.
pub trait GrantSessionLookup {
    /// The facts about one session, or `None` when there is no such session.
    fn grant_session(&self, session_id: &str) -> Option<GrantSessionFact>;
}

impl GrantSessionLookup for BTreeMap<String, GrantSessionFact> {
    fn grant_session(&self, session_id: &str) -> Option<GrantSessionFact> {
        self.get(session_id).cloned()
    }
}

pub use bootstrap::{BootstrapRefusal, LocalBootstrap, coordinator_base, coordinator_base_url};
pub use discovery::{
    BrowserEnvironment, DoorAbsence, DoorAdoption, DoorDiscovery, DoorPlan, LocalWorkerDoor,
};
pub use door::{
    LOCAL_TERMINAL_PATH, LOCAL_TERMINAL_SUBPROTOCOL, LoopbackAdmission, LoopbackReady,
    ReadyRefusal, SecretUseLedger, admit_ready, local_terminal_url, redial_delay_ms,
};
pub use grants::GrantOwner;
pub use outbound::{InputDestination, RouteClaims, SyncTerminalState, ready_sync_destination};
