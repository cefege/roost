//! `SessionsCancelGlobalSearch`: explicit coordinator cancellation of one
//! install-wide terminal search.
//!
//! Ports `apps/coord/src/search/global-search-cancel.ts`. The tombstone is
//! installed BEFORE any asynchronous worker discovery, so a search that
//! arrives while this cancel is still reading the database is already
//! refused. Called from the `SessionsCancelGlobalSearch` arm in
//! `rpc/service_impl.rs`; shares `services.search` with `search::rpc_search`.

use std::collections::HashSet;

use connectrpc::{Response, ServiceResult};
use roost_proto::{SessionsCancelGlobalSearchRequest, SessionsCancelGlobalSearchResponse};
use roost_protocol::terminal_search::GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::search::cursor_types::{GlobalSearchIdentity, GlobalSearchSessionPosition};
use crate::search::fanout::{
    list_authorized_global_search_sessions, send_global_search_cancellation_batches,
};
use crate::search::rpc_search::require_search_id;

/// Cancel one global search by the identity its tab owns.
pub async fn handle_sessions_cancel_global_search(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsCancelGlobalSearchRequest,
) -> ServiceResult<SessionsCancelGlobalSearchResponse> {
    let fingerprint = require_account_device(caller)?.to_owned();
    require_search_id(&req.search_id)?;
    let tab_id = caller.tab_id.clone().unwrap_or_default();
    let viewer_id = if tab_id.is_empty() {
        fingerprint.clone()
    } else {
        format!("{fingerprint}:{tab_id}")
    };
    let identity = GlobalSearchIdentity {
        device_fingerprint: fingerprint,
        tab_id,
        search_id: req.search_id,
    };
    let cancellation = core
        .services
        .search
        .cursors()
        .prepare_cancellation(&identity);
    if !cancellation.should_dispatch {
        return Response::ok(SessionsCancelGlobalSearchResponse::default());
    }
    let mut dispatch = CancellationDispatch {
        core,
        identity: &identity,
        viewer_id: &viewer_id,
        selected: cancellation.selected_sessions,
        queued: false,
    };
    let current = list_authorized_global_search_sessions(
        &core.services.db,
        GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    )
    .await?;
    dispatch.send_merged(&current.sessions);
    Response::ok(SessionsCancelGlobalSearchResponse::default())
}

/// Sends the worker cancels and then releases the in-flight search. Dropping
/// it unsent (a failed lookup, or the caller hanging up mid-lookup) still
/// cancels the sessions the search had selected, as v2's `finally` does.
struct CancellationDispatch<'a> {
    core: &'a CoordCore,
    identity: &'a GlobalSearchIdentity,
    viewer_id: &'a str,
    selected: Vec<GlobalSearchSessionPosition>,
    queued: bool,
}

impl CancellationDispatch<'_> {
    /// Cancel the selection first, then every currently authorized session,
    /// capped at one page.
    fn send_merged(&mut self, current: &[GlobalSearchSessionPosition]) {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut merged: Vec<GlobalSearchSessionPosition> = Vec::new();
        for session in self.selected.iter().chain(current) {
            if merged.len() >= GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS {
                break;
            }
            if seen.insert(session.session_id.as_str()) {
                merged.push(session.clone());
            }
        }
        self.send(&merged);
        self.queued = true;
    }

    fn send(&self, sessions: &[GlobalSearchSessionPosition]) {
        let relay = &self.core.services.scrollback;
        send_global_search_cancellation_batches(
            relay.workers(),
            relay.pending(),
            self.viewer_id,
            &self.identity.search_id,
            sessions,
        );
    }
}

impl Drop for CancellationDispatch<'_> {
    fn drop(&mut self) {
        if !self.queued {
            self.send(&self.selected);
        }
        self.core
            .services
            .search
            .cursors()
            .complete_cancellation(self.identity);
    }
}
