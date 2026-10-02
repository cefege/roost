//! The smoke harness's hooks on the direct terminal path: the per-input hold
//! an authenticated peer's input waits on before admission, and the one
//! accepted input result it may withhold. Attached by `runtime::owners` when
//! `roost worker` was given its fault sockets; read by `input`. Compiled only
//! with the `smoke` feature. Ports v2 `LocalTerminalSocketTestFaults`
//! (`apps/worker/src/local-door/local-terminal-socket-input.ts:45-56,91-95`).

use std::sync::Arc;

use roost_proto::InputCommand;

use super::authority::PortSession;
use super::sockets::LocalTerminalSockets;
use crate::session::input_write::WorkerInputResult;
use crate::smoke_faults::DirectPathFaults;

impl LocalTerminalSockets {
    /// Route every authenticated peer input through the harness's faults.
    pub fn attach_test_faults(&self, faults: Arc<DirectPathFaults>) {
        let attached = self.test_faults.set(faults).is_ok();
        tracing::info!(
            attached,
            "the direct terminal path's smoke fault hooks were attached"
        );
    }

    /// v2 `shouldSendPeerInputResult`: a peer's ACCEPTED result is withheld
    /// once when the harness armed it, after the PTY write already happened.
    pub(super) fn withholds_input_result(
        &self,
        session: &PortSession,
        result: &WorkerInputResult,
    ) -> bool {
        let withheld = session.expected_peer().is_some()
            && matches!(result, WorkerInputResult::Accepted { .. })
            && self
                .test_faults
                .get()
                .is_some_and(|faults| faults.consume_input_result_drop());
        if withheld {
            tracing::info!("a peer input result was withheld by the smoke harness");
        }
        withheld
    }

    /// v2 `onAuthenticatedPeerInput`, asked FIRST — before authorization, size,
    /// work budget and keeper admission — for a peer carrier only. Hands the
    /// command back when no hold applies; otherwise the hold's answer decides,
    /// on its own task, whether the input is admitted or rejected.
    pub(super) fn hold_peer_input(
        &self,
        session: &Arc<PortSession>,
        command: InputCommand,
    ) -> Option<InputCommand> {
        let Some(faults) = self.test_faults.get() else {
            return Some(command);
        };
        if session.expected_peer().is_none() {
            return Some(command);
        }
        let hold = faults.input_hold.clone();
        let sockets = self.self_handle.clone();
        let session = Arc::clone(session);
        self.runtime.spawn(async move {
            let admitted = hold.admits(&command.session_id, command.input_seq).await;
            let Some(sockets) = sockets.upgrade() else {
                return;
            };
            if admitted {
                sockets.admit_input(&session, command);
            } else {
                sockets.reject_held_input(&session, &command);
            }
        });
        None
    }
}
