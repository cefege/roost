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
    /// The session this event names, or `None` for a `snapshot`.
    ///
    /// A snapshot is the one variant with no session id: it announces a machine's
    /// whole live set, and every row it carries has its own. The coordinator's
    /// `events.session_id` column, and every admission rule that asks "which
    /// session is this about", read this one answer -- so a variant added later
    /// cannot forget to answer it.
    #[must_use]
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Opened { session_id, .. }
            | Self::Closed { session_id, .. }
            | Self::Attached { session_id, .. }
            | Self::Detached { session_id, .. }
            | Self::Cwd { session_id, .. }
            | Self::WorkspaceAssigned { session_id, .. }
            | Self::Respawned { session_id, .. }
            | Self::Renamed { session_id, .. }
            | Self::Git { session_id, .. }
            | Self::Pr { session_id, .. }
            | Self::Ports { session_id, .. }
            | Self::AgentReference { session_id, .. } => Some(session_id),
        }
    }

    /// The wire discriminator, exactly as serde renders it.
    ///
    /// This is the value the coordinator stores in `events.kind` and compares
    /// against, and it has to be the serde tag and nothing else: a durable column
    /// keyed on a spelling that drifts from the payload's own `kind` makes the
    /// log's private-kind filter and its force-close tombstone query answer about
    /// rows they cannot see. The test at the bottom of this file pins the two
    /// together for every variant.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Opened { .. } => "opened",
            Self::Closed { .. } => "closed",
            Self::Attached { .. } => "attached",
            Self::Detached { .. } => "detached",
            Self::Cwd { .. } => "cwd",
            Self::WorkspaceAssigned { .. } => "workspace_assigned",
            Self::Snapshot { .. } => "snapshot",
            Self::Respawned { .. } => "respawned",
            Self::Renamed { .. } => "renamed",
            Self::Git { .. } => "git",
            Self::Pr { .. } => "pr",
            Self::Ports { .. } => "ports",
            Self::AgentReference { .. } => "agent_reference",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::brand::ChannelId;
    use crate::wire::session::SessionKind;

    /// One event per variant, so the parity check covers the whole union.
    fn every_variant() -> Vec<SessionEvent> {
        let session_id = SessionId::try_from("00000000-0000-4000-8000-000000000001")
            .expect("the fixture id is a uuid");
        let worker_fp = WorkerFp::try_from("a".repeat(64)).expect("the fixture fp is hex");
        let channel = ChannelId::try_from(1_i64).expect("one is a channel");
        vec![
            SessionEvent::Opened {
                session_id: session_id.clone(),
                worker_fp: worker_fp.clone(),
                channel,
                session_kind: SessionKind::Shell,
                cwd: "/tmp".to_owned(),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Closed {
                session_id: session_id.clone(),
                exit_code: Some(0),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Attached {
                session_id: session_id.clone(),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Detached {
                session_id: session_id.clone(),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Cwd {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::WorkspaceAssigned {
                session_id: session_id.clone(),
                workspace_id: None,
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Snapshot {
                worker_fp,
                sessions: Vec::new(),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Respawned {
                session_id: session_id.clone(),
                new_channel: channel,
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Renamed {
                session_id: session_id.clone(),
                custom_title: "t".to_owned(),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Git {
                session_id: session_id.clone(),
                branch: None,
                remote: None,
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Pr {
                session_id: session_id.clone(),
                number: None,
                state: None,
                checks: None,
                url: None,
                ts: 1,
                trace_id: None,
            },
            SessionEvent::Ports {
                session_id: session_id.clone(),
                ports: Vec::new(),
                ts: 1,
                trace_id: None,
            },
            SessionEvent::AgentReference {
                session_id,
                reference: None,
                ts: 1,
                trace_id: None,
            },
        ]
    }

    #[test]
    fn the_discriminator_is_the_serde_tag_for_every_variant() {
        for event in every_variant() {
            let tag = serde_json::to_value(&event)
                .expect("an event encodes")
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("the tag is a string")
                .to_owned();
            assert_eq!(
                event.kind_name(),
                tag,
                "{} disagrees with its own payload",
                event.kind_name()
            );
        }
    }

    #[test]
    fn only_a_snapshot_carries_no_session_id() {
        for event in every_variant() {
            let carries_one = event.session_id().is_some();
            assert_eq!(
                carries_one,
                !matches!(event, SessionEvent::Snapshot { .. }),
                "{} disagreed about naming a session",
                event.kind_name()
            );
        }
    }
}
