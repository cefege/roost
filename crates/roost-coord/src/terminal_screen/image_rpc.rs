//! Demand-driven retrieval of one retained terminal image from its owning worker.
//!
//! The cell grid carries stable content keys, not image bytes. This handler
//! relays a key-scoped request and returns the PNG only when the caller asks.

use std::time::Duration;

use base64::Engine;
use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::{SessionsGetTerminalImageRequest, SessionsGetTerminalImageResponse};
use roost_protocol::wire::control::ClientControlFrame;
use serde_json::Value;

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::terminal_screen::rpc_relay::{error_text, send_browser_command, session_id};

const IMAGE_RPC_DEADLINE_MS: u64 = 8_000;

/// Fetch one retained image by its stable content key.
pub async fn handle_sessions_get_terminal_image(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsGetTerminalImageRequest,
) -> ServiceResult<SessionsGetTerminalImageResponse> {
    require_account_device(caller)?;
    let session = session_id(&req.session_id)?;
    let relay = &core.services.scrollback;
    let binding = relay
        .session_worker_socket(&core.services.db, &session)
        .await?;
    let mut pending = relay
        .pending()
        .create_fresh(Some(binding.worker_fp.as_str()), relay.now_ms())?;
    let send = send_browser_command(
        &binding.handle,
        caller.fingerprint(),
        pending.request_id(),
        ClientControlFrame::GetTerminalImage {
            request_id: pending.request_id().to_owned(),
            session_id: session,
            image_key: req.image_key,
            trace_id: None,
        },
    );
    if let Err(error) = send {
        relay.pending().reject_unavailable(
            pending.request_id(),
            error_text(&error),
            Some(binding.worker_fp.as_str()),
        );
        return Err(error);
    }
    let payload = await_image(&mut pending).await?;
    let Some(encoded) = payload.get("png").and_then(Value::as_str) else {
        return Err(ConnectError::new(
            ErrorCode::NotFound,
            "terminal image not found",
        ));
    };
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ConnectError::new(ErrorCode::Internal, "terminal image reply is invalid"))?;
    Response::ok(SessionsGetTerminalImageResponse {
        png,
        ..Default::default()
    })
}

async fn await_image(
    pending: &mut crate::terminal_screen::pending_rpcs::PendingRpc,
) -> Result<Value, ConnectError> {
    let timed_out = || ConnectError::new(ErrorCode::Unavailable, "terminal image serve timed out");
    match tokio::time::timeout(
        Duration::from_millis(IMAGE_RPC_DEADLINE_MS),
        pending.settle(),
    )
    .await
    {
        Ok(Ok(payload)) => Ok(payload),
        Ok(Err(error)) => Err(match error.code {
            ErrorCode::DeadlineExceeded => timed_out(),
            _ => ConnectError::new(
                ErrorCode::Internal,
                format!("terminal image serve failed: {}", error_text(&error)),
            ),
        }),
        Err(_) => Err(timed_out()),
    }
}
