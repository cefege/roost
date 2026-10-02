//! The disposable peer fault controls a smoke harness drives: the two sockets
//! `roost worker` connects to when given its fault flags, and the in-process
//! fault state their commands mutate. Built by `runtime::owners` from
//! `WorkerBoot::fault_sockets`; compiled only with the `smoke` feature, so a
//! release worker has neither the flags nor the hooks. Ports
//! `smoke/terminal/stack-direct-input-worker.ts` and the state half of
//! `apps/worker/src/terminal/peer/terminal-peer-test-faults.ts`.

mod commands;
mod control_socket;
mod input_hold;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::runtime::Handle;

pub use input_hold::DirectInputHold;

use crate::local_terminal::LocalTerminalDoor;
use crate::peer::OfferFaultSlot;
use crate::runtime::WorkerBoot;

/// Where the harness listens, and the label this worker says hello as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultSockets {
    /// `--terminal-peer-fault-socket`: one connection carrying commands.
    pub peer_fault_socket: PathBuf,
    /// `--direct-input-hold-socket`: one connection per authenticated peer
    /// input, asking whether to write it.
    pub input_hold_socket: PathBuf,
    /// `ROOST_WORKER_LABEL`, which the harness addresses commands by.
    pub worker_label: String,
}

/// The fault state every command mutates and every hook reads (v2
/// `TerminalPeerTestFaultState`).
#[derive(Debug, Default)]
pub struct PeerFaultState {
    offer: Arc<OfferFaultSlot>,
}

impl PeerFaultState {
    /// v2 `dispose`: the harness went away, so nothing it armed may outlive it.
    fn dispose(&self) {
        self.offer.consume();
        tracing::info!("the terminal peer fault state was cleared");
    }
}

/// The fault controls of one worker: its sockets and the state they drive.
#[derive(Debug)]
pub struct FaultControls {
    sockets: FaultSockets,
    state: Arc<PeerFaultState>,
}

impl FaultControls {
    /// The controls `roost worker` was launched with, their hooks attached to
    /// the direct path; `None` for a worker given no fault sockets.
    pub fn attach(boot: &WorkerBoot, door: &LocalTerminalDoor) -> Option<Self> {
        let sockets = boot.fault_sockets.clone()?;
        door.sockets()
            .attach_test_faults(DirectInputHold::new(sockets.input_hold_socket.clone()));
        Some(Self {
            sockets,
            state: Arc::new(PeerFaultState::default()),
        })
    }

    /// The one-shot offer fault the peer owner consumes at `offer()`.
    pub fn offer_slot(&self) -> Arc<OfferFaultSlot> {
        Arc::clone(&self.state.offer)
    }

    /// Connect to the harness's command socket and serve it until it closes.
    /// Called once every owner a command reaches is built.
    pub fn serve_commands(&self, runtime: &Handle) {
        runtime.spawn(control_socket::serve_fault_commands(
            self.sockets.peer_fault_socket.clone(),
            self.sockets.worker_label.clone(),
            Arc::clone(&self.state),
        ));
    }
}
