//! What the coordinator's buses carry: the payloads that are its own, and the
//! protocol types the rest are.
//!
//! Owned by the coordinator and ported from the type half of
//! `apps/coord/src/events/buses.ts:6-78`. Which bus carries which payload is
//! `bus_domains`'s table; this file is the vocabulary those buses speak.
//!
//! THREE OF THESE ARE COORD-INTERNAL BY DESIGN. `PairRequestDelta` has no shared
//! wire schema because it never crosses a boundary: it is published by the pair
//! handlers and the retention sweep and consumed only by the firehose adapter
//! (`buses.ts:30-35`). `AuditRow` is an `audit_log` row as inserted, which is why
//! `caller_label` is a field that is *always* null here -- the insert path has
//! no key label to write (`apps/coord/src/middleware/security.ts:215`) and the
//! RPC read path joins `authorized_keys` to supply it. `TaskBusMsg` and
//! `UiBusMsg` carry generated protobuf messages, so the firehose can build a
//! frame without the JSON round-trip the old Zod-shape relay cost
//! (`buses.ts:22-28`).
//!
//! WHY THE PROTO TYPES GET HAND-WRITTEN `Debug`. Generated buffa messages are
//! not `Debug` -- the generated code suppresses that lint on purpose, because
//! materialising a debug rendering of a view type defeats its purpose. These
//! wrappers print the fields an operator actually reads off a fan-out line
//! (which task, which tab, which session) instead of a derived dump they cannot
//! produce.

use roost_proto::{Task, UiCommand, UiReportStateRequest};
use roost_protocol::wire::{SessionEvent, WorkerFp};
use serde_json::Value;

/// Durable session fan-out, carrying the durable row's id as the replay order.
///
/// The stamp is **internal**: it is never a wire field. It exists so the Sync
/// feed can order what it receives against what `get_event_max_id` returns from
/// the same log, which is what makes the recovery cutoff stable
/// (`buses.ts:15-19`, `docs/phase3-coord-contract.md` §3.8).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionBusMessage {
    /// The committed public event.
    pub event: SessionEvent,
    /// The `events.id` it was committed under, absent only for an event
    /// published without one -- which the durable path never does.
    pub event_id: Option<u64>,
}

impl SessionBusMessage {
    /// A message for an event whose durable id is known.
    #[must_use]
    pub fn committed(event: SessionEvent, event_id: u64) -> Self {
        Self {
            event,
            event_id: Some(event_id),
        }
    }
}

/// Which of the two task mutations a [`TaskBusMsg`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskBusMsgKind {
    /// A task row appeared.
    Created,
    /// A task row changed state.
    State,
}

/// A task row as the firehose should carry it: the generated message, not a row.
#[derive(Clone, PartialEq)]
pub struct TaskBusMsg {
    /// Which mutation produced this.
    pub kind: TaskBusMsgKind,
    /// The task row, already in its wire shape.
    pub task: Task,
}

impl std::fmt::Debug for TaskBusMsg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TaskBusMsg")
            .field("kind", &self.kind)
            .field("task_id", &self.task.id)
            .finish()
    }
}

/// A pair-request change for install-wide Sync viewers.
///
/// `pending` upserts, `removed` drops by `ephemeral_id`, and `completed` is a
/// volatile "a new browser paired" notice carrying only non-secret descriptors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairRequestDelta {
    /// A pending request was created or changed.
    Pending {
        /// The ceremony's opaque handle; the row's identity for both other arms.
        ephemeral_id: String,
        /// The operator-facing device label.
        label: String,
        /// When the request was created, epoch milliseconds.
        created_at_ms: i64,
        /// The requester's user agent, as reported.
        user_agent: String,
        /// Browser name parsed from it.
        client_browser: String,
        /// Operating system parsed from it.
        client_os: String,
        /// Device class parsed from it.
        client_device_type: String,
        /// The address the request arrived from.
        source_ip: String,
        /// Edge geo country, when the edge supplied one.
        country_code: String,
        /// Edge geo region, when the edge supplied one.
        region: String,
        /// Edge geo city, when the edge supplied one.
        city: String,
        /// The identity provider the edge vouched for.
        edge_identity_provider: String,
        /// The identity the edge vouched for.
        edge_identity: String,
        /// Whether the edge attested that identity.
        edge_identity_verified: bool,
        /// When the request expires, epoch milliseconds.
        expires_at_ms: i64,
    },
    /// A request left the pending set, by approval, denial, or expiry.
    Removed {
        /// The ceremony's opaque handle.
        ephemeral_id: String,
    },
    /// A device finished pairing. Carries descriptors only: no key material and
    /// no token ever crosses this bus.
    Completed {
        /// The ceremony's opaque handle.
        ephemeral_id: String,
        /// The paired device's label.
        label: String,
        /// Browser name parsed from its user agent.
        client_browser: String,
        /// Operating system parsed from its user agent.
        client_os: String,
        /// Device class parsed from its user agent.
        client_device_type: String,
        /// Edge geo country.
        country_code: String,
        /// Edge geo region.
        region: String,
        /// Edge geo city.
        city: String,
        /// When pairing completed, epoch milliseconds.
        paired_at_ms: i64,
    },
}

/// An `audit_log` row as the insert path publishes it.
///
/// `caller_label` is null on every value this bus ever sees: the interceptor
/// writes the row before it knows the key's label, and the label is joined at
/// read time (`apps/coord/src/middleware/security.ts:215`). The field exists
/// because the Sync frame carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    /// The row's id.
    pub id: i64,
    /// When the request completed, epoch milliseconds.
    pub ts: i64,
    /// The authenticated key's fingerprint, absent for an anonymous request.
    pub caller_fp: Option<String>,
    /// The caller's label, always absent on this path. See the type header.
    pub caller_label: Option<String>,
    /// The RPC method name.
    pub method: String,
    /// The request path.
    pub path: String,
    /// The response status.
    pub status: i64,
    /// The propagated trace id, when the request carried one.
    pub trace_id: Option<String>,
}

/// An OSC 0/2 terminal title, supplied as semantic worker metadata.
///
/// Published only on meaningful change and seeded to fresh Sync subscribers;
/// browser clients never parse PTY bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTitleUpdate {
    /// The session whose title changed.
    pub session_id: String,
    /// The new title.
    pub title: String,
}

/// A last-activity timestamp from a semantic worker observation.
///
/// The coordinator throttles the live fan-out; the retained value is what lets
/// a fresh subscriber age an idle OPEN session immediately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastActivityUpdate {
    /// The session whose activity changed.
    pub session_id: String,
    /// The observation, epoch milliseconds.
    pub ts_ms: i64,
}

/// Which workers are reachable right now: the full current set, every time.
///
/// This is the **authoritative** "the server is reachable" signal, distinct from
/// `last_seen_ms` heartbeat freshness, and it is published in full on every
/// connect and disconnect because the set is a handful of machines. It is what
/// keeps a browser's online indicator live instead of waiting for the periodic
/// worker list ("the active server shows red").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRoutableSet {
    /// Every worker with a live coordinator socket.
    pub fps: Vec<WorkerFp>,
}

/// Session-scoped presence: viewers, input claims, and the other ephemeral facts
/// a viewer needs about a session it is watching.
///
/// The payload is opaque by construction. v2 typed it `unknown` and stringified
/// it straight into `SessionPresence.payload_json`
/// (`apps/coord/src/sync/sync-feed.ts:245`), so the Sync feed can filter a
/// presence update addressed to a *different* viewer without this bus knowing
/// the shape.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionPresenceUpdate {
    /// The session the presence is about.
    pub session_id: String,
    /// The opaque payload, published verbatim.
    pub data: Value,
}

/// Volatile coding-agent and UI control traffic.
///
/// `state` upserts a tab's reported state, `command` is a legacy control that
/// retains publication-count delivery, and `apply` is the acknowledged form
/// whose delivery is fenced by the socket generation. The bus retains **nothing**
/// of any of them.
#[derive(Clone, PartialEq)]
pub enum UiBusMsg {
    /// A tab reported its state.
    State {
        /// The fingerprint whose tab reported.
        fp: String,
        /// The tab's id.
        tab_id: String,
        /// The reported state.
        state: UiReportStateRequest,
    },
    /// A legacy command for a tab.
    Command {
        /// The tab meant.
        target_tab_id: String,
        /// The command.
        command: UiCommand,
    },
    /// An acknowledged command, addressed to one socket.
    Apply {
        /// The tab meant.
        target_tab_id: String,
        /// The socket that acknowledged it.
        target_socket_id: String,
        /// The RPC correlation id, so the acknowledgement can be matched.
        correlation_id: String,
        /// The command.
        command: UiCommand,
    },
}

impl std::fmt::Debug for UiBusMsg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::State { fp, tab_id, .. } => formatter
                .debug_struct("UiBusMsg::State")
                .field("fp", fp)
                .field("tab_id", tab_id)
                .finish(),
            Self::Command { target_tab_id, .. } => formatter
                .debug_struct("UiBusMsg::Command")
                .field("target_tab_id", target_tab_id)
                .finish(),
            Self::Apply {
                target_tab_id,
                target_socket_id,
                correlation_id,
                ..
            } => formatter
                .debug_struct("UiBusMsg::Apply")
                .field("target_tab_id", target_tab_id)
                .field("target_socket_id", target_socket_id)
                .field("correlation_id", correlation_id)
                .finish(),
        }
    }
}
