//! The production owner-view transport: the relay's writes to the worker link.
//!
//! Ported from `apps/coord/src/terminal/view/worker-send-terminal-view.ts`.
//! It exists as its own file because it is the one place the view hub names the
//! workers domain, and the direction of that dependency is worth seeing in a
//! single screen: everything else here talks to the sink, the owner index and
//! the membership machine, never to a worker handle.

use roost_proto::__buffa::oneof::d_terminal_view_relay::Command as RelayCommand;
use roost_proto::{
    DTerminalViewRelay, DTerminalViewSocketClosed, TerminalResyncCommand, TerminalViewCommand,
};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use std::sync::Arc;

use crate::coord_core::worker_handle::WorkerRegistry;
use crate::workers::send::{SendOutcome, send_frame};

use super::relay::{OwnerViewTransport, RelayIdentity};

/// The control window a relayed command is budgeted, in milliseconds.
///
/// v2 quotes the legacy stream-state hop's window here
/// (`worker-send.ts:117`): the coordinator holds no waiter for a relayed
/// command, because the worker answers with its own view state whenever its
/// reconciliation settles, so the whole control window is available to it.
pub const TERMINAL_VIEW_RELAY_BUDGET_MS: u32 = 8_000;

/// The owner-view transport over a live worker registry.
#[derive(Debug, Clone)]
pub struct WorkerLinkViewTransport {
    registry: Arc<WorkerRegistry>,
}

impl WorkerLinkViewTransport {
    /// A transport that resolves each worker to its current generation.
    #[must_use]
    pub fn new(registry: Arc<WorkerRegistry>) -> Self {
        Self { registry }
    }
}

impl OwnerViewTransport for WorkerLinkViewTransport {
    fn relay_view(
        &self,
        worker_fp: &WorkerFp,
        identity: &RelayIdentity,
        command: &TerminalViewCommand,
    ) -> bool {
        let frame = CoordWorkerDownstream::TerminalViewRelay(DTerminalViewRelay {
            socket_id: identity.socket_id.clone(),
            viewer_key: identity.viewer_key.clone(),
            device_fingerprint: identity.device_fingerprint.clone(),
            budget_ms: TERMINAL_VIEW_RELAY_BUDGET_MS,
            command: Some(RelayCommand::View(Box::new(command.clone()))),
            __buffa_unknown_fields: Default::default(),
        });
        admitted(send_frame(&self.registry, worker_fp, frame))
    }

    fn relay_resync(
        &self,
        worker_fp: &WorkerFp,
        identity: &RelayIdentity,
        command: &TerminalResyncCommand,
    ) -> bool {
        let frame = CoordWorkerDownstream::TerminalViewRelay(DTerminalViewRelay {
            socket_id: identity.socket_id.clone(),
            viewer_key: identity.viewer_key.clone(),
            device_fingerprint: identity.device_fingerprint.clone(),
            budget_ms: TERMINAL_VIEW_RELAY_BUDGET_MS,
            command: Some(RelayCommand::Resync(Box::new(command.clone()))),
            __buffa_unknown_fields: Default::default(),
        });
        admitted(send_frame(&self.registry, worker_fp, frame))
    }

    fn socket_closed(&self, worker_fp: &WorkerFp, socket_id: &str) -> bool {
        let frame = CoordWorkerDownstream::TerminalViewSocketClosed(DTerminalViewSocketClosed {
            socket_id: socket_id.to_owned(),
            __buffa_unknown_fields: Default::default(),
        });
        admitted(send_frame(&self.registry, worker_fp, frame))
    }
}

/// Whether a relay reached the socket, with the refusal named when it did not.
fn admitted(outcome: SendOutcome) -> bool {
    match outcome {
        SendOutcome::Admitted { .. } => true,
        SendOutcome::Refused(refusal) => {
            tracing::warn!(%refusal, "a terminal view relay did not reach its owner");
            false
        }
    }
}
