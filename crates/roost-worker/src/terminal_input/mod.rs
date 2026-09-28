//! Browser terminal input at the worker boundary: the work budget every
//! browser-triggered write reserves against, the actor/session route owner
//! that fences old routes out, the coordinator link's authority over one
//! request, and the input owner `runtime::downstream` routes to. Built by the
//! composition root; the session write itself is `session::input_write`. Ports
//! `apps/worker/src/terminal/terminal-input-{route-owner,work-budget}.ts` and
//! `apps/worker/src/transport/coord-link-input-authority.ts`.

pub mod link_authority;
pub mod port;
mod route_attempt;
pub mod route_owner;
pub mod work_budget;

pub use port::InputOwner;
pub use route_owner::{RouteActor, RouteClaim, RouteClaimBudget, TerminalInputRouteOwner};
pub use work_budget::{InputWorkOrigin, TerminalInputWorkBudget};
