//! Which carrier answers one history page: the elected direct route, or the
//! coordinator. The pager validates and splices whatever comes back; this only
//! SELECTS the carrier, composing the route and token rules `roost-client-core`
//! owns. Ports `apps/web/src/lib/scrollbackDirectHistory.ts`: direct history is
//! legal only for the exact route/token pair, and the coordinator is asked
//! ONCE after a direct transport failure or an overlimit page.

use roost_client_core::terminal::routes::SessionRoute;
use roost_client_core::terminal::token::{TerminalToken, token_matches};

/// How a direct history read ended without a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectReadError {
    /// The read never completed: the socket dropped or the request failed.
    Transport,
    /// The page was larger than the direct transport admits.
    Overlimit,
    /// The carrier answered with some other refusal.
    Rejected,
}

impl DirectReadError {
    /// The error string a direct carrier uses for an overlimit page.
    pub const OVERLIMIT_ERROR: &'static str = "scrollback response exceeds direct transport limit";

    /// Read the carrier's error field; the empty string IS the success spelling.
    pub fn parse(error: &str) -> Option<Self> {
        match error {
            "" => None,
            Self::OVERLIMIT_ERROR => Some(Self::Overlimit),
            _ => Some(Self::Rejected),
        }
    }
}

/// Why a direct history read fails the page outright instead of falling back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectHistoryFailure {
    /// The elected route moved while the read was out, so no carrier answers it.
    RouteChanged,
    /// The direct carrier refused the read; masking that as a coordinator read
    /// would hide a broken direct reader behind a working relay.
    Rejected,
}

impl DirectHistoryFailure {
    /// The failure v2 throws, for the pane's log line.
    pub const fn message(self) -> &'static str {
        match self {
            Self::RouteChanged => "direct terminal scrollback route changed",
            Self::Rejected => "direct terminal scrollback request was rejected",
        }
    }
}

/// What a read that went out on the direct carrier does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectHistoryOutcome {
    /// The page is the direct carrier's.
    Use,
    /// Ask the coordinator instead, once.
    FallBackToCoordinator,
    /// The read failed; the pager treats it as a failed fetch.
    Fail(DirectHistoryFailure),
}

/// The route a history read may use directly, or `None` for the coordinator.
///
/// Legal only for the exact canonical pair: a route that exists, a live
/// generation for the session, and the two equal. A missing generation never
/// matches, so a session with no admission cannot count as current.
pub fn elected_direct_route<'route>(
    route: Option<&'route SessionRoute>,
    session_token: Option<&TerminalToken>,
) -> Option<&'route SessionRoute> {
    route.filter(|route| token_matches(Some(&route.token), session_token))
}

/// What one direct read's answer means. `elected_now` is `elected_direct_route`
/// re-evaluated after the read: a transport failure falls back only while the
/// read's own route is still the elected one, because a moved route means the
/// failure belongs to a carrier this session no longer reads from.
#[must_use]
pub fn direct_history_outcome(
    read_on: &SessionRoute,
    elected_now: Option<&SessionRoute>,
    error: Option<DirectReadError>,
) -> DirectHistoryOutcome {
    match error {
        None => DirectHistoryOutcome::Use,
        Some(DirectReadError::Transport) if elected_now != Some(read_on) => {
            DirectHistoryOutcome::Fail(DirectHistoryFailure::RouteChanged)
        }
        Some(DirectReadError::Transport | DirectReadError::Overlimit) => {
            DirectHistoryOutcome::FallBackToCoordinator
        }
        Some(DirectReadError::Rejected) => {
            DirectHistoryOutcome::Fail(DirectHistoryFailure::Rejected)
        }
    }
}
