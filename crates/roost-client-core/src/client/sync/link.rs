//! The socket the host has installed, and the two moments its frames may be used.
//!
//! The store's `SyncLink` begins at the coordinator's announcement; this record
//! begins at the browser's `open` event, which is earlier. Both hold a
//! generation and neither is derived from the other, because the transport has to
//! recognise its own socket before the coordinator has said anything about it —
//! and a host that reused the store's link for that would find it empty at
//! exactly the moment it needed an answer.
//!
//! Ported from `apps/web/src/store/sync.ts:170-264`, whose two gates
//! (`canOpenSyncLink`, `canAcceptSyncLink`, `sync-flow.ts:18-36`) are these two
//! predicates. Identity becomes a generation rather than an object reference,
//! because a Rust host holds one socket handle at a time and the handle it
//! replaced is already gone.

use crate::client::sync::abort::AbortReason;

/// The socket the host currently has installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InstalledLink {
    generation: Option<u64>,
    open: bool,
    accepting: bool,
    abort_reason: Option<AbortReason>,
}

impl InstalledLink {
    /// No socket installed.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Record the socket a dial installed. Not yet open: a dial that never
    /// completes must not be reported as a link.
    pub fn dialled(&mut self, generation: u64) {
        self.generation = Some(generation);
        self.open = false;
        self.accepting = false;
        self.abort_reason = None;
    }

    /// The browser reported the handshake complete. Returns whether this is the
    /// socket that was dialled; a `false` here is a socket the host has already
    /// replaced, and adopting it would install a generation the store has
    /// already retired.
    pub fn opened(&mut self, generation: u64) -> bool {
        if self.generation != Some(generation) {
            return false;
        }
        self.open = true;
        self.accepting = true;
        true
    }

    /// Stop accepting, then record why this client is retiring it.
    ///
    /// The order is the contract and matches the platform socket's
    /// (`platform/sync_socket.rs:209-212`): a frame already in the browser's
    /// queue between the two states would otherwise be applied to a generation
    /// this side has already given up.
    pub fn retire(&mut self, generation: u64, reason: AbortReason) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.accepting = false;
        self.abort_reason = Some(reason);
        self
    }

    /// The socket is gone without a decision of this client's — a peer close, or
    /// a peer that vanished. The abort reason stays whatever it was, so a link
    /// already retired for a reason keeps it.
    pub fn closed(&mut self, generation: u64) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.open = false;
        self.accepting = false;
        self
    }

    /// The generation of the installed socket.
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }

    /// Whether `generation` names the installed socket.
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == Some(generation)
    }

    /// Whether the installed socket is still taking frames.
    pub fn is_accepting(&self) -> bool {
        self.accepting
    }

    /// Whether the installed socket's handshake completed.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Why this client retired the installed socket, if it did.
    pub fn abort_reason(&self) -> Option<AbortReason> {
        self.abort_reason
    }
}

/// Whether a socket that has just completed its handshake may be adopted.
///
/// v2's `canOpenSyncLink`: same link, no abort reason recorded, and open. Note
/// that `accepting` is deliberately NOT required — a link becomes accepting
/// because this gate passed, so requiring it would make adoption impossible.
pub fn can_open_sync_link(link: &InstalledLink, generation: u64) -> bool {
    link.is_current(generation) && link.is_open() && link.abort_reason.is_none()
}

/// Whether a frame observed on the installed socket may be reported to the core.
///
/// v2's `canAcceptSyncLink`: same link, accepting, and open. This is the
/// transport's gate and it is not the store's — `SyncState::accepts` answers
/// whether the store will act on the frame, and the two disagree on purpose
/// during the window between the browser's `open` and the coordinator's
/// announcement, where this one says yes and the other says not yet.
pub fn can_accept_sync_link(link: &InstalledLink, generation: u64) -> bool {
    link.is_current(generation) && link.is_accepting() && link.is_open()
}
