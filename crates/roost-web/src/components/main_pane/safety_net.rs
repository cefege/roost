//! MainPane's wiring of the dead-route safety net: evaluate on every change of
//! the route's resolution, run the grace timer, and on a durable miss navigate
//! to the newest open sibling in the viewed session's folder, else home. The
//! machine is `crate::dead_route_safety_net`; this file is its host. Ports the
//! `installDeadRouteSafetyNet` call and `bounceTarget` in
//! `apps/web/src/components/MainPane.tsx`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::store::Store;
use roost_client_core::store::selectors::session_by_id;

use crate::dead_route_safety_net::{Bounce, DeadRouteSafetyNet, RouteLiveness, SafetyNetStep};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::Pump;
use crate::route_session::{active_open_session_for_route, sibling_or_home_href};
use crate::router_state::navigate_path;
use crate::routes::Route;

/// Install the safety net for the calling pane.
pub(super) fn use_dead_route_safety_net(
    pump: &Pump,
    path: Signal<String>,
    route: Route,
    open_session_id: Option<String>,
    on_terminal_route: bool,
    hydrated: bool,
) {
    let net = use_hook(|| Rc::new(RefCell::new(DeadRouteSafetyNet::default())));
    let effect_pump = pump.clone();
    let effect_net = Rc::clone(&net);
    use_effect(use_reactive(
        (&route, &open_session_id, &on_terminal_route, &hydrated),
        move |(_, open_session_id, on_terminal_route, hydrated)| {
            let core = effect_pump.core();
            let step = {
                let core = core.borrow();
                let open = open_session_id
                    .as_deref()
                    .and_then(|session_id| session_by_id(core.store(), session_id));
                effect_net.borrow_mut().evaluate(RouteLiveness {
                    open_session: open,
                    on_terminal_route,
                    hydrated,
                })
            };
            if let SafetyNetStep::Arm { ticket, grace_ms } = step {
                let timer_pump = effect_pump.clone();
                let timer_net = Rc::clone(&effect_net);
                schedule(grace_ms, move || {
                    let core = timer_pump.core();
                    let bounce = {
                        let core = core.borrow();
                        let recovered = active_open_session_for_route(
                            core.store(),
                            &BrowserWorkerPaths,
                            &Route::parse(&path.peek()),
                        )
                        .is_some();
                        timer_net
                            .borrow_mut()
                            .fire(ticket, recovered)
                            .map(|bounce| (bounce_target(core.store(), &bounce), bounce))
                    };
                    if let Some((target, bounce)) = bounce {
                        tracing::warn!(
                            target: "nav",
                            sid = bounce.last_open.as_ref().map_or("", |session| session.id.as_str()),
                            reason = bounce.reason(),
                            %target,
                            "nav.safety_net_redirect"
                        );
                        navigate_path(path, target);
                    }
                });
            }
        },
    ));
    use_drop(move || net.borrow_mut().dispose());
}

/// Where a durable miss lands (`route_session::sibling_or_home_href` from the
/// last session this route rendered open, else home).
fn bounce_target(store: &Store, bounce: &Bounce) -> String {
    bounce.last_open.as_ref().map_or_else(
        || "/".to_owned(),
        |last| sibling_or_home_href(store, &BrowserWorkerPaths, last),
    )
}

#[cfg(target_arch = "wasm32")]
fn schedule(delay_ms: u32, callback: impl FnOnce() + 'static) {
    use wasm_bindgen::JsCast as _;

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = wasm_bindgen::closure::Closure::once_into_js(callback);
    let delay = i32::try_from(delay_ms).unwrap_or(i32::MAX);
    if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay)
        .is_err()
    {
        tracing::warn!(target: "nav", "the browser refused the safety-net timer");
    }
}

/// A native build has no event loop to wait on; nothing bounces.
#[cfg(not(target_arch = "wasm32"))]
fn schedule(_delay_ms: u32, _callback: impl FnOnce() + 'static) {}
