//! Which carrier answers one history page: the elected direct route, or the
//! coordinator.
//!
//! The pager validates and splices whatever comes back; this module only
//! SELECTS the carrier, and it composes the route registry and the token
//! comparison `roost-client-core` already owns rather than re-deriving either.
//! The row arithmetic is `roost-client-core::terminal::history`'s, and the
//! retained-floor clamp is `terminal::history_backfill`'s — neither is here.
//!
//! Ported from `apps/web/src/lib/scrollbackDirectHistory.ts`, whose rule is
//! preserved exactly: direct history is legal only for the canonical
//! connection/token pair, and a direct read that fails, is overlimit, or whose
//! route moved underneath it is retried on the coordinator ONCE.

use roost_client_core::terminal::routes::SessionRoute;
use roost_client_core::terminal::token::{TerminalToken, token_matches};

/// The error a direct carrier answers a history read with instead of a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectReadError {
    /// The read never completed: the socket dropped or the page request timed out.
    Transport,
    /// The page was larger than the direct transport admits, so it is refused
    /// rather than truncated. A truncated page would paint a history total that
    /// no frame ever described.
    Overlimit,
    /// The carrier answered with some other refusal.
    Rejected,
}

impl DirectReadError {
    /// The word a direct carrier uses for an overlimit refusal.
    pub const OVERLIMIT_ERROR: &'static str = "scrollback response exceeds direct transport limit";

    /// Read one refusal string off the carrier's whole error channel, or `None`
    /// when it named none — an empty error string IS the success spelling.
    pub fn parse(error: &str) -> Option<Self> {
        match error {
            "" => None,
            Self::OVERLIMIT_ERROR => Some(Self::Overlimit),
            _ => Some(Self::Rejected),
        }
    }
}

/// Which carrier one history read goes out on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryCarrier {
    /// The elected direct route.
    Direct,
    /// The coordinator, which every read may fall back to.
    Coordinator,
}

/// What a read that started on the direct carrier does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectHistoryOutcome {
    /// The page is the direct carrier's.
    Use,
    /// Ask the coordinator instead.
    FallBackToCoordinator {
        /// Why the direct route is being abandoned for this one read.
        why: DirectReadError,
    },
    /// Refuse the read: the elected route MOVED while it was out, so its answer
    /// belongs to a route this read never addressed and applying it would paint
    /// rows from a generation the pane has already retired.
    Reject,
}

/// Whether a session's direct route may serve a history read right now.
///
/// Legal only for the exact canonical pair: a route that exists, a live
/// generation for the session, and the two equal. A missing generation must
/// never match — that is the rule v2 spells out as a null argument returning
/// false, and it is what stops a frame arriving with no admission from counting
/// as current.
pub fn direct_route_is_legal(
    route: Option<&SessionRoute>,
    session_token: Option<&TerminalToken>,
) -> bool {
    let Some(route) = route else {
        return false;
    };
    token_matches(Some(&route.token), session_token)
}

/// The carrier one read goes out on, given whether a direct route is legal.
#[must_use]
pub fn carrier_for(direct_legal: bool) -> HistoryCarrier {
    if direct_legal {
        HistoryCarrier::Direct
    } else {
        HistoryCarrier::Coordinator
    }
}

/// What one direct read's answer means.
///
/// `route_before` and `route_now` are the elected connection identities either
/// side of the read. A TRANSPORT failure only falls back when they agree: a route
/// that changed underneath the read is not a transport problem, it is an answer
/// for someone else, and applying it would be the history-corruption class.
#[must_use]
pub fn direct_history_outcome(
    route_before: Option<&str>,
    route_now: Option<&str>,
    error: Option<DirectReadError>,
) -> DirectHistoryOutcome {
    let Some(error) = error else {
        return DirectHistoryOutcome::Use;
    };
    if matches!(error, DirectReadError::Transport) && route_before != route_now {
        return DirectHistoryOutcome::Reject;
    }
    DirectHistoryOutcome::FallBackToCoordinator { why: error }
}
