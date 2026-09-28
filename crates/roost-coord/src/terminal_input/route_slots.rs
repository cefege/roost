//! The bounded slot table behind the input-route result owner: one slot per
//! browser nonce, its binding to one worker control, the coordinator request
//! id that control was sent under, and every worker epoch a socket reached.
//! Mutated only under `TerminalInputRouteResults`' lock; it performs no I/O.
//! Ports the slot bookkeeping of
//! `apps/coord/src/terminal/input/terminal-input-route-results.ts`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use roost_protocol::wire::WorkerFp;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_input::route_contract::{
    MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET, RouteControlRefusal, is_terminal_route_identifier,
};
use crate::terminal_input::route_state::{
    PendingRouteControl, RouteControlKind, RouteControlSlot, RouteControlSlotId,
    connection_nonce_key,
};

/// What a claim binds into its slot beyond the worker.
pub(super) type ClaimBinding<'a> = Option<(&'a str, u64)>;

/// Every map the result owner keeps.
#[derive(Debug, Default)]
pub(super) struct RouteControlTable {
    next_identity: u64,
    slots: HashMap<RouteControlSlotId, RouteControlSlot>,
    slots_by_connection: HashMap<String, BTreeSet<RouteControlSlotId>>,
    slots_by_nonce: HashMap<(String, String), RouteControlSlotId>,
    pending_by_outer: HashMap<String, RouteControlSlotId>,
    /// Every worker epoch a socket ever sent a control to, so its close can
    /// retire the routes the worker holds for it.
    workers_by_connection: HashMap<String, BTreeMap<WorkerFp, String>>,
}

impl RouteControlTable {
    /// Reserve one bounded slot for a browser nonce.
    pub(super) fn reserve(
        &mut self,
        connection_id: &str,
        browser_request_id: &str,
    ) -> Result<RouteControlSlotId, RouteControlRefusal> {
        if !is_terminal_route_identifier(connection_id)
            || !is_terminal_route_identifier(browser_request_id)
        {
            return Err(RouteControlRefusal::InputRouteUnavailable);
        }
        let nonce = connection_nonce_key(connection_id, browser_request_id);
        let held = self
            .slots_by_connection
            .get(connection_id)
            .map_or(0, BTreeSet::len);
        if held >= MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET
            || self.slots_by_nonce.contains_key(&nonce)
        {
            return Err(RouteControlRefusal::RouteClaimBusy);
        }
        let slot = self.mint();
        self.slots.insert(
            slot,
            RouteControlSlot {
                connection_id: connection_id.to_owned(),
                browser_request_id: browser_request_id.to_owned(),
                pending: None,
            },
        );
        self.slots_by_connection
            .entry(connection_id.to_owned())
            .or_default()
            .insert(slot);
        self.slots_by_nonce.insert(nonce, slot);
        Ok(slot)
    }

    /// The slot a control runs in: the caller's reservation when it still
    /// belongs to this nonce, or a fresh one.
    pub(super) fn require(
        &mut self,
        connection_id: &str,
        browser_request_id: &str,
        reserved: Option<RouteControlSlotId>,
    ) -> Result<RouteControlSlotId, RouteControlRefusal> {
        let Some(slot) = reserved else {
            return self.reserve(connection_id, browser_request_id);
        };
        match self.slots.get(&slot) {
            Some(entry)
                if entry.connection_id == connection_id
                    && entry.browser_request_id == browser_request_id =>
            {
                Ok(slot)
            }
            _ => Err(RouteControlRefusal::InputRouteUnavailable),
        }
    }

    /// Bind one worker control into an unbound slot.
    pub(super) fn bind(
        &mut self,
        slot: RouteControlSlotId,
        worker: &Arc<WorkerHandle>,
        worker_epoch: &str,
        kind: RouteControlKind,
        claim: ClaimBinding<'_>,
    ) -> Result<PendingRouteControl, RouteControlRefusal> {
        let binding = self.mint();
        let Some(entry) = self
            .slots
            .get_mut(&slot)
            .filter(|entry| entry.pending.is_none())
        else {
            return Err(RouteControlRefusal::InputRouteUnavailable);
        };
        let pending = PendingRouteControl {
            kind,
            binding,
            worker: Arc::clone(worker),
            worker_fp: worker.worker_fp.clone(),
            worker_epoch: worker_epoch.to_owned(),
            connection_generation: worker.connection_generation.clone(),
            session_id: claim.map(|(session_id, _)| session_id.to_owned()),
            revision: claim.map(|(_, revision)| revision),
            outer_request_id: None,
        };
        entry.pending = Some(pending.clone());
        Ok(pending)
    }

    /// Remember that a socket reached this worker epoch.
    pub(super) fn track_worker(&mut self, connection_id: &str, worker_fp: &WorkerFp, epoch: &str) {
        self.workers_by_connection
            .entry(connection_id.to_owned())
            .or_default()
            .insert(worker_fp.clone(), epoch.to_owned());
    }

    /// Record the coordinator request id a bound control is about to be sent
    /// under. Refused when the binding is gone, already sent, or the id would
    /// shadow the browser's own nonce or another pending control.
    pub(super) fn install_outer(
        &mut self,
        slot: RouteControlSlotId,
        binding: u64,
        request_id: &str,
    ) -> Result<(), String> {
        let Self {
            slots,
            pending_by_outer,
            ..
        } = self;
        let gone = || "terminal route control is no longer pending".to_owned();
        let entry = slots.get_mut(&slot).ok_or_else(gone)?;
        let browser_nonce = entry.browser_request_id.as_str();
        let pending = entry
            .pending
            .as_mut()
            .filter(|pending| {
                pending.binding == binding
                    && pending.outer_request_id.is_none()
                    && request_id != browser_nonce
            })
            .ok_or_else(gone)?;
        if pending_by_outer.contains_key(request_id) {
            return Err("terminal route control request is already pending".to_owned());
        }
        pending.outer_request_id = Some(request_id.to_owned());
        pending_by_outer.insert(request_id.to_owned(), slot);
        Ok(())
    }

    /// Whether this exact binding still occupies its slot.
    pub(super) fn is_bound(&self, slot: RouteControlSlotId, binding: u64) -> bool {
        self.slots
            .get(&slot)
            .and_then(|entry| entry.pending.as_ref())
            .is_some_and(|pending| pending.binding == binding)
    }

    /// The pending control an outer request id names, while it is correlated.
    pub(super) fn pending_for_outer(&self, outer: &str) -> Option<&PendingRouteControl> {
        let slot = self.pending_by_outer.get(outer)?;
        self.slots.get(slot)?.pending.as_ref()
    }

    /// Stop correlating an outer request id: its result has been taken.
    pub(super) fn forget_outer(&mut self, outer: &str) {
        self.pending_by_outer.remove(outer);
    }

    /// Drop one slot from every index, handing back what was bound into it.
    pub(super) fn release(&mut self, slot: RouteControlSlotId) -> Option<PendingRouteControl> {
        let entry = self.slots.remove(&slot)?;
        if let Some(outer) = entry
            .pending
            .as_ref()
            .and_then(|pending| pending.outer_request_id.as_ref())
            && self.pending_by_outer.get(outer) == Some(&slot)
        {
            self.pending_by_outer.remove(outer);
        }
        if let Some(held) = self.slots_by_connection.get_mut(&entry.connection_id) {
            held.remove(&slot);
            if held.is_empty() {
                self.slots_by_connection.remove(&entry.connection_id);
            }
        }
        let nonce = connection_nonce_key(&entry.connection_id, &entry.browser_request_id);
        if self.slots_by_nonce.get(&nonce) == Some(&slot) {
            self.slots_by_nonce.remove(&nonce);
        }
        entry.pending
    }

    /// Release every slot a socket holds and forget every worker it reached.
    pub(super) fn release_connection(
        &mut self,
        connection_id: &str,
    ) -> (Vec<PendingRouteControl>, BTreeMap<WorkerFp, String>) {
        let slots: Vec<RouteControlSlotId> = self
            .slots_by_connection
            .get(connection_id)
            .map(|held| held.iter().copied().collect())
            .unwrap_or_default();
        let released = slots
            .into_iter()
            .filter_map(|slot| self.release(slot))
            .collect();
        let workers = self
            .workers_by_connection
            .remove(connection_id)
            .unwrap_or_default();
        (released, workers)
    }

    /// Release every slot bound to this exact worker generation.
    pub(super) fn release_for_worker(
        &mut self,
        worker: &Arc<WorkerHandle>,
    ) -> Vec<PendingRouteControl> {
        let slots: Vec<RouteControlSlotId> = self
            .slots
            .iter()
            .filter(|(_, entry)| {
                entry
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(&pending.worker, worker))
            })
            .map(|(slot, _)| *slot)
            .collect();
        slots
            .into_iter()
            .filter_map(|slot| self.release(slot))
            .collect()
    }

    fn mint(&mut self) -> u64 {
        self.next_identity += 1;
        self.next_identity
    }
}
