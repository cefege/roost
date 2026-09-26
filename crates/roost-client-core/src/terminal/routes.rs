//! Direct carriers, the routes elected between them, and the staged candidate a
//! promotion promotes from.
//!
//! The registry that owns these lives in `registry`. Split because the SHAPES
//! and the RULES are read for different reasons: a caller building a carrier
//! wants this file, and a caller asking "may this session promote?" wants the
//! registry's preconditions.
//!
//! Ported from `apps/web/src/store/terminal-stream-transport.ts`; every
//! precondition and its reason is in `docs/phase4-client-contract.md` §8.

pub mod registry;

pub use registry::RouteRegistry;

use std::collections::BTreeSet;

use crate::terminal::token::{TerminalToken, TerminalTransport};

/// One authenticated direct carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectCarrier {
    /// The host's own identity for this connection, so a retirement names ONE
    /// connection rather than every connection that ever existed.
    pub connection_id: String,
    /// Which worker it reaches.
    pub worker_fp: String,
    /// Loopback or WebRTC.
    pub transport: TerminalTransport,
    /// The generation it presents.
    pub token: TerminalToken,
    /// The sessions its grant admits, exact. A grant is scope-bound
    /// (`protocol/spec/direct-terminal.md:23`), so this is never "all".
    pub granted_sessions: BTreeSet<String>,
}

impl DirectCarrier {
    /// Whether this connection's grant still admits a session.
    pub fn allows_session(&self, session_id: &str) -> bool {
        self.granted_sessions.contains(session_id)
    }

    /// Whether `token` is one this connection could present: right kind, right
    /// worker, right process epoch.
    ///
    /// The process epoch is the part that matters after a worker restart. The
    /// socket generation and domain generation are deliberately NOT checked
    /// here — they belong to the socket, and a carrier outlives a redial.
    pub fn presents(&self, token: &TerminalToken) -> bool {
        token.transport == self.transport
            && token.worker_fp.as_deref() == Some(self.worker_fp.as_str())
            && token.process_epoch == self.token.process_epoch
    }
}

/// The elected route for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoute {
    /// The connection serving it.
    pub connection_id: String,
    /// The exact generation the route was committed on. A frame or a write on
    /// any other generation is not this route's.
    pub token: TerminalToken,
}

/// A staged candidate: one connection's replica for a session, folded SEPARATELY
/// from the session's canonical until it is promoted.
///
/// Separate folding is the rule, not an optimization
/// (`protocol/spec/direct-terminal.md:27`): a candidate with a half-built
/// baseline must not be able to paint, and a promotion must be able to swap a
/// complete grid in atomically.
/// The candidate is METADATA. The replica it describes lives in the registry,
/// keyed by session, because a `TerminalSession` owns a chunk assembler and
/// cannot be cloned — so a candidate that carried its own replica would force
/// either a `Clone` the assembler does not have, or a second copy of a grid that
/// has to stay one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionCandidate {
    /// The session being staged for.
    pub session_id: String,
    /// The connection staging it.
    pub connection_id: String,
    /// The generation the candidate's replica was folded on.
    pub token: TerminalToken,
    /// The attempt this preparation belongs to. A promotion is refused when the
    /// attempt has moved on, so a slow fold cannot overwrite a newer one.
    pub attempt_id: u64,
    /// Whether the staged replica has a complete validated baseline.
    pub baseline_ready: bool,
}

/// Why a promotion was refused. Every member is a precondition of
/// `RouteRegistry::promote`, in the order it is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionRefusal {
    /// No candidate is staged for this session.
    NoCandidate,
    /// A newer attempt has already been staged.
    AttemptMovedOn,
    /// The candidate has no complete validated baseline yet.
    BaselineIncomplete,
    /// The connection no longer presents the prepared token.
    TokenChanged,
    /// The connection is gone from its worker's slots.
    ConnectionGone,
    /// The grant no longer admits this session.
    GrantDoesNotAdmit,
    /// No live view wants this session on this worker any more.
    NoViewDemand,
    /// Another connection already holds the active slot and this one is not
    /// loopback. The preference order is enforced here rather than by hoping
    /// registration order matches it.
    LoopbackPreferred,
}
