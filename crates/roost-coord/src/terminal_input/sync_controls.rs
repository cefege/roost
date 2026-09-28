//! A Sync socket's terminal commands, carried out: a view or resync goes to the
//! terminal view hub, an input batch through its sender's lane, a route claim
//! or probe to the typed route owner. Every answer rides the control lane, and
//! a closing socket cancels its queued input and retires its input routes.
//! Called by `sync_ws::socket` (link unlocked) and `sync_ws::socket_open`.
//! Ports `apps/coord/src/terminal/input/sync-terminal-controls.ts`.

use std::sync::Arc;

use roost_proto::{FirehoseFrame, InputCommand};
use roost_protocol::viewport::is_terminal_uuid;

use crate::services::CoordServices;
use crate::sync_ws::commands::{ClientContext, TerminalCommand};
use crate::sync_ws::control_frames::{
    input_accepted_frame, input_ambiguous_frame, input_rejected_frame,
};
use crate::sync_ws::driver::{SyncLink, now_ms};
use crate::terminal_input::control_lane::TerminalViewerIdentity;
use crate::terminal_input::input_control::{
    InputControlCommand, InputRouteAuthority, process_input_control,
};
use crate::terminal_input::route_contract::is_terminal_route_identifier;
use crate::terminal_input::sync_route_controls::{accept_route_claim, accept_transport_probe};
use crate::terminal_input::write_control::TerminalWriteStatus;

/// What a terminal command needs from the socket that sent it, read once
/// under the link lock and then held without it.
#[derive(Debug, Clone)]
pub(super) struct SyncControlSocket {
    /// The socket's opaque id: the input generation and the route owner.
    pub socket_id: String,
    /// Whether the socket may write at all.
    pub read_only: bool,
    /// The browser tab the socket speaks for.
    pub tab_id: Option<String>,
    /// The `${fingerprint}:${tab}` sender key.
    pub viewer_key: Option<String>,
    /// The verified device fingerprint.
    pub device_fingerprint: String,
}

impl SyncControlSocket {
    fn of(link: &SyncLink) -> Self {
        let state = link.lock();
        Self {
            socket_id: state.socket_id.clone(),
            read_only: state.context.read_only,
            tab_id: state.context.tab_id.clone(),
            viewer_key: state.context.viewer_key.clone(),
            device_fingerprint: state.context.fingerprint.clone(),
        }
    }
}

/// Carry out one terminal command that passed the Sync command gate. Must be
/// called with the link unlocked: the view hub answers through this socket's
/// own sink, which takes the same lock.
pub fn accept_sync_terminal_command(
    services: &Arc<CoordServices>,
    link: &Arc<SyncLink>,
    command: TerminalCommand,
) {
    let socket = SyncControlSocket::of(link);
    match command {
        TerminalCommand::View(view) => {
            services
                .views
                .handle_view_command(&socket.socket_id, &view, now_ms());
        }
        TerminalCommand::Resync(resync) => {
            services
                .views
                .handle_resync(&socket.socket_id, &resync, now_ms());
        }
        TerminalCommand::Input(input) => accept_sync_input(services, link, &socket, input),
        TerminalCommand::RouteClaim(claim) => accept_route_claim(services, link, socket, claim),
        TerminalCommand::TransportProbe(probe) => {
            accept_transport_probe(services, link, socket, probe);
        }
    }
}

/// A closing tab-bound socket: cancel its queued input that has not begun,
/// and retire every worker route it claimed. Called once, at release.
pub fn close_sync_terminal_controls(
    services: &CoordServices,
    context: &ClientContext,
    socket_id: &str,
) {
    let (Some(viewer_key), Some(_)) = (&context.viewer_key, &context.tab_id) else {
        return;
    };
    let input = &services.terminal_input;
    input.route_results().retire_browser_connection(socket_id);
    input.lanes().cancel_generation(viewer_key, socket_id);
}

/// Send one answer on the socket's control lane, unless it is closing.
pub(super) fn reply(link: &SyncLink, frame: FirehoseFrame) {
    link.deliver_with(|state| {
        state.send_control(&frame, now_ms());
        None
    });
}

/// Whether the socket may observe this session right now. Read live: a
/// session the socket lost since its upgrade is no longer one it may write.
pub(super) fn scope_has_session(link: &SyncLink, session_id: &str) -> bool {
    link.lock().index.session_ids.contains(session_id)
}

fn accept_sync_input(
    services: &Arc<CoordServices>,
    link: &Arc<SyncLink>,
    socket: &SyncControlSocket,
    command: InputCommand,
) {
    let refusal = if !scope_has_session(link, &command.session_id) {
        Some("terminal session is unavailable")
    } else if socket.viewer_key.is_none() || socket.tab_id.is_none() {
        Some("terminal input requires a tab-bound Sync socket")
    } else if !command.input_route_epoch.is_empty()
        && !is_terminal_route_identifier(&command.input_route_epoch)
    {
        Some("invalid terminal input route epoch")
    } else if command
        .view_id
        .as_deref()
        .is_some_and(|view_id| !view_id.is_empty() && !is_terminal_uuid(view_id))
    {
        Some("invalid terminal input view id")
    } else {
        None
    };
    let (Some(viewer_key), Some(tab_id), None) = (&socket.viewer_key, &socket.tab_id, refusal)
    else {
        let reason = refusal.unwrap_or("terminal input requires a tab-bound Sync socket");
        let frame = input_rejected_frame(
            &command.session_id,
            command.input_seq,
            command.domain_generation,
            reason,
        );
        reply(link, frame);
        return;
    };
    let domain_generation = command.domain_generation;
    let outcome = process_input_control(
        services,
        InputControlCommand {
            identity: TerminalViewerIdentity {
                viewer_key: viewer_key.clone(),
                caller_fingerprint: socket.device_fingerprint.clone(),
            },
            input_route_authority: Some(InputRouteAuthority {
                device_fingerprint: socket.device_fingerprint.clone(),
                tab_id: tab_id.clone(),
                connection_id: socket.socket_id.clone(),
                input_route_epoch: command.input_route_epoch,
            }),
            session_id: command.session_id,
            input_seq: command.input_seq,
            data: command.data,
            socket_generation: Some(socket.socket_id.clone()),
            audited: true,
            deadline: None,
        },
    );
    let link = Arc::clone(link);
    tokio::spawn(async move {
        let result = outcome.await;
        tracing::debug!(session_id = %result.session_id, input_seq = result.input_seq,
            status = result.status.as_str(), written_bytes = result.written_bytes,
            "sync terminal input settled");
        let frame = match result.status {
            TerminalWriteStatus::Accepted => input_accepted_frame(
                &result.session_id,
                result.input_seq,
                domain_generation,
                result.written_bytes,
            ),
            TerminalWriteStatus::Rejected => input_rejected_frame(
                &result.session_id,
                result.input_seq,
                domain_generation,
                &result.reason,
            ),
            TerminalWriteStatus::Ambiguous => input_ambiguous_frame(
                &result.session_id,
                result.input_seq,
                domain_generation,
                result.written_bytes,
                &result.reason,
            ),
        };
        reply(&link, frame);
    });
}
