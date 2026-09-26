//! The one-time tokens that bind an authoritative session list to the live
//! Sync v2 socket that asked for it.
//!
//! Owned by the Sync link, and shared by every socket: the binding is keyed by
//! socket id because that is what `domain_ready` presents, and it is process-wide
//! because the issuer is an RPC that does not hold the socket.
//!
//! WHY A TOKEN EXISTS AT ALL. `domain_ready` for the terminal domain is what
//! lets a socket start receiving cells, and cells for sessions the socket may
//! not observe are a data leak. The list of sessions it may observe is resolved
//! during upgrade and changes as sessions open and close, so the client cannot
//! simply assert which sessions it hydrated. Instead the client calls the
//! authoritative list RPC, the coordinator binds the result to the socket that
//! asked, and `domain_ready` consumes the binding exactly once
//! (`sync-ws-v2-commands.ts:115-147`).
//!
//! WHY THE TOKEN IS CONSUMED, NOT READ. A token that could be replayed would
//! re-open the fence the client already closed, re-admitting a session list from
//! a moment the client's own snapshot no longer matches. Consuming the whole
//! binding also means a second `domain_ready` with the same token finds nothing,
//! which resets the domain instead of silently re-admitting.

use std::collections::{BTreeMap, BTreeSet};

/// Tokens one socket may hold. A browser that hydrates repeatedly evicts its
/// oldest, so a socket cannot pin an unbounded list of session snapshots.
pub const MAX_TOKENS_PER_SOCKET: usize = 4;

/// One live socket's bound snapshots.
#[derive(Debug)]
struct SnapshotBinding {
    fingerprint: String,
    /// Identifies THIS registration. A late teardown from a socket that has
    /// already been replaced must not delete the live socket's binding.
    registration: u64,
    /// Insertion-ordered, because eviction is oldest-first.
    tokens: BTreeMap<String, BTreeSet<String>>,
    order: Vec<String>,
}

/// Every live Sync v2 socket's bound snapshots.
#[derive(Debug, Default)]
pub struct SnapshotTokenRegistry {
    sockets: BTreeMap<String, SnapshotBinding>,
    next_registration: u64,
}

impl SnapshotTokenRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one live socket and return its registration handle.
    ///
    /// A re-registration of the same socket id replaces the binding, which is
    /// what a socket that reconnected under a new identity must get, and the
    /// handle it returns is what makes the OLD socket's eventual teardown a
    /// no-op.
    pub fn register_socket(&mut self, socket_id: &str, fingerprint: &str) -> u64 {
        self.next_registration += 1;
        let registration = self.next_registration;
        self.sockets.insert(
            socket_id.to_owned(),
            SnapshotBinding {
                fingerprint: fingerprint.to_owned(),
                registration,
                tokens: BTreeMap::new(),
                order: Vec::new(),
            },
        );
        registration
    }

    /// Unregister one socket, if the handle is still the live registration.
    ///
    /// Identity-safe on purpose: a socket that closes twice, or a teardown that
    /// races a re-registration under the same id, must not delete the binding
    /// the live socket will consume its token from
    /// (`sync-snapshot-registry.ts:16-27`).
    pub fn unregister_socket(&mut self, socket_id: &str, registration: u64) -> bool {
        if self
            .sockets
            .get(socket_id)
            .is_none_or(|binding| binding.registration != registration)
        {
            return false;
        }
        self.sockets.remove(socket_id);
        true
    }
    /// Bind an authoritative session list to the socket that requested it.
    ///
    /// The token is supplied by the caller because the id namespace belongs to
    /// the RPC that issued the snapshot, and this registry does not mint ids.
    /// Returns `false` when the socket is not registered or was registered for a
    /// different fingerprint, which a caller must not ignore: it means the socket
    /// is gone.
    pub fn bind(
        &mut self,
        socket_id: &str,
        fingerprint: &str,
        token: &str,
        session_ids: BTreeSet<String>,
    ) -> bool {
        let Some(binding) = self.sockets.get_mut(socket_id) else {
            return false;
        };
        if binding.fingerprint != fingerprint {
            return false;
        }
        if binding
            .tokens
            .insert(token.to_owned(), session_ids)
            .is_none()
        {
            binding.order.push(token.to_owned());
        }
        while binding.order.len() > MAX_TOKENS_PER_SOCKET {
            let oldest = binding.order.remove(0);
            binding.tokens.remove(&oldest);
        }
        true
    }

    /// Consume one token exactly once, returning the sessions it covered.
    ///
    /// The whole binding is cleared, not just the token: the bound list is the
    /// client's answer to "what may I observe", and a second hydration must
    /// present a second list rather than reuse the first.
    pub fn consume(&mut self, socket_id: &str, token: &str) -> Option<BTreeSet<String>> {
        let binding = self.sockets.get_mut(socket_id)?;
        let session_ids = binding.tokens.remove(token)?;
        binding.tokens.clear();
        binding.order.clear();
        Some(session_ids)
    }

    /// How many tokens one socket holds, for a test and a diagnostic.
    #[must_use]
    pub fn token_count(&self, socket_id: &str) -> usize {
        self.sockets
            .get(socket_id)
            .map_or(0, |binding| binding.tokens.len())
    }
}
