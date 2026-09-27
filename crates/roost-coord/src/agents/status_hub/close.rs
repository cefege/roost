//! What a session close does to the status table: fence it, release its waits,
//! publish the synthetic deletion, and let the fence expire.
//!
//! Split out of `status_hub.rs` because it is a second concept with a second
//! set of rules, and because it is the half of the hub whose lifetime policy is
//! about TIME rather than about admission: a closed session's fence and its
//! admission order must expire together, or the table grows one entry per closed
//! session for the life of the process.
//!
//! Owned by `super`, which owns the tables this reads and writes.

use roost_observability::LogFields;
use roost_protocol::wire::{AgentStatusFields, AgentStatusUpdate, SessionId};

use crate::agents::status_wait::AGENT_STATUS_WAIT_MAX_TIMEOUT_MS;
use crate::events::bus_domains::Buses;

use super::{HubState, MAX_SAFE_INTEGER};

impl HubState {
    /// A session closed.
    ///
    /// The waiters are released FIRST: a wait that resolved as
    /// `occupant_changed` would tell the client its agent had been replaced when
    /// the session had simply gone away, and `session_closed` is the answer it
    /// can act on.
    pub(super) fn close_session(&self, buses: &Buses, session_id: &SessionId) {
        self.sweep_tombstones();
        self.lock(&self.tables)
            .tombstones
            .insert(session_id.clone(), (self.now_ms)());
        self.waits.close_session(session_id);
        self.push.cancel(session_id);
        let current = self.lock(&self.tables).active.remove(session_id);
        let Some(current) = current else {
            return;
        };
        // The deletion the coordinator publishes on the session's behalf. It
        // names the exact identity it retires and carries one revision past the
        // last one seen, so a worker that resends the frame it sent a moment ago
        // is refused rather than allowed to recreate the row.
        let inactive = AgentStatusUpdate {
            common: AgentStatusFields {
                revision: current
                    .common
                    .revision
                    .saturating_add(1)
                    .min(MAX_SAFE_INTEGER),
                updated_at: (self.now_ms)().max(current.common.updated_at),
                ..current.common.clone()
            },
            active: false,
        };
        self.lock(&self.tables)
            .order
            .entry(session_id.clone())
            .or_default()
            .record_close(&current, inactive.common.revision);
        buses.agent_status_bus.publish(inactive);
    }

    /// Drop close fences nothing may consult any more.
    ///
    /// A fence and its admission order expire TOGETHER, both once the longest
    /// admissible wait has elapsed: a tombstone older than that cannot still be
    /// pinning a waiter, and keeping either map's entry forever would make the
    /// hub's footprint a function of uptime rather than of how many agents are
    /// running. Swept lazily on the next frame, so a coordinator with no traffic
    /// does no work for fences nobody will ask about.
    pub(super) fn sweep_tombstones(&self) {
        let expired_at_or_before = (self.now_ms)() - AGENT_STATUS_WAIT_MAX_TIMEOUT_MS as i64;
        let mut swept = 0_u64;
        {
            let mut tables = self.lock(&self.tables);
            let stale: Vec<SessionId> = tables
                .tombstones
                .iter()
                .filter(|(_, closed_at_ms)| **closed_at_ms <= expired_at_or_before)
                .map(|(session_id, _)| session_id.clone())
                .collect();
            for session_id in stale {
                tables.tombstones.remove(&session_id);
                tables.order.remove(&session_id);
                swept += 1;
            }
        }
        if swept > 0 {
            roost_observability::log::info(
                "agents.status",
                "tombstones_swept",
                LogFields::new().set("count", swept),
            );
        }
    }
}
