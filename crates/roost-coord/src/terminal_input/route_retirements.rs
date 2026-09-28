//! Browser-route retirements that could not reach their worker: a Sync socket
//! closed while the worker it had claimed routes on was unroutable. The route
//! result owner records them here; a worker becoming ready flushes them, but
//! only to the SAME process epoch -- a restarted worker already lost the route
//! table the retirement would mutate, so it is dropped instead.
//! Ports `apps/coord/src/terminal/input/terminal-input-route-retirements.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use roost_protocol::wire::WorkerFp;

use crate::coord_core::worker_handle::WorkerRegistry;
use crate::terminal_input::route_sender::send_terminal_input_route_connection_closed;

/// The retained retirements, by `(worker, process epoch)`.
#[derive(Debug, Default)]
pub struct TerminalInputRouteRetirements {
    pending: Mutex<BTreeMap<(WorkerFp, String), BTreeSet<String>>>,
}

impl TerminalInputRouteRetirements {
    /// Nothing retained.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Retire one socket's routes on one worker epoch: sent now when that
    /// exact epoch is routable, dropped when a different epoch replaced it,
    /// retained otherwise.
    pub fn retire(
        &self,
        workers: &WorkerRegistry,
        worker_fp: &WorkerFp,
        worker_epoch: &str,
        connection_id: &str,
    ) {
        let worker = workers.current_routable(worker_fp);
        if let Some(worker) = &worker {
            let same_epoch = worker.process_epoch.as_deref() == Some(worker_epoch);
            if same_epoch
                && send_terminal_input_route_connection_closed(
                    workers,
                    worker,
                    worker_epoch,
                    connection_id,
                )
            {
                tracing::debug!(%worker_fp, worker_epoch, connection_id,
                    "terminal input routes retired on the worker");
                return;
            }
            if !same_epoch {
                self.drop_replaced_epochs(worker_fp, worker.process_epoch.as_deref());
                return;
            }
        }
        tracing::info!(%worker_fp, worker_epoch, connection_id,
            "terminal input route retirement retained until the worker is ready");
        self.lock()
            .entry((worker_fp.clone(), worker_epoch.to_owned()))
            .or_default()
            .insert(connection_id.to_owned());
    }

    /// Send every retirement retained for the worker's current epoch, keeping
    /// any the transport refused.
    pub fn flush(&self, workers: &WorkerRegistry, worker_fp: &WorkerFp) {
        let Some(worker) = workers.current_routable(worker_fp) else {
            return;
        };
        let Some(epoch) = worker.process_epoch.clone() else {
            return;
        };
        self.drop_replaced_epochs(worker_fp, Some(&epoch));
        let key = (worker_fp.clone(), epoch);
        let Some(connections) = self.lock().remove(&key) else {
            return;
        };
        let unsent: BTreeSet<String> = connections
            .into_iter()
            .filter(|connection_id| {
                !send_terminal_input_route_connection_closed(
                    workers,
                    &worker,
                    &key.1,
                    connection_id,
                )
            })
            .collect();
        tracing::info!(%worker_fp, worker_epoch = %key.1, unsent = unsent.len(),
            "retained terminal input route retirements flushed");
        if !unsent.is_empty() {
            self.lock().entry(key).or_default().extend(unsent);
        }
    }

    fn drop_replaced_epochs(&self, worker_fp: &WorkerFp, current_epoch: Option<&str>) {
        self.lock()
            .retain(|(fp, epoch), _| fp != worker_fp || current_epoch == Some(epoch.as_str()));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<(WorkerFp, String), BTreeSet<String>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
