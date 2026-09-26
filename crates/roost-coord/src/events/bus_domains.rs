//! The one table of buses: thirteen domains, thirteen bounds, one owner.
//!
//! Ported from the singleton block of `apps/coord/src/events/buses.ts:114-180`.
//! v2 exports thirteen module-level `const` buses and every publisher,
//! subscriber and test imports them by name. Here they are fields of one
//! injected value, built once at boot beside the other process singletons.
//!
//! WHY INJECTED AND NOT A LAZY STATIC. `services.rs` states the rule for this
//! crate -- "nothing in this crate reaches for a global" -- and the reason is in
//! v2's own test surface rather than in taste: v2's unit tests construct a
//! barrier with an explicit callback *because no global hub exists at
//! construction time*. A crate-root `static` would make every test in the
//! coordinator share one set of buses, so a test that subscribes would see a
//! stranger's publications and a test that published would perturb the next one.
//! The names are unchanged (`buses.session_bus`), so the port reads like the
//! original at every call site.
//!
//! EVERY BOUND IS v2's, AND NONE OF THEM DELIVERS ANYTHING. A bus's bound sizes
//! its diagnostic ring, and `bus.rs`'s header is explicit that the ring is never
//! replayed to a new subscriber. Three of the numbers carry a reason in the
//! source and are quoted where they appear; the rest are stated as they are,
//! because a bound nobody can justify is a bound nobody will dare to change.

use std::sync::Arc;

use roost_protocol::wire::{AgentStatusUpdate, McpStreamMessage, WorkerPresenceEvent, WorkspaceDelta};

use crate::events::bus::BoundedBus;
use crate::events::bus_messages::{
    AuditRow, LastActivityUpdate, PairRequestDelta, SessionBusMessage, SessionPresenceUpdate,
    SessionTitleUpdate, TaskBusMsg, UiBusMsg, WorkerRoutableSet,
};

/// Every in-process broadcast bus the coordinator owns.
///
/// `Clone` shares: the clone is another handle on the same buses, which is what
/// makes `Arc<Buses>` in the services struct the right way to hand them to a
/// transport.
#[derive(Debug, Clone)]
pub struct Buses {
    /// Durable session fan-out. 256, so a burst of session events leaves a
    /// diagnosable trail rather than the last two of them.
    pub session_bus: BoundedBus<SessionBusMessage>,
    /// Workspace create/update/delete and junction deltas. 64.
    pub workspace_bus: BoundedBus<WorkspaceDelta>,
    /// Task rows, already in their generated wire shape. 64.
    pub task_bus: BoundedBus<TaskBusMsg>,
    /// MCP relay stream messages. 128.
    pub mcp_bus: BoundedBus<McpStreamMessage>,
    /// Volatile coding-agent state; an active update upserts, an inactive one
    /// deletes. 128.
    pub agent_status_bus: BoundedBus<AgentStatusUpdate>,
    /// Pair-request deltas: low traffic, a handful per ceremony. 32.
    pub pair_bus: BoundedBus<PairRequestDelta>,
    /// `audit_log` inserts. 256, quoted from the source: "ring size 256 so
    /// short-lived SSE subscribers don't miss bursts during reconnect" -- the
    /// retention that serves that intent is the seeded snapshot, since the ring
    /// is not replayed, and the bound is kept because it is load-bearing for the
    /// diagnostics that read it.
    pub audit_bus: BoundedBus<AuditRow>,
    /// Worker registration, heartbeat and removal. 128.
    pub presence_bus: BoundedBus<WorkerPresenceEvent>,
    /// Which workers hold a live coordinator socket; the full set every time.
    /// 64.
    pub worker_routable_bus: BoundedBus<WorkerRoutableSet>,
    /// Session-scoped presence, shared by every viewer of a session. 64.
    pub global_presence_bus: BoundedBus<SessionPresenceUpdate>,
    /// OSC 0/2 terminal titles as worker metadata. 256.
    pub title_bus: BoundedBus<SessionTitleUpdate>,
    /// Last-activity observations. 256.
    pub last_activity_bus: BoundedBus<LastActivityUpdate>,
    /// UI state reports and commands. **Zero**: volatile and never replayed, so
    /// this bus must hold no payload at all. Pinned by
    /// `apps/coord/tests/ui-state/ui-state-owner.test.ts:117-124`.
    pub ui_bus: BoundedBus<UiBusMsg>,
}

impl Buses {
    /// The thirteen buses, each at its v2 bound.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session_bus: BoundedBus::new(256),
            workspace_bus: BoundedBus::new(64),
            task_bus: BoundedBus::new(64),
            mcp_bus: BoundedBus::new(128),
            agent_status_bus: BoundedBus::new(128),
            pair_bus: BoundedBus::new(32),
            audit_bus: BoundedBus::new(256),
            presence_bus: BoundedBus::new(128),
            worker_routable_bus: BoundedBus::new(64),
            global_presence_bus: BoundedBus::new(64),
            title_bus: BoundedBus::new(256),
            last_activity_bus: BoundedBus::new(256),
            ui_bus: BoundedBus::new(0),
        }
    }

    /// The thirteen buses behind one shared handle.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }
}

impl Default for Buses {
    fn default() -> Self {
        Self::new()
    }
}
