//! Bridges the authenticated `SessionsNegotiateLocalTerminalPeer` RPC into the
//! negotiation owner, which performs every grant, route, SDP, capacity and
//! worker-generation check; this supplies only the trusted caller, its tab, and
//! the browser's cancellation (the request future being dropped).
//! Called by `rpc::service_impl`. Ports
//! `apps/coord/src/terminal/direct/handlers-sessions-terminal-peer.ts`.

use connectrpc::{Response, ServiceResult};
use roost_proto::{
    SessionsNegotiateLocalTerminalPeerRequest, SessionsNegotiateLocalTerminalPeerResponse,
};
use tokio_util::sync::CancellationToken;

use crate::auth::principal::{device_refusal, require_account_device};
use crate::coord_core::{Caller, CoordCore};
use crate::terminal_direct::peer_state::TerminalPeerCaller;

/// Negotiate one direct terminal peer for the caller's document and worker.
pub async fn handle_sessions_negotiate_local_terminal_peer(
    core: &CoordCore,
    caller: &Caller,
    request: SessionsNegotiateLocalTerminalPeerRequest,
) -> ServiceResult<SessionsNegotiateLocalTerminalPeerResponse> {
    let device_fingerprint = require_account_device(caller)?.to_owned();
    let owner_key = caller
        .principal
        .capture_owner_key()
        .ok_or_else(device_refusal)?;
    // The browser abandoning the call is v2's `context.signal` aborting: the
    // guard cancels the negotiation (withdrawing any sent offer) when this
    // future is dropped before it finishes.
    let abort = CancellationToken::new();
    let _abort_when_dropped = abort.clone().drop_guard();
    let response = core
        .services
        .terminal_direct
        .negotiations()
        .negotiate(
            TerminalPeerCaller {
                owner_key,
                device_fingerprint,
            },
            caller.tab_id.as_deref(),
            request,
            abort,
        )
        .response()
        .await?;
    Response::ok(response)
}
