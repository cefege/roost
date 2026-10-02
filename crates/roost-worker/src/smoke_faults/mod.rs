//! The disposable peer fault controls a smoke harness drives: the two sockets
//! `roost worker` connects to when given its fault flags, and the in-process
//! fault state their commands mutate. Built by `runtime::owners` from
//! `WorkerBoot::fault_sockets`; compiled only with the `smoke` feature, so a
//! release worker has neither the flags nor the hooks. Ports
//! `smoke/terminal/stack-direct-input-worker.ts` and the state half of
//! `apps/worker/src/terminal/peer/terminal-peer-test-faults.ts`.

mod admission_hold;
mod commands;
mod control_socket;
mod history_hold;
mod input_hold;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::runtime::Handle;

pub use admission_hold::AdmissionHolds;
pub use history_hold::HistoryHold;
pub use input_hold::DirectInputHold;

use crate::local_terminal::{LocalTerminalDoor, LocalTerminalGrantStore};
use crate::peer::{DirectTerminal, PeerTestFaults};
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

/// The faults `crate::local_terminal` reads on the direct path (v2
/// `LocalTerminalSocketTestFaults`).
#[derive(Debug)]
pub struct DirectPathFaults {
    /// Asked before an authenticated peer's input is admitted.
    pub input_hold: DirectInputHold,
    /// Asked before a peer's scrollback read is re-authorized and sent.
    pub history: HistoryHold,
    drop_next_input_result: AtomicBool,
}

impl DirectPathFaults {
    /// v2 `dropNextPeerInputResult`.
    fn arm_input_result_drop(&self) {
        self.drop_next_input_result.store(true, Ordering::SeqCst);
        tracing::info!("the next peer input result will be withheld");
    }

    /// v2 `consumeDroppedInputResult`: whether to withhold this one accepted
    /// result. One-shot.
    pub fn consume_input_result_drop(&self) -> bool {
        self.drop_next_input_result.swap(false, Ordering::SeqCst)
    }
}

/// The owners a fault command reaches, built before the harness is served.
#[derive(Debug)]
pub struct FaultTargets {
    pub direct: Arc<DirectTerminal>,
    pub grants: LocalTerminalGrantStore,
    pub admission: AdmissionHolds,
}

/// The fault state every command mutates and every hook reads (v2
/// `TerminalPeerTestFaultState`), with the owners commands reach it through.
#[derive(Debug)]
pub struct PeerFaultState {
    peer: Arc<PeerTestFaults>,
    direct_path: Arc<DirectPathFaults>,
    targets: FaultTargets,
}

impl PeerFaultState {
    /// v2 `dispose`: the harness went away, so nothing it armed or holds may
    /// outlive it.
    fn dispose(&self) {
        self.targets.admission.release_all();
        self.direct_path.history.clear();
        self.peer.clear();
        self.direct_path
            .drop_next_input_result
            .store(false, Ordering::SeqCst);
        tracing::info!("the terminal peer fault state was cleared");
    }
}

/// The fault controls of one worker: its sockets and the state they drive.
#[derive(Debug)]
pub struct FaultControls {
    sockets: FaultSockets,
    peer: Arc<PeerTestFaults>,
    direct_path: Arc<DirectPathFaults>,
}

impl FaultControls {
    /// The controls `roost worker` was launched with, their hooks attached to
    /// the direct path; `None` for a worker given no fault sockets.
    pub fn attach(boot: &WorkerBoot, door: &LocalTerminalDoor) -> Option<Self> {
        let sockets = boot.fault_sockets.clone()?;
        let direct_path = Arc::new(DirectPathFaults {
            input_hold: DirectInputHold::new(sockets.input_hold_socket.clone()),
            history: HistoryHold::default(),
            drop_next_input_result: AtomicBool::new(false),
        });
        door.sockets().attach_test_faults(Arc::clone(&direct_path));
        Some(Self {
            sockets,
            peer: Arc::new(PeerTestFaults::default()),
            direct_path,
        })
    }

    /// The faults the peer owner, its connections and ports read.
    pub fn peer_faults(&self) -> Arc<PeerTestFaults> {
        Arc::clone(&self.peer)
    }

    /// Connect to the harness's command socket and serve it until it closes.
    /// Called once every owner a command reaches is built.
    pub fn serve_commands(self, runtime: &Handle, targets: FaultTargets) {
        let state = PeerFaultState {
            peer: self.peer,
            direct_path: self.direct_path,
            targets,
        };
        runtime.spawn(control_socket::serve_fault_commands(
            self.sockets.peer_fault_socket,
            self.sockets.worker_label,
            Arc::new(state),
        ));
    }
}

/// Poison is not a fault the harness can act on; its state stays usable.
fn lock_state<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
