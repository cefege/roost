//! The coordinator round trips the search page owns: arming a query, taking the
//! first page when the debounce has ended, asking for the next one, and
//! cancelling what the reader has walked away from.
//!
//! The state machine is the client's, in `roost_client_core::client::global_search`;
//! this file is the HOST half of it and nothing more — it mints the identity the
//! coordinator cancels under, allocates the `call_id` the answer is correlated
//! with, and issues the calls. It reads no rows: the fold that publishes them
//! is `handle_sync`'s, because a page has to be fenced against the search that
//! asked for it and only the controller knows which search that is.
//!
//! Ports the calls of `apps/web/src/lib/globalContentSearchController.ts`.

#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
use roost_client_core::client::global_search::{GlobalSearchQuery, GlobalSearchRequest};
use roost_client_core::effect::RpcCall;

use crate::components::terminal::dom::now_ms;
use crate::pump::Pump;

/// Arm `query` on the controller and cancel whatever it was running.
///
/// The identity is read BEFORE the new search is armed, because arming one
/// drops the previous identity: the coordinator's ledger keeps a scan running
/// for a cursor's whole lifetime otherwise, and that scan is work no viewer is
/// waiting for. Returns the identity that was cancelled, for the caller's log.
#[must_use]
pub fn set_query(pump: &Pump, query: GlobalSearchQuery, at_ms: u64) -> Option<String> {
    let abandoned = pump.write_store(|store| {
        let abandoned = store.global_search.active_search_id().map(str::to_owned);
        let outcome = store.global_search.set_search(query, at_ms);
        tracing::info!(
            target: "search",
            scope = ?outcome,
            abandoned = abandoned.as_deref().unwrap_or(""),
            "global search armed"
        );
        abandoned
    });
    if let Some(search_id) = &abandoned {
        cancel(pump, search_id.clone());
    }
    abandoned
}

/// Stop the search this client is running, wherever it got to.
pub fn cancel(pump: &Pump, search_id: String) {
    let call_id = next_call_id(pump);
    tracing::info!(target: "search", call_id, %search_id, "global search cancelled");
    send(
        pump,
        RpcCall::SessionsCancelGlobalSearch { call_id, search_id },
    );
}

/// Send the first page if the debounce has ended; report whether one went.
///
/// The debounce is the CONTROLLER's clock: it decides when a fleet-wide scan may
/// start, and a host that sent on its own timer would put a keystroke's scan in
/// front of the reader's last one.
pub fn take_due_first_page(pump: &Pump, search_id: String) -> bool {
    let call_id = next_call_id(pump);
    let request = pump.write_store(|store| {
        store
            .global_search
            .take_first_page(&search_id, call_id, now_ms())
    });
    request.is_some_and(|request| {
        tracing::info!(
            target: "search",
            call_id = request.call_id,
            %request.search_id,
            query = %request.query,
            "global search page requested"
        );
        send(pump, into_call(request));
        true
    })
}

/// Ask for the page after the one published, if there is one and none is
/// outstanding.
pub fn take_next_page(pump: &Pump) -> bool {
    let call_id = next_call_id(pump);
    let request = pump.write_store(|store| store.global_search.load_more(call_id));
    request.is_some_and(|request| {
        tracing::info!(
            target: "search",
            call_id = request.call_id,
            %request.search_id,
            "global search continuation requested"
        );
        send(pump, into_call(request));
        true
    })
}

/// Run the running query again from its first page, after a failure.
pub fn retry(pump: &Pump) {
    let desired = {
        let core = pump.core();
        let core = core.borrow();
        core.store().global_search.desired().clone()
    };
    let _ = set_query(pump, desired, now_ms());
}

/// The identity a logical search is cancelled under.
///
/// A UUID, because the coordinator keys its ledger by an opaque client string
/// and this is the only party that can mint one; `crypto.randomUUID` needs a
/// secure context, and outside one there is nothing to cancel under, so the
/// search is not started at all.
#[cfg(target_arch = "wasm32")]
pub fn mint_search_id() -> Option<String> {
    web_sys::window()
        .filter(web_sys::Window::is_secure_context)
        .and_then(|window| window.crypto().ok())
        .map(|crypto| crypto.random_uuid())
}

/// A native build talks to no coordinator, so it mints nothing.
#[cfg(not(target_arch = "wasm32"))]
pub fn mint_search_id() -> Option<String> {
    None
}

/// The next `call_id`, from the core's counter every other call uses. The
/// counter is store state, so minting one is a write like any other.
fn next_call_id(pump: &Pump) -> u64 {
    pump.write_store(|store| store.next_call_id())
}

/// One controller request as the call the wire carries.
fn into_call(request: GlobalSearchRequest) -> RpcCall {
    RpcCall::SessionsSearchGlobal {
        call_id: request.call_id,
        search_id: request.search_id,
        query: request.query,
        case_sensitive: request.case_sensitive,
        cursor: request.cursor,
        max_sessions: request.max_sessions,
        max_rows_per_session: request.max_rows_per_session,
        max_matches: request.max_matches,
    }
}

/// Issue one call and hand its answer back to the core.
///
/// The same path `Effect::Rpc` takes: a host that performed its own call would
/// be an answer the core's fold never sees, so a page would arrive with nobody
/// to fence it.
#[cfg(target_arch = "wasm32")]
fn send(pump: &Pump, call: RpcCall) {
    let pump = pump.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = pump.rpc().call_core(&call).await;
        pump.dispatch(ClientEvent::RpcResultReceived(result));
    });
}

/// A native build has no coordinator to call.
#[cfg(not(target_arch = "wasm32"))]
fn send(_pump: &Pump, _call: RpcCall) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The caps are the COORDINATOR's, and a host that invented them would ask
    /// for a different page shape than the coordinator documents.
    #[test]
    fn a_page_request_carries_the_caps_the_controller_chose() {
        let request = GlobalSearchRequest {
            call_id: 7,
            search_id: "search-1".to_owned(),
            version: 1,
            query: "marker".to_owned(),
            case_sensitive: true,
            cursor: Some("cursor-1".to_owned()),
            max_sessions: 4,
            max_rows_per_session: 512,
            max_matches: 64,
        };
        let RpcCall::SessionsSearchGlobal {
            call_id,
            search_id,
            query,
            case_sensitive,
            cursor,
            max_sessions,
            max_rows_per_session,
            max_matches,
        } = into_call(request)
        else {
            panic!("a page request must stay one global-search call");
        };
        assert_eq!((call_id, search_id.as_str()), (7, "search-1"));
        assert_eq!((query.as_str(), case_sensitive), ("marker", true));
        assert_eq!(cursor.as_deref(), Some("cursor-1"));
        assert_eq!(
            (max_sessions, max_rows_per_session, max_matches),
            (4, 512, 64)
        );
    }
}
