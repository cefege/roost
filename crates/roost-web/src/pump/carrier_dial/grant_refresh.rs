//! One loopback connection per worker across grant refreshes: whether a newly
//! minted grant needs a socket of its own at all.
//!
//! Called by `pump::carrier_dial::dial` before it opens anything. Reads the
//! core's `RouteRegistry` (the one copy of a carrier's scope) and the pump's
//! carrier table. Ports v2 `store/transport/local-terminal.ts` `presentGrant`.

use roost_client_core::TerminalTransport;
use roost_client_core::client::local::LocalTerminalGrant;

use crate::pump::Pump;

/// `false` when the worker's registered loopback carrier already admits every
/// session `grant` names — the core widened it when the grant was reported, so
/// a second socket would only be a second connection to one worker. Otherwise
/// every socket still waiting on its `Ready` for an older grant is closed,
/// because that `Ready` would name the older scope, and `true` is returned.
pub(super) fn spends_new_socket(pump: &Pump, grant: &LocalTerminalGrant) -> bool {
    if live_carrier_covers(pump, grant) {
        tracing::info!(
            target: "carriers",
            worker_fp = %grant.worker_fp,
            "the live loopback carrier already carries the refreshed grant; no second dial"
        );
        return false;
    }
    let superseded = pump.inner.carriers.borrow().pending_for(&grant.worker_fp);
    for connection_id in superseded {
        let handle = pump
            .inner
            .carriers
            .borrow_mut()
            .drop_pending(&connection_id);
        if let Some(handle) = handle {
            handle.close(1000, "local terminal grant scope changed");
            tracing::info!(
                target: "carriers",
                connection_id,
                worker_fp = %grant.worker_fp,
                "a loopback socket dialled on an older grant was replaced"
            );
        }
    }
    true
}

fn live_carrier_covers(pump: &Pump, grant: &LocalTerminalGrant) -> bool {
    let core = pump.inner.core.borrow();
    let routes = &core.store().routes;
    let Some(connection_id) =
        routes.route_connection_for(&grant.worker_fp, TerminalTransport::Loopback)
    else {
        return false;
    };
    let carriers = pump.inner.carriers.borrow();
    let Some(token) = carriers.token_for(&connection_id) else {
        return false;
    };
    routes.granted_sessions_for(token).is_some_and(|granted| {
        grant
            .session_ids
            .iter()
            .all(|session_id| granted.contains(session_id))
    })
}
