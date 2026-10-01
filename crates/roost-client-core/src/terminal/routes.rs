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

use std::collections::{BTreeMap, BTreeSet};

pub use registry::{ConnectionRegistration, LostRoute, RouteRegistry};

use crate::terminal::token::{TerminalToken, TerminalTransport};
use crate::terminal::view::ViewIntent;

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

/// One pane the candidate is preparing a SEPARATE wire view for.
///
/// The pane's own identity never changes — a renderer keeps its DOM across a
/// transport change — but the authority holds one lease per view id, so a view
/// published on two transports at once is one worker told twice, and the second
/// answer is refused as a duplicate live socket. The candidate therefore mints
/// its own id per pane and adopts it only on promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProspectiveView {
    /// The id the candidate published, once the host minted one. `None` while
    /// the request is outstanding, which is also what a pending mint looks like
    /// to a cancellation: nothing was published, so nothing is released.
    pub wire_view_id: Option<String>,
    /// The intent the canonical view held when this attempt began.
    pub source_intent: ViewIntent,
    /// The revision `source_intent` was published under.
    pub source_revision: u64,
    /// The revision this candidate publishes under: one past the source, so the
    /// authority reads a new id under a new intent rather than a replay.
    pub candidate_revision: u64,
    /// Whether the authority has acknowledged this view on the candidate.
    pub acknowledged: bool,
}

/// A staged candidate: one connection's replica for a session, folded SEPARATELY
/// from the session's canonical until it is promoted.
///
/// Separate folding is the rule, not an optimization
/// (`protocol/spec/direct-terminal.md:27`): a candidate with a half-built
/// baseline must not be able to paint, and a promotion must be able to swap a
/// complete grid in atomically.
///
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
    /// The views this attempt is preparing, by the pane's own identity.
    pub prospective_views: BTreeMap<String, ProspectiveView>,
    /// The host's clock when the attempt began, for the baseline deadline.
    pub staged_at_ms: u64,
}

/// A cancelled candidate: what its worker is still holding for this document,
/// and the token it must be released on.
///
/// Returned rather than released inside the registry because the release is a
/// COMMAND on a socket the caller owns, and a registry that both decided the
/// attempt died and sent the goodbye would be doing the host's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledCandidate {
    /// The session the attempt was for.
    pub session_id: String,
    /// The generation the attempt published on.
    pub token: TerminalToken,
    /// The minted wire ids, with the revision each was published under.
    pub minted: Vec<(String, u64)>,
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
    /// A view the candidate published has not been acknowledged yet.
    ViewUnacknowledged,
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
