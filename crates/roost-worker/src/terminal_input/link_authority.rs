//! Live Sync-input authority and request budgets for input that arrived over
//! the coordinator link. Coordinator authorization is the current link
//! connection; route ownership stays worker-owned and is checked again
//! immediately before `session::input_write` begins the keeper write. Frames
//! from a coordinator that sends no actor fields keep the empty-epoch rule.
//! Built by `terminal_input::port`. Ports
//! `apps/worker/src/transport/coord-link-input-authority.ts` and the link half of
//! `terminalBudget` (`coord-link-direct-deps.ts` `onTerminalInputRouteClaim`).

use std::sync::Arc;
use std::time::Instant;

use roost_proto::DInputRequest;
use roost_protocol::wire::brand::SessionId;

use super::route_owner::{RouteActor, RouteClaimBudget, TerminalInputRouteOwner};
use crate::session::input_write::{TerminalWriteAuthority, TerminalWriteBudget};
use crate::session::table::SessionTable;
use crate::uplink::{LinkFence, RequestBudget};

/// A request budget measured from frame receipt, fenced to the connection the
/// frame arrived on.
#[derive(Debug, Clone)]
pub struct LinkRequestBudget {
    budget: RequestBudget,
    fence: LinkFence,
}

impl LinkRequestBudget {
    pub fn new(budget: RequestBudget, fence: LinkFence) -> Self {
        Self { budget, fence }
    }
}

impl TerminalWriteBudget for LinkRequestBudget {
    fn is_current_connection(&self) -> bool {
        self.fence.is_current()
    }

    fn expired(&self) -> bool {
        self.budget.expired(Instant::now())
    }
}

/// A route claim's budget over the link: the request budget, plus session
/// authority that is re-read at every check rather than snapshotted.
#[derive(Debug, Clone)]
pub struct LinkClaimBudget {
    request: LinkRequestBudget,
    sessions: Arc<SessionTable>,
    session_id: String,
}

impl LinkClaimBudget {
    pub fn new(
        request: LinkRequestBudget,
        sessions: Arc<SessionTable>,
        session_id: String,
    ) -> Self {
        Self {
            request,
            sessions,
            session_id,
        }
    }
}

impl TerminalWriteBudget for LinkClaimBudget {
    fn is_current_connection(&self) -> bool {
        self.request.is_current_connection()
    }

    fn expired(&self) -> bool {
        self.request.expired()
    }
}

impl RouteClaimBudget for LinkClaimBudget {
    fn is_session_authorized(&self) -> bool {
        self.request.is_current_connection() && holds_session(&self.sessions, &self.session_id)
    }
}

/// v2 `coordLinkInputAuthority`'s predicates for one `DInputRequest`.
#[derive(Debug, Clone)]
pub struct LinkInputAuthority {
    fence: LinkFence,
    sessions: Arc<SessionTable>,
    routes: TerminalInputRouteOwner,
    session_id: String,
    /// `None` when the coordinator sent no actor fields.
    actor: Option<RouteActor>,
    input_route_epoch: String,
}

impl LinkInputAuthority {
    pub fn for_request(
        request: &DInputRequest,
        fence: LinkFence,
        sessions: Arc<SessionTable>,
        routes: TerminalInputRouteOwner,
    ) -> Self {
        let has_actor = !request.device_fingerprint.is_empty()
            && !request.tab_id.is_empty()
            && !request.browser_connection_id.is_empty();
        let actor = has_actor.then(|| RouteActor {
            device_fingerprint: request.device_fingerprint.clone(),
            tab_id: request.tab_id.clone(),
            connection_id: request.browser_connection_id.clone(),
        });
        Self {
            fence,
            sessions,
            routes,
            session_id: request.session_id.clone(),
            actor,
            input_route_epoch: request.input_route_epoch.clone(),
        }
    }
}

impl TerminalWriteAuthority for LinkInputAuthority {
    fn is_session_authorized(&self) -> bool {
        self.fence.is_current() && holds_session(&self.sessions, &self.session_id)
    }

    fn is_current_input_route(&self) -> bool {
        let Some(actor) = &self.actor else {
            return self.input_route_epoch.is_empty();
        };
        if self.input_route_epoch.is_empty() {
            self.routes.allows_legacy_input(actor, &self.session_id)
        } else {
            self.routes
                .is_current(actor, &self.session_id, &self.input_route_epoch)
        }
    }
}

fn holds_session(sessions: &SessionTable, session_id: &str) -> bool {
    SessionId::try_from(session_id)
        .is_ok_and(|session_id| sessions.channel_of(&session_id).is_some())
}
