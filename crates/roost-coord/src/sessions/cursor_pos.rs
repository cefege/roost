//! Cursor presence: publishes a tab-scoped cursor to the session's watchers.
//! Called by the `SessionsCursorPos` arm in `rpc/service_impl.rs`; depends on
//! the presence hub in `sync_ws::feed::presence` and the session route index.

use connectrpc::{Response, ServiceResult};
use roost_proto::{SessionsCursorPosRequest, SessionsCursorPosResponse};
use roost_protocol::wire::{ChannelId, WorkerFp};
use serde_json::json;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::sessions::rpc_sessions::{lease, read_failed, viewer_key};
use crate::sync_ws::feed::presence::publish_presence;

/// Publish the caller's cursor to the session's other viewers. Presence is
/// tab-scoped. `accepted` says the session's route resolved and the delta was
/// published; an unknown session is `false`, never an error.
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
    let mut accepted = false;
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
        accepted = true;
    }
    Response::ok(SessionsCursorPosResponse {
        accepted,
        ..Default::default()
    })
}
