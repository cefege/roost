//! Direct terminal input for `LocalTerminalSockets`: both carriers share the
//! live grant/route checks and the input-work accounting here. Everything up to
//! keeper admission runs in receive order inside `on_message`; only the wait for
//! the keeper's answer runs on its own task, and the reservation is released
//! after the answer is sent. Ports
//! `apps/worker/src/local-door/local-terminal-socket-input.ts`.

use std::sync::Arc;

use roost_keeper::codec::KEEPER_MAX_INPUT_BYTES;
use roost_proto::InputCommand;
use roost_protocol::wire::brand::SessionId;

use super::authority::{DirectPortAuthority, DirectPortBudget, PortSession};
use super::delivery::input_result_frame;
use super::sockets::LocalTerminalSockets;
use crate::session::input_write::WorkerInputResult;
use crate::terminal_input::InputWorkOrigin;

const SESSION_UNAVAILABLE: &str = "terminal session is unavailable";
const OVERSIZED: &str = "input exceeds 64 KiB";
#[cfg(feature = "smoke")]
const TEST_HOOK_REJECTED: &str = "terminal input test hook rejected";

impl LocalTerminalSockets {
    /// Run one decoded input frame from an authenticated port.
    pub(super) fn start_input(&self, session: &Arc<PortSession>, command: InputCommand) {
        #[cfg(feature = "smoke")]
        let Some(command) = self.hold_peer_input(session, command) else {
            return;
        };
        self.admit_input(session, command);
    }

    /// Check, reserve and write one input; everything up to keeper admission
    /// runs on the caller.
    pub(super) fn admit_input(&self, session: &Arc<PortSession>, mut command: InputCommand) {
        if !self.is_session_authorized(session, &command.session_id) {
            self.send_input_result(session, &command, &rejected(SESSION_UNAVAILABLE));
            return;
        }
        if command.data.len() > KEEPER_MAX_INPUT_BYTES as usize {
            self.send_input_result(session, &command, &rejected(OVERSIZED));
            return;
        }
        let origin = InputWorkOrigin::Direct {
            port_id: session.socket_id().to_owned(),
        };
        let reservation = match self.work_budget.reserve_input(&origin, command.data.len()) {
            Ok(reservation) => reservation,
            Err(reason) => {
                self.send_input_result(session, &command, &rejected(reason));
                return;
            }
        };
        let Ok(session_id) = SessionId::try_from(command.session_id.as_str()) else {
            self.send_input_result(session, &command, &rejected(SESSION_UNAVAILABLE));
            return;
        };
        let deps = &self.authorization;
        let authority = DirectPortAuthority {
            deps: Arc::clone(deps),
            session: Arc::clone(session),
            session_id: command.session_id.clone(),
            input_route_epoch: command.input_route_epoch.clone(),
        };
        let written = self.manager.write_terminal_input(
            &session_id,
            command.input_seq,
            std::mem::take(&mut command.data),
            Some(Box::new(DirectPortBudget::new(deps, session))),
            Some(Box::new(authority)),
        );
        let sockets = self.self_handle.clone();
        let session = Arc::clone(session);
        self.runtime.spawn(async move {
            let result = written.await;
            if let Some(sockets) = sockets.upgrade() {
                sockets.send_input_result(&session, &command, &result);
            }
            drop(reservation);
        });
    }

    fn send_input_result(
        &self,
        session: &Arc<PortSession>,
        command: &InputCommand,
        result: &WorkerInputResult,
    ) {
        self.send_control(
            session,
            input_result_frame(session.generation(), command, result),
        );
    }
}

#[cfg(feature = "smoke")]
impl LocalTerminalSockets {
    /// A held input the harness dropped: answered, never written.
    pub(super) fn reject_held_input(&self, session: &Arc<PortSession>, command: &InputCommand) {
        self.send_input_result(session, command, &rejected(TEST_HOOK_REJECTED));
    }
}

fn rejected(reason: &str) -> WorkerInputResult {
    WorkerInputResult::Rejected {
        reason: reason.to_owned(),
    }
}
