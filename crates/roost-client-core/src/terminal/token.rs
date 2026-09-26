//! The exact identity of one carrier generation, and the comparison every
//! terminal rule is scoped to.
//!
//! A token is a socket generation, a worker process epoch, a domain generation,
//! a transport kind, and the worker fingerprint the route belongs to. Two frames
//! belong to the same session only when all five agree, and a repair requested
//! on one token is never sent on another.
//!
//! Ported from `apps/web/src/store/terminal-stream-types.ts:79-107`, which
//! builds the same six-part value and compares it the same way. `socket_id` is
//! deliberately NOT part of equality: a socket that redials is a new generation,
//! so including its id would make a reconnect look like the same link and let a
//! stale callback write into a fresh socket's authority.

/// Which carrier a frame arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TerminalTransport {
    /// The authenticated Sync socket. Always present, and the fallback when no
    /// direct carrier is elected.
    Sync,
    /// Same-worker loopback WebSocket.
    Loopback,
    /// An authenticated WebRTC peer.
    Peer,
}

impl TerminalTransport {
    /// The wire spelling, which is also what a route key is built from.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Loopback => "loopback",
            Self::Peer => "peer",
        }
    }
}

/// One carrier generation, by value.
///
/// `domain_generation` is `u64` because the wire field is an unsigned 64-bit
/// generation the coordinator advances per domain. It is compared, never
/// ordered, and never interpreted: a smaller value is not an older token, it is
/// a different one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerminalToken {
    /// How many Sync sockets this tab has dialled. Monotonic per client.
    pub socket_generation: u64,
    /// The coordinator's identity for the worker process behind this route. A
    /// restarted worker changes it, which is what fences input that was already
    /// in flight when the process went away.
    pub process_epoch: String,
    /// The coordinator's per-domain generation.
    pub domain_generation: u64,
    /// Which carrier this token names.
    pub transport: TerminalTransport,
    /// The worker the route belongs to. `None` on Sync, where the coordinator
    /// is the authority and no single worker owns the stream.
    pub worker_fp: Option<String>,
}

impl TerminalToken {
    /// The Sync token for a socket generation. Sync carries no worker, so a
    /// direct-carrier comparison can never match one.
    ///
    /// `socket_id` is taken and NOT stored: it changes on every redial, and
    /// including it in equality would make a reconnect look like the same link.
    /// The coordinator's socket identity is the ACK's business, and the sync state
    /// holds it.
    pub fn sync(
        socket_generation: u64,
        _socket_id: impl Into<String>,
        process_epoch: impl Into<String>,
        domain_generation: u64,
    ) -> Self {
        Self {
            socket_generation,
            process_epoch: process_epoch.into(),
            domain_generation,
            transport: TerminalTransport::Sync,
            worker_fp: None,
        }
    }

    /// A direct-carrier token for one worker.
    pub fn direct(
        socket_generation: u64,
        transport: TerminalTransport,
        worker_fp: impl Into<String>,
        process_epoch: impl Into<String>,
        domain_generation: u64,
    ) -> Self {
        Self {
            socket_generation,
            process_epoch: process_epoch.into(),
            domain_generation,
            transport,
            worker_fp: Some(worker_fp.into()),
        }
    }

    /// The key a latch, a sent command, or a retry timer is recorded under.
    ///
    /// A generation change must re-arm a resync that was never answered, so the
    /// recorded key has to change with the token — which is why it is built from
    /// every part rather than from a counter the core owns separately.
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.socket_generation,
            self.process_epoch,
            self.domain_generation,
            self.transport.as_str(),
            self.worker_fp.as_deref().unwrap_or("")
        )
    }

    /// True when this token is still the one a session is fenced to.
    pub fn matches(&self, current: Option<&TerminalToken>) -> bool {
        current.is_some_and(|other| other == self)
    }
}

/// True when two tokens are the same generation, treating `None` as "no
/// generation" rather than as a wildcard.
///
/// v2 spells this `terminalGenerationMatches`, which takes either a token or a
/// live Sync state and returns false for a null argument
/// (`apps/web/src/store/terminal-stream-liveness.ts:31-43`). The asymmetry
/// matters: a missing token must never match, or a frame arriving with no
/// admission would be treated as current.
pub fn token_matches(left: Option<&TerminalToken>, right: Option<&TerminalToken>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalToken, TerminalTransport, token_matches};

    fn sync() -> TerminalToken {
        TerminalToken::sync(3, "sock-1", "epoch-a", 7)
    }

    #[test]
    fn a_sync_token_carries_no_worker() {
        // A direct-carrier token names one worker and a Sync token names none, so
        // the two can never be confused for each other's route.
        assert!(sync().worker_fp.is_none());
        let direct = TerminalToken::direct(3, TerminalTransport::Loopback, "fp-1", "epoch-a", 7);
        assert_eq!(direct.worker_fp.as_deref(), Some("fp-1"));
        assert_ne!(sync(), direct);
    }

    #[test]
    fn every_stored_component_participates_in_equality() {
        let base = sync();
        let variants = [
            TerminalToken::sync(4, "sock-1", "epoch-a", 7),
            TerminalToken::sync(3, "sock-1", "epoch-b", 7),
            TerminalToken::sync(3, "sock-1", "epoch-a", 8),
            TerminalToken::direct(3, TerminalTransport::Peer, "fp-1", "epoch-a", 7),
        ];
        for variant in variants {
            assert_ne!(base, variant, "{variant:?} must not equal the base token");
        }
    }

    #[test]
    fn the_socket_id_is_deliberately_not_part_of_equality() {
        // The coordinator mints a new socket id on every redial, so including it
        // would make a reconnect look like the same link — and a frame from the
        // OLD socket would then be admitted into the authority the NEW socket
        // holds. The id is the ACK's business and `SyncState` keeps it.
        assert_eq!(
            TerminalToken::sync(3, "sock-1", "epoch-a", 7),
            TerminalToken::sync(3, "sock-2", "epoch-a", 7)
        );
    }

    #[test]
    fn the_key_changes_with_every_component() {
        // A latched resync is rate-limited per generation key, so a key that
        // ignored a component would suppress the first request on a new socket.
        let base = sync();
        let mut keys = std::collections::BTreeSet::new();
        keys.insert(base.key());
        keys.insert(TerminalToken::sync(4, "sock-1", "epoch-a", 7).key());
        keys.insert(TerminalToken::sync(3, "sock-1", "epoch-b", 7).key());
        keys.insert(TerminalToken::sync(3, "sock-1", "epoch-a", 8).key());
        keys.insert(
            TerminalToken::direct(3, TerminalTransport::Loopback, "fp-1", "epoch-a", 7).key(),
        );
        assert_eq!(keys.len(), 5);
    }

    #[test]
    fn an_absent_token_matches_nothing() {
        // Not even another absent token: "no generation" is not a wildcard, or a
        // frame that arrived with no admission would be treated as current.
        assert!(!token_matches(None, None));
        assert!(!token_matches(Some(&sync()), None));
        assert!(!token_matches(None, Some(&sync())));
        assert!(token_matches(Some(&sync()), Some(&sync())));
    }
}
