//! The append-only session event union and the rules one decoded event must meet.
//!
//! The union is the wire's half of the session log: every variant is a fact
//! about one session at one instant, and each field's absence is itself a
//! state the fold reads. The fold those variants feed is in the parent module —
//! there is exactly one fold in Roost.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_conversation_reference::{
    AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES, AgentConversationReferenceV1,
    is_agent_conversation_reference_event_envelope_bounded,
};
use crate::validate::integer_in_range;
use crate::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp, WorkspaceId};
use crate::wire::session::{PullRequestChecks, PullRequestState, Session, SessionKind};
use crate::{ProtocolError, ProtocolResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    Opened {
        session_id: SessionId,
        worker_fp: WorkerFp,
        channel: ChannelId,
        session_kind: SessionKind,
        cwd: String,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    Closed {
        session_id: SessionId,
        exit_code: Option<i64>,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    Attached {
        session_id: SessionId,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    Detached {
        session_id: SessionId,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    Cwd {
        session_id: SessionId,
        cwd: String,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    WorkspaceAssigned {
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// A worker re-announces all of its live sessions on reconnect. Every
    /// session of this worker absent from the snapshot keeps its row.
    Snapshot {
        worker_fp: WorkerFp,
        sessions: Vec<Session>,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The worker rebooted and the keeper PTY behind this session died. It
    /// spawned a fresh PTY at the same cwd and re-bound it to the same session
    /// id, so the sidebar row stays in place.
    Respawned {
        session_id: SessionId,
        new_channel: ChannelId,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// A user rename from the sidebar. An empty title clears the override and
    /// reverts to the auto title, and it is sticky: auto-title events never
    /// write `custom_title`, so a rename survives title churn.
    Renamed {
        session_id: SessionId,
        custom_title: String,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The worker resolved or re-resolved the git branch of the session's cwd.
    /// A null branch means the folder is not a git repo. `remote` is present
    /// only when resolved, and its absence means "leave the prior value alone".
    Git {
        session_id: SessionId,
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote: Option<String>,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// GitHub pull-request status for the branch, resolved through
    /// `gh pr list --head <branch>`. A null number means no open PR.
    Pr {
        session_id: SessionId,
        number: Option<i64>,
        state: Option<PullRequestState>,
        checks: Option<PullRequestChecks>,
        url: Option<String>,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The TCP ports the session's process tree is listening on. Empty means
    /// nothing is listening.
    Ports {
        session_id: SessionId,
        ports: Vec<i64>,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// Private recovery metadata. It is durable and ordered by the worker
    /// envelope's `client_seq`, and it is never projected into public session
    /// state — see `agent_conversation_reference`.
    AgentReference {
        session_id: SessionId,
        reference: Option<AgentConversationReferenceV1>,
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
}

impl SessionEvent {
    /// Every variant carries the same timestamp, so a caller reads it without
    /// first matching on the kind.
    pub fn ts(&self) -> i64 {
        match self {
            Self::Opened { ts, .. }
            | Self::Closed { ts, .. }
            | Self::Attached { ts, .. }
            | Self::Detached { ts, .. }
            | Self::Cwd { ts, .. }
            | Self::WorkspaceAssigned { ts, .. }
            | Self::Snapshot { ts, .. }
            | Self::Respawned { ts, .. }
            | Self::Renamed { ts, .. }
            | Self::Git { ts, .. }
            | Self::Pr { ts, .. }
            | Self::Ports { ts, .. }
            | Self::AgentReference { ts, .. } => *ts,
        }
    }

    /// Decode and check one event. `value` is the already-decoded JSON.
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let event: SessionEvent = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("session_event", error.to_string()))?;
        integer_in_range("session_event.ts", event.ts(), 1, i64::MAX)?;
        if let Self::Snapshot { sessions, .. } = &event {
            for (index, session) in sessions.iter().enumerate() {
                session
                    .check()
                    .map_err(|error| error.within(&format!("session_event.sessions[{index}]")))?;
            }
        }
        if let Self::AgentReference { reference, .. } = &event
            && let Some(reference) = reference
        {
            // The reference's own rules — the `omp` literal, the per-kind byte
            // bound, the control-character class, a path that must be absolute
            // — are checked here as well as in the reference's own `check`. A
            // serde decode builds the struct without running either, and the
            // event boundary is the one place a value from a peer enters.
            reference
                .check()
                .map_err(|error| error.within("session_event.reference"))?;
        }
        if let Self::AgentReference { .. } = &event {
            // The bound is on the serialized envelope, which is what the log
            // stores, so it is measured on the parsed event rather than on the
            // caller's input: an unknown key must not push a stored record past
            // its limit, and must not be stored either.
            let envelope = serde_json::to_value(&event)
                .map_err(|error| ProtocolError::new("session_event", error.to_string()))?;
            if !is_agent_conversation_reference_event_envelope_bounded(&envelope) {
                return Err(ProtocolError::new(
                    "session_event.reference",
                    format!(
                        "agent conversation reference event must not exceed \
                         {AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES} UTF-8 bytes"
                    ),
                ));
            }
        }
        Ok(event)
    }
}
