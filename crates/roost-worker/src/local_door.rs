//! Loopback sockets before and after an authenticated terminal Hello, and the
//! grants that authenticate them. Owned by the worker.
//!
//! The door is reachable from anything on the machine, so everything here is
//! about what an unauthenticated peer can hold open. Two rules do the work:
//! an unauthenticated socket is EXPIRED rather than left to wait, and
//! replaying a grant REPLACES its prior socket rather than multiplying sinks.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long an unauthenticated local socket may wait for its Hello.
///
/// Three seconds. Long enough for a browser to send one, short enough that a
/// peer which opens sockets and says nothing cannot hold them.
pub const PREHELLO_DEADLINE: Duration = Duration::from_secs(3);

/// How many sockets may be established against this worker at once, whether
/// waiting for a Hello or already authenticated.
///
/// Bounded because the door is local: an unbounded one is a loopback peer
/// opening sockets until the worker runs out of memory, and the answer must
/// not require the peer to authenticate first.
pub const MAX_ESTABLISHED: usize = 32;

/// The identity of a socket for the lifetime of its connection.
pub type SocketId = u64;

/// What authenticating a socket against a grant did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authenticated {
    /// The socket is authenticated and may carry terminal frames.
    pub socket_id: SocketId,
    /// The socket this one REPLACED, if any.
    ///
    /// Reported rather than silently done, because the caller has to close it.
    /// A grant replayed on a new connection is the normal case for a device
    /// that reconnected, and the prior socket is a leak if nobody closes it.
    pub replaced: Option<SocketId>,
}

/// Why a socket was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Already waiting for a Hello on this socket.
    AlreadyAdmitted,
    /// The worker is at its socket ceiling.
    AtCapacity,
    /// The grant is expired, or was never issued.
    UnknownGrant,
    /// The presented secret does not match the grant's digest.
    ///
    /// Reported distinctly from [`Refusal::UnknownGrant`] so a caller can tell a
    /// stale authorization from a wrong one, and log them differently.
    BadSecret,
}

/// What authenticating a socket did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authentication {
    Authenticated(Authenticated),
    Refused(Refusal),
}

/// A grant, as the worker holds it.
///
/// The secret is a DIGEST. The coordinator installs digests and the worker can
/// never read the secret back, so a bug in the worker cannot leak a credential
/// — the process simply does not have it.
#[derive(Debug, Clone)]
struct Grant {
    secret_digest: [u8; 32],
    expires_at: Instant,
}

/// Sockets waiting for a Hello, and grants already authenticated.
#[derive(Debug, Default)]
pub struct PreHelloOwner {
    waiting: HashMap<SocketId, Instant>,
    /// Grant id to the socket currently authenticated by it.
    authenticated_by_grant: HashMap<String, SocketId>,
    /// Socket to the grant that authenticated it, so a close can find its grant.
    grant_by_socket: HashMap<SocketId, String>,
    grants: HashMap<String, Grant>,
    next_socket_id: SocketId,
}

impl PreHelloOwner {
    pub fn new() -> Self {
        Self {
            waiting: HashMap::new(),
            authenticated_by_grant: HashMap::new(),
            grant_by_socket: HashMap::new(),
            grants: HashMap::new(),
            next_socket_id: 1,
        }
    }

    /// Mint a socket id. The owner owns socket identity so that a caller cannot
    /// present one it invented.
    pub fn next_socket_id(&mut self) -> SocketId {
        let id = self.next_socket_id;
        self.next_socket_id = self.next_socket_id.saturating_add(1);
        id
    }

    /// Admit a socket that has connected but not yet authenticated.
    pub fn admit(&mut self, socket_id: SocketId, now: Instant) -> Result<(), Refusal> {
        if self.waiting.contains_key(&socket_id) {
            return Err(Refusal::AlreadyAdmitted);
        }
        if self.established() >= MAX_ESTABLISHED {
            return Err(Refusal::AtCapacity);
        }
        self.waiting.insert(socket_id, now);
        Ok(())
    }

    /// Sockets this worker is holding, waiting or authenticated.
    pub fn established(&self) -> usize {
        self.waiting.len() + self.grant_by_socket.len()
    }

    /// Sockets whose Hello deadline has passed, and drops them from the wait
    /// set.
    ///
    /// The caller closes what this returns. An unauthenticated socket that is
    /// merely forgotten stays open, and a peer that opens sockets and never
    /// says anything would then hold them indefinitely.
    pub fn expire(&mut self, now: Instant) -> Vec<SocketId> {
        let expired: Vec<SocketId> = self
            .waiting
            .iter()
            .filter(|(_, since)| now.saturating_duration_since(**since) >= PREHELLO_DEADLINE)
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.waiting.remove(id);
        }
        expired
    }

    /// Install a grant, replacing any existing one with the same id.
    pub fn install_grant(
        &mut self,
        grant_id: impl Into<String>,
        secret_digest: [u8; 32],
        ttl: Duration,
        now: Instant,
    ) {
        self.grants.insert(
            grant_id.into(),
            Grant {
                secret_digest,
                expires_at: now + ttl,
            },
        );
    }

    /// Authenticate a socket with a grant and a secret.
    ///
    /// The secret is hashed and compared; it is never stored, so this function
    /// cannot leak it and neither can anything that reads this type.
    pub fn authenticate(
        &mut self,
        grant_id: &str,
        secret: &[u8],
        socket_id: SocketId,
        now: Instant,
    ) -> Authentication {
        let Some(grant) = self.grants.get(grant_id) else {
            return Authentication::Refused(Refusal::UnknownGrant);
        };
        if now >= grant.expires_at {
            // ACTIVE expiry: a connected carrier must not outlive its
            // authorization just because the coordinator is unreachable to
            // renew it.
            self.grants.remove(grant_id);
            return Authentication::Refused(Refusal::UnknownGrant);
        }
        if sha256_of(secret) != grant.secret_digest {
            return Authentication::Refused(Refusal::BadSecret);
        }

        // Replaying a grant REPLACES its socket. It does not add a second, and
        // that is the whole point: one grant is one terminal, and a second
        // socket would be a second sink for the same frames.
        let replaced = self
            .authenticated_by_grant
            .insert(grant_id.to_string(), socket_id);
        if let Some(prior) = replaced
            && prior != socket_id
        {
            self.grant_by_socket.remove(&prior);
        }
        self.waiting.remove(&socket_id);
        self.grant_by_socket.insert(socket_id, grant_id.to_string());
        Authentication::Authenticated(Authenticated {
            socket_id,
            replaced: replaced.filter(|prior| *prior != socket_id),
        })
    }

    /// Whether this socket is authenticated, and by which grant.
    pub fn grant_for(&self, socket_id: SocketId) -> Option<&str> {
        self.grant_by_socket.get(&socket_id).map(String::as_str)
    }

    /// A socket is gone. Its grant becomes available again, so a reconnecting
    /// device is not refused for a limit it is no longer occupying.
    pub fn close(&mut self, socket_id: SocketId) {
        self.waiting.remove(&socket_id);
        if let Some(grant_id) = self.grant_by_socket.remove(&socket_id)
            && self.authenticated_by_grant.get(&grant_id) == Some(&socket_id)
        {
            self.authenticated_by_grant.remove(&grant_id);
        }
    }

    /// Grants that have expired, dropped. Called on a tick; an expired grant
    /// nobody presents is still an entry this process is holding.
    pub fn expire_grants(&mut self, now: Instant) -> usize {
        let before = self.grants.len();
        self.grants.retain(|_, grant| now < grant.expires_at);
        before - self.grants.len()
    }
}

/// The grant digest. SHA-256, because a grant secret is a bearer credential
/// and a weak digest would make a stolen digest a usable credential.
///
/// Public so a caller can compute the digest to INSTALL, and so a test can check
/// it against a published value.
pub fn sha256_of(secret: &[u8]) -> [u8; 32] {
    // The digest is computed and compared, never stored. Implemented over
    // `sha2` so this is the real algorithm rather than a stand-in: a stand-in
    // here would make every test pass and every deployment insecure.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.finalize().into()
}
