//! The local door: the URL a discovered origin is dialled at, what a refusal
//! looks like, the credential a dial spends, and the handshake a loopback socket
//! must pass before it is admitted. No sockets — a host opens one for the URL
//! named here and reports what it sees as the `Ready` judged below.
//!
//! The handshake rules are the point. A connection is admitted ONLY after the
//! worker has revalidated the tuple against the live grant, so this is where a
//! worker's answer is checked against the exact grant that asked for it: same
//! worker, no peer id, every named session granted, and a worker epoch that does
//! not contradict the grant's. A worker answering without an epoch AND without a
//! socket id is a ROLLING one and gets a fresh per-connection namespace.
//!
//! Ported from `apps/web/src/store/transport/local-terminal.ts:98-217`. The
//! worker end is `crates/roost-worker/src/local_door.rs`, which REPLACES a
//! socket presented a secret it already accepted rather than multiplying sinks;
//! [`SecretUseLedger`] is the client half of that same rule.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::client::local::LocalTerminalGrant;
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// The path a worker's local UI door upgrades for a terminal socket.
pub const LOCAL_TERMINAL_PATH: &str = "/ws/local-terminal";

/// The subprotocol every loopback terminal socket is opened with.
pub const LOCAL_TERMINAL_SUBPROTOCOL: &str = "roost-local-terminal";

/// The first redial, before any streak is established.
pub const REDIAL_BASE_MS: u64 = 500;

/// The ceiling on the redial ladder. Bounded because the door is on this
/// machine: a page that keeps hammering a worker which is not running spends the
/// battery of the laptop it is on.
pub const REDIAL_MAX_MS: u64 = 8_000;

/// How long one input-route claim may wait for its answer. A property of this
/// handshake, spent by `client::local::outbound`, which owns the claim waiters.
pub const ROUTE_CLAIM_TIMEOUT_MS: u64 = 8_000;

/// A grant secret, which is a bearer credential for a worker's PTYs.
///
/// `Debug` renders `redacted` and nothing else. A grant is logged, traced and
/// propagated through `?` more freely than any other value in a client, and the
/// only defence against one of those reaching a credential is a type that cannot
/// be printed by accident — the same rule, for the same reason, as the CLI's
/// `OneShotGrant` (`crates/roost-cli/src/quickstart/grant.rs`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GrantSecret(String);

impl GrantSecret {
    /// Wrap a secret the coordinator returned.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the value. The only way out, so a caller putting it on a socket is
    /// visible at the call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GrantSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("redacted")
    }
}

/// Why a grant did not authorize something. Each member is one rule, checked in
/// the order it is listed, so a host's log says which one fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRefusal {
    /// No grant has ever been minted for this worker.
    NoGrant,
    /// The coordinator confirmed this worker is gone; nothing mints for it until
    /// the auth boundary resets.
    WorkerRetired,
    /// The grant's own lifetime has run out, so this refuses a window the far
    /// end has already closed.
    Expired,
    /// The grant does not name this session.
    OutOfScope,
    /// The session's PTY is not open.
    SessionNotOpen,
    /// The session belongs to another worker, so this grant can never name it.
    SessionOnAnotherWorker,
    /// This secret already holds a live loopback socket in this document.
    SecretInUse,
}

impl GrantRefusal {
    /// The string a host records.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NoGrant => "no direct-terminal grant has been minted",
            Self::WorkerRetired => "this worker's direct-terminal grant was retired",
            Self::Expired => "the direct-terminal grant expired",
            Self::OutOfScope => "the direct-terminal grant does not name this session",
            Self::SessionNotOpen => "the session is not open",
            Self::SessionOnAnotherWorker => "the session belongs to another worker",
            Self::SecretInUse => "this grant secret already holds a live loopback socket",
        }
    }
}

/// Which grant secrets currently hold a live loopback socket in this document.
///
/// The worker's door answers a second socket presenting the same secret by
/// REPLACING the first, because a worker that multiplied them would be one
/// replayed credential away from a second authority over the same PTYs. That
/// answer is right for the worker and useless to the client, which would quietly
/// lose the carrier it already had — so the second dial is refused here, where
/// the caller can mint a fresh grant instead.
#[derive(Debug, Default)]
pub struct SecretUseLedger {
    live: BTreeMap<GrantSecret, String>,
}

impl SecretUseLedger {
    /// A ledger holding nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a secret for one socket, or refuse it.
    pub fn claim(&mut self, grant: &LocalTerminalGrant) -> Result<(), GrantRefusal> {
        if self.live.contains_key(&grant.secret) {
            return Err(GrantRefusal::SecretInUse);
        }
        self.live
            .insert(grant.secret.clone(), grant.worker_fp.clone());
        Ok(())
    }

    /// Give a secret back, so a redial of the same carrier may use it again.
    pub fn release(&mut self, secret: &GrantSecret) {
        self.live.remove(secret);
    }

    /// Give back every secret of one worker, when its grants are retired.
    pub fn release_worker(&mut self, worker_fp: &str) {
        self.live.retain(|_, holder| holder != worker_fp);
    }

    /// Whether this secret is already holding a socket.
    pub fn holds(&self, secret: &GrantSecret) -> bool {
        self.live.contains_key(secret)
    }
}

/// The authority of a BARE `http` or `https` origin, or `None`.
///
/// Bare means what a browser's `URL.origin` means: scheme, host, and an optional
/// port, and nothing else. An operator override carrying a path, a query, a
/// trailing slash or any other scheme is IGNORED rather than dialed, and it is
/// the same rule the probe URL is built from, so the origin chosen and the one
/// dialled cannot disagree.
pub fn http_origin_authority(origin: &str) -> Option<String> {
    let authority = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    if authority.is_empty() || authority.contains(['/', '?', '#', ' ', '\\', '@']) {
        return None;
    }
    Some(authority.to_string())
}

/// The socket URL a door origin is dialled at, over the matching scheme. A
/// coordinator behind a TLS-terminating front door is reached as `wss` even when
/// an operator wrote the override down as `http`, so the scheme is read off the
/// origin and never assumed.
pub fn local_terminal_url(origin: &str) -> Result<String, DialRefusal> {
    let authority = http_origin_authority(origin).ok_or(DialRefusal::UnusableOrigin {
        origin: origin.to_string(),
    })?;
    let scheme = if origin.starts_with("https://") {
        "wss"
    } else {
        "ws"
    };
    Ok(format!("{scheme}://{authority}{LOCAL_TERMINAL_PATH}"))
}

/// The delay before redial `attempt` (0-based), with equal jitter.
///
/// `jitter_sample` is a value in `0..1000` the caller draws from its own
/// randomness. A worker that restarts leaves every open tab redialling at the
/// same instant, and a deterministic ladder brings them all back in one wave.
pub fn redial_delay_ms(attempt: u32, jitter_sample: u32) -> u64 {
    let ceiling = redial_ceiling_ms(attempt);
    let sample = u64::from(jitter_sample.min(999));
    ceiling / 2 + (ceiling / 2) * sample / 1_000
}

/// The un-jittered delay before redial `attempt`, saturating at [`REDIAL_MAX_MS`].
///
/// Saturating on the shift: a tab left open against a worker that stays down must
/// not wrap into a SHORT delay, the one thing a backoff must never do.
pub fn redial_ceiling_ms(attempt: u32) -> u64 {
    REDIAL_BASE_MS
        .saturating_mul(1u64 << attempt.min(16))
        .min(REDIAL_MAX_MS)
}

/// Why a door could not be dialled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialRefusal {
    /// No door has been discovered for this page.
    NoDoor,
    /// The origin is not a bare `http`/`https` origin, so there is nothing to dial.
    UnusableOrigin { origin: String },
    /// A carrier is already open on this door, so the host has nothing to do.
    AlreadyDialled,
    /// The secret behind this grant already holds a live socket, and the
    /// worker's door would REPLACE it: one secret, one socket.
    SecretInUse,
}

impl DialRefusal {
    /// The string a host records, and the reason it passes to `close`.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoDoor => "no local worker door is reachable",
            Self::UnusableOrigin { .. } => "local worker door origin is not a bare http origin",
            Self::AlreadyDialled => "a loopback carrier is already open on this door",
            Self::SecretInUse => "this grant secret already holds a live loopback socket",
        }
    }
}

/// The `Ready` a worker sends once it has revalidated the presented tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackReady {
    /// The worker that answered. Must equal the door's worker.
    pub worker_fingerprint: String,
    /// The sessions the worker admitted. Every one must be named by the grant.
    pub session_ids: BTreeSet<String>,
    /// The worker's own generation for this socket. Never zero.
    pub socket_generation: u64,
    /// Empty when the worker does not report one.
    pub worker_epoch: String,
    /// Empty when the worker does not report one.
    pub socket_id: String,
    /// Always empty on loopback; a non-empty one is a PEER answer on the wrong
    /// path.
    pub peer_id: String,
}

impl LoopbackReady {
    /// Whether this is a worker old enough to report neither an epoch nor a
    /// socket id.
    ///
    /// BOTH or NEITHER: one of the two without the other is a half-present tuple,
    /// and treating that as a rolling worker would mint a namespace for a worker
    /// that has an identity this client cannot match.
    pub fn is_rolling(&self) -> bool {
        self.worker_epoch.is_empty() && self.socket_id.is_empty() && self.peer_id.is_empty()
    }
}

/// Why a `Ready` was refused. Each member is one rule, so a host's log says
/// which one the worker broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyRefusal {
    /// The grant was minted for another worker than the door it was presented to.
    GrantNamesAnotherWorker,
    /// The worker that answered is not the worker this door is.
    WorkerMismatch,
    /// The answer carried a peer id, which is a peer handshake on the loopback.
    PeerIdPresent,
    /// The generation is zero, which `RouteRegistry::register` refuses too.
    ///
    /// Refused HERE as well so the host can still redial, rather than opening a
    /// socket for a carrier the registry will turn away.
    ZeroGeneration,
    /// The worker admitted a session the grant does not name.
    SessionNotGranted,
    /// The worker reported an epoch that contradicts the grant's.
    EpochMismatch,
    /// The worker reported an epoch or a socket id, but not both.
    HalfPresentTuple,
}

impl ReadyRefusal {
    /// The string a host records, and the reason it passes to `close`.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::GrantNamesAnotherWorker => "local terminal grant names another worker",
            Self::WorkerMismatch => "local terminal Ready did not match its authenticated grant",
            Self::PeerIdPresent => "local terminal Ready carried a peer id",
            Self::ZeroGeneration => "local terminal Ready carried generation zero",
            Self::SessionNotGranted => "local terminal Ready admitted an ungranted session",
            Self::EpochMismatch => "local terminal Ready epoch contradicts the grant",
            Self::HalfPresentTuple => "local terminal Ready carried a half-present rolling tuple",
        }
    }
}

/// A loopback connection that passed the handshake, and what it may carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackAdmission {
    /// The generation this carrier presents. Registering it is the host's next
    /// step, and only now.
    pub token: TerminalToken,
    /// The sessions the worker admitted. A refreshed grant may grow this, never
    /// shrink it.
    pub ready_sessions: BTreeSet<String>,
    /// Whether input-route claims are available. False on a rolling worker, which
    /// reports no epoch to claim against.
    pub input_route_supported: bool,
}

impl LoopbackAdmission {
    /// Whether this connection may carry a session.
    pub fn allows_session(&self, session_id: &str) -> bool {
        self.ready_sessions.contains(session_id)
    }

    /// Widen this connection to a refreshed grant, or refuse it.
    ///
    /// A grant may only ADD. Narrowing is refused, because the sessions the
    /// worker has already admitted are live PTYs whose bytes are in flight, and
    /// a narrowed scope would strand them; a contradicting epoch is refused for
    /// the same reason `admit_ready` refuses one.
    pub fn extend_grant(&mut self, grant: &LocalTerminalGrant) -> bool {
        if self.token.worker_fp.as_deref() != Some(grant.worker_fp.as_str()) {
            return false;
        }
        if !grant.worker_epoch.is_empty() && grant.worker_epoch != self.token.process_epoch {
            return false;
        }
        if !self
            .ready_sessions
            .iter()
            .all(|session_id| grant.session_ids.contains(session_id))
        {
            return false;
        }
        self.ready_sessions
            .extend(grant.session_ids.iter().cloned());
        self.input_route_supported &= grant.input_route_supported;
        true
    }
}

/// Admit one `Ready` against the grant that asked for it.
///
/// `connection_id` is the host's own name for this socket, and is the namespace
/// a ROLLING worker gets: such a worker has no identity of its own, so a
/// per-connection value is the only thing that can fence a redial's frames from
/// the generation they replaced.
pub fn admit_ready(
    grant: &LocalTerminalGrant,
    door_worker_fp: &str,
    connection_id: &str,
    ready: &LoopbackReady,
) -> Result<LoopbackAdmission, ReadyRefusal> {
    if grant.worker_fp != door_worker_fp {
        return Err(ReadyRefusal::GrantNamesAnotherWorker);
    }
    if ready.worker_fingerprint != door_worker_fp {
        return Err(ReadyRefusal::WorkerMismatch);
    }
    if !ready.peer_id.is_empty() {
        return Err(ReadyRefusal::PeerIdPresent);
    }
    if ready.socket_generation == 0 {
        return Err(ReadyRefusal::ZeroGeneration);
    }
    if !ready.session_ids.is_subset(&grant.session_ids) {
        return Err(ReadyRefusal::SessionNotGranted);
    }
    let rolling = ready.is_rolling();
    if !rolling && (ready.worker_epoch.is_empty() || ready.socket_id.is_empty()) {
        return Err(ReadyRefusal::HalfPresentTuple);
    }
    if !grant.worker_epoch.is_empty()
        && !ready.worker_epoch.is_empty()
        && grant.worker_epoch != ready.worker_epoch
    {
        return Err(ReadyRefusal::EpochMismatch);
    }
    Ok(LoopbackAdmission {
        token: TerminalToken::direct(
            ready.socket_generation,
            TerminalTransport::Loopback,
            door_worker_fp,
            if rolling {
                connection_id
            } else {
                ready.worker_epoch.as_str()
            },
            ready.socket_generation,
        ),
        ready_sessions: ready.session_ids.clone(),
        input_route_supported: !rolling && grant.input_route_supported,
    })
}
