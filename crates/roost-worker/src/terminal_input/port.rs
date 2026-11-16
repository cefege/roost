//! The coordinator link's input owner: acknowledged `input-request`s, route
//! claims, and the route half of a closed browser socket. Implements
//! `link_ports::TerminalInputPort` for `runtime::downstream`; built by the
//! composition root over the session layer.

use std::sync::Arc;

use roost_proto::{DInputRequest, DTerminalInputRouteClaim, TerminalInputRouteResult};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::InputResult;

use super::link_authority::{LinkClaimBudget, LinkInputAuthority, LinkRequestBudget};
use super::route_owner::{RouteActor, RouteClaim, TerminalInputRouteOwner};
use super::work_budget::{InputWorkOrigin, TerminalInputWorkBudget};
use crate::link_ports::TerminalInputPort;
use crate::session::input_write::WorkerInputResult;
use crate::session::lifecycle::SessionManager;
use crate::session::table::SessionTable;
use crate::uplink::terminal_results::{InputResultKey, worker_input_result};
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// The coordinator-link input callbacks, over one route owner and one work
/// budget that the local door shares.
#[derive(Debug)]
pub struct InputOwner {
    manager: Arc<SessionManager>,
    sessions: Arc<SessionTable>,
    routes: TerminalInputRouteOwner,
    work_budget: TerminalInputWorkBudget,
}

impl InputOwner {
    pub fn new(
        manager: Arc<SessionManager>,
        sessions: Arc<SessionTable>,
        routes: TerminalInputRouteOwner,
        work_budget: TerminalInputWorkBudget,
    ) -> Self {
        Self {
            manager,
            sessions,
            routes,
            work_budget,
        }
    }
}

impl TerminalInputPort for InputOwner {
    /// The work reservation is taken before anything else and released only
    /// after the result exists, so a coordinator that floods input is refused
    /// (pre-write) instead of queueing unbounded keeper work.
    fn write_input(
        &self,
        request: DInputRequest,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<InputResult>> {
        let key = InputResultKey::from(&request);
        let reservation = match self
            .work_budget
            .reserve_input(&InputWorkOrigin::Sync, request.data.len())
        {
            Ok(reservation) => reservation,
            Err(reason) => {
                let refused = WorkerInputResult::Rejected {
                    reason: reason.to_owned(),
                };
                return Box::pin(std::future::ready(worker_input_result(
                    &key, &refused, false,
                )));
            }
        };
        let authority = LinkInputAuthority::for_request(
            &request,
            fence.clone(),
            Arc::clone(&self.sessions),
            self.routes.clone(),
        );
        let written = match SessionId::try_from(request.session_id.as_str()) {
            Ok(session_id) => self.manager.write_terminal_input(
                &session_id,
                request.input_seq,
                request.data,
                Some(Box::new(LinkRequestBudget::new(budget, fence))),
                Some(Box::new(authority)),
            ),
            Err(_) => Box::pin(std::future::ready(WorkerInputResult::Rejected {
                reason: "terminal session is unavailable".to_owned(),
            })),
        };
        Box::pin(async move {
            let result = written.await;
            drop(reservation);
            worker_input_result(&key, &result, false)
        })
    }

    fn claim_route(
        &self,
        request: DTerminalInputRouteClaim,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<TerminalInputRouteResult>> {
        let actor = RouteActor {
            device_fingerprint: request.device_fingerprint,
            tab_id: request.tab_id,
            connection_id: request.browser_connection_id,
        };
        let claim_budget = LinkClaimBudget::new(
            LinkRequestBudget::new(budget, fence),
            Arc::clone(&self.sessions),
            request.session_id.clone(),
        );
        let claim = RouteClaim {
            request_id: request.request_id,
            session_id: request.session_id,
            revision: request.revision,
            worker_epoch: request.worker_epoch,
        };
        let pending = self.routes.claim(actor, claim, Box::new(claim_budget));
        Box::pin(async move { Some(pending.await) })
    }

    fn retire_connection(&self, socket_id: &str) {
        self.routes.retire_connection(socket_id);
    }
}
