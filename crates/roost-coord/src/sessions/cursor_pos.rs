//! `SessionsCursorPos`: publish the caller's cursor to a session's other viewers
//! and relay it to the session's worker. Called from the arm in
//! `rpc/service_impl.rs`. Ports the `sessionsCursorPos` handler of
//! `apps/coord/src/sessions/handlers-sessions.ts` and `forwardToSessionWorker` of
//! `apps/coord/src/rpc/router-helpers.ts`, whose other two helpers are
//! `ScrollbackRelay::session_worker_socket` and `rpc_relay::send_browser_command`.

use connectrpc::{Response, ServiceResult};
use roost_proto::{SessionsCursorPosRequest, SessionsCursorPosResponse};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};
use serde_json::json;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::sessions::rpc_sessions::{lease, read_failed, viewer_key};
use crate::sync_ws::feed::presence::publish_presence;
use crate::terminal_screen::rpc_relay::send_browser_command;

/// Publish the caller's cursor to the session's other viewers, and relay it to
/// the worker. Presence is tab-scoped; the worker envelope carries the bare
/// device fingerprint.
pub async fn handle_sessions_cursor_pos(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsCursorPosRequest,
) -> ServiceResult<SessionsCursorPosResponse> {
    let browser_fp = require_account_device(caller)?;
    let _lease = lease(core)?;
    let viewer = viewer_key(browser_fp, caller.tab_id.as_deref());
    let route: Option<(String, i64)> =
        sqlx::query_as("SELECT worker_fp, channel FROM sessions WHERE id = $1")
            .bind(&req.session_id)
            .fetch_optional(core.services.db.pool())
            .await
            .map_err(|error| read_failed("cursor_pos", &error))?;
    if let Some((worker_fp, channel)) = route
        && let (Ok(worker_fp), Ok(channel_id)) = (
            WorkerFp::try_from(worker_fp.as_str()),
            ChannelId::try_from(channel),
        )
    {
        let presence = json!({
            "kind": "presence-delta",
            "channel_id": channel,
            "viewer_id": viewer,
            "cursor_col": req.col,
            "cursor_row": req.row,
            "label": browser_fp.chars().take(8).collect::<String>(),
        });
        let services = &core.services;
        publish_presence(
            &services.buses,
            services.byte_hub.as_ref(),
            &worker_fp,
            channel_id,
            presence,
        );
    }
    let accepted = forward_to_session_worker(core, &req.session_id, browser_fp, |session_id| {
        ClientControlFrame::CursorPos {
            session_id,
            col: i64::from(req.col),
            row: i64::from(req.row),
            trace_id: None,
        }
    })
    .await;
    Response::ok(SessionsCursorPosResponse {
        accepted,
        ..Default::default()
    })
}

/// Relay a fire-and-ack control frame to a session's worker. `false`, never an
/// error, when the session, its worker or the send is gone.
async fn forward_to_session_worker(
    core: &CoordCore,
    raw_session_id: &str,
    viewer_fp: &str,
    frame_for: impl FnOnce(SessionId) -> ClientControlFrame,
) -> bool {
    let Ok(session) = SessionId::try_from(raw_session_id) else {
        return false;
    };
    let relay = &core.services.scrollback;
    let Ok(binding) = relay
        .session_worker_socket(&core.services.db, &session)
        .await
    else {
        return false;
    };
    match send_browser_command(
        &binding.handle,
        viewer_fp,
        &relay.pending().next_request_id(),
        frame_for(session),
    ) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(worker_fp = %binding.worker_fp, %error, "sessions: a forwarded control frame was not sent");
            false
        }
    }
}
