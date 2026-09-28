//! `SessionsInput`: unary terminal input, through the same sender lane and
//! write control as Sync input, carrying no browser route authority and no
//! audit of its own. A coordinator-minted sequence stands in for the one a
//! Sync client supplies. Called from the arm in `rpc/service_impl.rs`.
//! Ports the `sessionsInput` handler of `apps/coord/src/sessions/handlers-sessions.ts`.

use connectrpc::{Response, ServiceResult};
use roost_proto::{SessionsInputRequest, SessionsInputResponse};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::terminal_input::control_lane::terminal_viewer_identity;
use crate::terminal_input::input_control::{InputControlCommand, process_input_control};
use crate::terminal_input::write_control::TerminalWriteStatus;

/// Write one batch to a session's PTY; `accepted` only when the keeper proved
/// every byte written.
pub async fn handle_sessions_input(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsInputRequest,
) -> ServiceResult<SessionsInputResponse> {
    let browser_fp = require_account_device(caller)?;
    let services = &core.services;
    let command = InputControlCommand {
        identity: terminal_viewer_identity(browser_fp, caller.tab_id.as_deref()),
        session_id: req.session_id,
        input_seq: services.terminal_input.next_compatibility_input_seq(),
        data: req.data,
        socket_generation: None,
        input_route_authority: None,
        audited: false,
        deadline: None,
    };
    let result = process_input_control(services, command).await;
    tracing::debug!(session_id = %result.session_id, status = result.status.as_str(),
        written_bytes = result.written_bytes, "sessions input settled");
    Response::ok(SessionsInputResponse {
        accepted: result.status == TerminalWriteStatus::Accepted,
        ..Default::default()
    })
}
