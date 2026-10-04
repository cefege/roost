//! The lane's pre-warm surface: which workers a peer is held ready for before
//! any view wants one, and the three instructions that change it. A child of
//! `lane` because it reads the machine table directly; the rules each
//! instruction runs are `Signalling`'s, in `super::super::signaling_demand`.
//! Called by `handle_terminal::reconcile_prewarm`, the one place that decides
//! the selection.

use std::collections::BTreeSet;

use super::CarrierLane;
use crate::client::carriers::SignallingInput;
use crate::effect::Effect;

impl CarrierLane {
    /// Hold a granted peer ready on this worker for `session_ids`, with no view
    /// asking yet. An unchanged set is no instruction at all, so a reconcile
    /// that changes nothing costs the machine nothing.
    pub fn set_prewarm(
        &mut self,
        worker_fp: &str,
        session_ids: BTreeSet<String>,
        now_ms: u64,
        out: &mut Vec<Effect>,
    ) {
        let unchanged = match self.machines.get(worker_fp) {
            Some(machine) => machine.prewarm_sessions == session_ids,
            None => session_ids.is_empty(),
        };
        if unchanged {
            return;
        }
        self.observe(
            worker_fp,
            SignallingInput::Prewarm {
                session_ids,
                now_ms,
            },
            out,
        );
    }

    /// Stop pre-warming this worker and keep whatever peer it brought up: the
    /// document stopped asking, and a peer is cheaper to keep than to
    /// renegotiate when it asks again.
    pub fn clear_prewarm(&mut self, worker_fp: &str, now_ms: u64, out: &mut Vec<Effect>) {
        self.set_prewarm(worker_fp, BTreeSet::new(), now_ms, out);
    }

    /// Stop pre-warming this worker, and close its peer when no view wants it,
    /// which hands that peer's slot under the document cap to whichever worker
    /// the selection preferred.
    pub fn release_prewarm(&mut self, worker_fp: &str, now_ms: u64, out: &mut Vec<Effect>) {
        if self.prewarm_sessions(worker_fp).is_none() {
            return;
        }
        self.observe(worker_fp, SignallingInput::PrewarmReleased { now_ms }, out);
    }

    /// The workers a peer is held ready for.
    pub fn prewarmed_workers(&self) -> impl Iterator<Item = &str> {
        self.machines
            .iter()
            .filter(|(_, machine)| !machine.prewarm_sessions.is_empty())
            .map(|(worker_fp, _)| worker_fp.as_str())
    }

    /// The sessions this worker is pre-warmed for, or `None` when it is not.
    pub fn prewarm_sessions(&self, worker_fp: &str) -> Option<&BTreeSet<String>> {
        self.machines
            .get(worker_fp)
            .map(|machine| &machine.prewarm_sessions)
            .filter(|sessions| !sessions.is_empty())
    }

    /// The workers some live view wants a session on. Each holds, or is about
    /// to hold, a peer whatever the pre-warm selection decides.
    pub fn workers_with_view_demand(&self) -> impl Iterator<Item = &str> {
        self.machines
            .iter()
            .filter(|(_, machine)| machine.active_views > 0)
            .map(|(worker_fp, _)| worker_fp.as_str())
    }
}
