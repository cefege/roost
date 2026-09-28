//! The view hub as the workers domain and the session bus see it: a retired
//! worker's sessions and a closed session release their views and their screen
//! replica, and a booted coordinator sweeps lapsed leases.
//!
//! Split out of `mod.rs` for the size cap. Ports `workerRetired`, `closeSession`,
//! the session-bus subscription and the sweep interval of
//! `apps/coord/src/terminal/view/terminal-view-hub.ts:256-260,346-353`.

use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use roost_protocol::viewport::{TERMINAL_VIEW_SWEEP_MS, TerminalGeometry};
use roost_protocol::wire::{SessionId, WorkerFp};

use crate::coord_core::seams::TerminalViewLifecycle;
use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::SessionBusMessage;
use crate::terminal_screen::ScreenHub;

use super::TerminalViewHub;

/// The screen replica a closed session is released from. Weak: the replica
/// reaches the view hub through its repair sink, and a strong edge back would
/// be a cycle.
#[derive(Debug, Default)]
pub struct ScreenRelease(RwLock<Option<Weak<ScreenHub>>>);

impl ScreenRelease {
    /// Drop a closed session's replica, so its resident bytes go back to the
    /// pool and every socket watching it loses its lane.
    pub(super) fn drop_session(&self, session_id: &SessionId) {
        let screens = self
            .0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(screens) = screens {
            screens.drop_session(session_id);
        }
    }
}

impl TerminalViewHub {
    /// Install the screen replica a closed session is released from.
    pub fn set_screens(&self, screens: Weak<ScreenHub>) {
        *self
            .screens
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(screens);
    }

    /// Close every session the session bus reports closed, for as long as the
    /// returned handle lives; `serve` holds it for the process.
    pub fn subscribe_session_close(
        self: &Arc<Self>,
        buses: &Buses,
    ) -> Subscription<SessionBusMessage> {
        let views = Arc::clone(self);
        buses.session_bus.subscribe(move |message| {
            if message.event.kind_name() != "closed" {
                return;
            }
            if let Some(session_id) = message.event.session_id() {
                tracing::debug!(%session_id, "terminal views released for a closed session");
                views.close_session(session_id, crate::serve::now_ms().max(0) as u64);
            }
        })
    }
}

impl TerminalViewLifecycle for TerminalViewHub {
    fn notify_worker_retired(&self, worker_fp: &WorkerFp, session_ids: &[SessionId]) {
        self.owners.drop_owner(worker_fp);
        let now_ms = crate::serve::now_ms().max(0) as u64;
        for session_id in session_ids {
            // The worker is gone, so nothing can answer for these sessions any
            // more: their membership and replica are released rather than left
            // to a lease that will never be renewed.
            self.close_session(session_id, now_ms);
        }
        tracing::info!(
            worker_fp = %worker_fp,
            sessions = session_ids.len(),
            "terminal views released for a retired worker"
        );
    }

    fn effective_geometry(&self, session_id: &SessionId) -> Option<TerminalGeometry> {
        let now_ms = crate::serve::now_ms().max(0) as u64;
        if let Some(row) = self.owners.row(session_id) {
            return row.effective;
        }
        self.session_geometry(session_id, now_ms)
    }
}

/// Sweep lapsed leases and lapsed park graces on an interval, for a booted
/// coordinator; `serve` calls this beside the other schedulers.
pub fn spawn_view_sweep(views: Arc<TerminalViewHub>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(TERMINAL_VIEW_SWEEP_MS));
        loop {
            ticker.tick().await;
            let now_ms = crate::serve::now_ms().max(0) as u64;
            views.sweep(now_ms);
        }
    });
}
