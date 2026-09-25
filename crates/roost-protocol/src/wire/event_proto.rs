//! The boundary between the session event union and its generated protobuf
//! oneof. The worker link, the coordinator's projector and every browser store
//! cross it, so the two things it must never confuse are told apart here: an
//! event kind this build does not know is not an event at all, and a known
//! kind arriving malformed is a decode error naming the offending field. Only
//! `agent_reference` has a `trace_id` field, and `event_id`/`ts` stay integers.

use roost_proto::__buffa::oneof::session_event_proto::Kind;
use roost_proto::{
    AgentReferenceEvt, AttachedEvt, ClosedEvt, CwdEvt, DetachedEvt, GitEvt, OpenedEvt, PortsEvt,
    PrEvt, RenamedEvt, RespawnedEvt, SessionEventProto, SnapshotEvt, WorkspaceAssignedEvt,
};

use crate::proto_adapters::agent_conversation_reference_proto::{
    agent_conversation_reference_from_proto, agent_conversation_reference_to_proto,
};
use crate::validate::integer_in_range;
use crate::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp, WorkspaceId};
use crate::wire::event::SessionEvent;
use crate::wire::session_proto::{
    pull_request_checks_from_str, pull_request_state_from_str, session_from_proto,
    session_kind_from_str, session_to_proto,
};
use crate::{ProtocolError, ProtocolResult};

/// A decoded event plus the transport sequence number it arrived under. The
/// number is link metadata rather than part of the durable value, so it travels
/// beside the event instead of inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedEvent {
    pub event_id: u64,
    pub event: SessionEvent,
}

fn session_id(field: &str, value: &str) -> ProtocolResult<SessionId> {
    SessionId::try_from(value).map_err(|error| error.within(field))
}

fn worker_fp(field: &str, value: &str) -> ProtocolResult<WorkerFp> {
    WorkerFp::try_from(value).map_err(|error| error.within(field))
}

fn channel(field: &str, value: u32) -> ProtocolResult<ChannelId> {
    ChannelId::try_from(i64::from(value)).map_err(|error| error.within(field))
}

fn trace_id(field: &str, value: &str) -> ProtocolResult<TraceId> {
    TraceId::try_from(value).map_err(|error| error.within(field))
}

/// A `uint64` timestamp that does not fit the union's `i64`, or that is not a
/// whole second at or after the epoch, is a decode error rather than a wrap.
fn event_ts(field: &str, value: u64) -> ProtocolResult<i64> {
    let timestamp = i64::try_from(value).map_err(|_| {
        ProtocolError::new(field, format!("must not exceed {}, got {value}", i64::MAX))
    })?;
    integer_in_range(field, timestamp, 1, i64::MAX).map(|()| timestamp)
}

/// The encode direction: a negative timestamp is not a value `uint64` can carry.
fn wire_ts(field: &str, value: i64) -> ProtocolResult<u64> {
    u64::try_from(value)
        .map_err(|_| ProtocolError::new(field, format!("must not be negative, got {value}")))
}

fn int32_field(field: &str, value: i64) -> ProtocolResult<i32> {
    i32::try_from(value)
        .map_err(|_| ProtocolError::new(field, format!("must fit in 32 bits, got {value}")))
}

/// Re-run the union's own contract on a value about to be emitted or just built,
/// so both directions answer the durable-envelope bound the same way.
fn recheck_event(event: &SessionEvent) -> ProtocolResult<()> {
    let durable = serde_json::to_value(event)
        .map_err(|error| ProtocolError::new("session_event", error.to_string()))?;
    SessionEvent::parse(durable)?;
    Ok(())
}

/// Encode an event as its oneof, exhaustively: a partial match drops events.
pub fn event_to_proto(event: &SessionEvent, event_id: u64) -> ProtocolResult<SessionEventProto> {
    if matches!(event, SessionEvent::AgentReference { .. }) {
        recheck_event(event)?;
    }
    let kind = match event {
        SessionEvent::Opened {
            session_id,
            worker_fp,
            channel,
            session_kind,
            cwd,
            ts,
            ..
        } => Kind::Opened(Box::new(OpenedEvt {
            session_id: session_id.as_str().to_owned(),
            worker_fp: worker_fp.as_str().to_owned(),
            channel: channel.as_u32(),
            session_kind: session_kind.as_str().to_owned(),
            cwd: cwd.clone(),
            ts: wire_ts("session_event.opened.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Closed {
            session_id,
            exit_code,
            ts,
            ..
        } => Kind::Closed(Box::new(ClosedEvt {
            session_id: session_id.as_str().to_owned(),
            // Null and absent are the same here: a close with no code and an unset code look alike.
            exit_code: exit_code
                .map(|code| int32_field("session_event.closed.exit_code", code))
                .transpose()?,
            ts: wire_ts("session_event.closed.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Attached { session_id, ts, .. } => Kind::Attached(Box::new(AttachedEvt {
            session_id: session_id.as_str().to_owned(),
            ts: wire_ts("session_event.attached.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Detached { session_id, ts, .. } => Kind::Detached(Box::new(DetachedEvt {
            session_id: session_id.as_str().to_owned(),
            ts: wire_ts("session_event.detached.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Cwd {
            session_id,
            cwd,
            ts,
            ..
        } => Kind::Cwd(Box::new(CwdEvt {
            session_id: session_id.as_str().to_owned(),
            cwd: cwd.clone(),
            ts: wire_ts("session_event.cwd.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::WorkspaceAssigned {
            session_id,
            workspace_id,
            ts,
            ..
        } => Kind::WorkspaceAssigned(Box::new(WorkspaceAssignedEvt {
            session_id: session_id.as_str().to_owned(),
            workspace_id: workspace_id.as_ref().map(|id| id.as_str().to_owned()),
            ts: wire_ts("session_event.workspace_assigned.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Snapshot {
            worker_fp,
            sessions,
            ts,
            ..
        } => {
            let mut encoded = Vec::with_capacity(sessions.len());
            for (index, session) in sessions.iter().enumerate() {
                let path = format!("session_event.sessions[{index}]");
                encoded.push(session_to_proto(session).map_err(|error| error.within(&path))?);
            }
            Kind::Snapshot(Box::new(SnapshotEvt {
                worker_fp: worker_fp.as_str().to_owned(),
                sessions: encoded,
                ts: wire_ts("session_event.snapshot.ts", *ts)?,
                ..Default::default()
            }))
        }
        SessionEvent::Respawned {
            session_id,
            new_channel,
            ts,
            ..
        } => Kind::Respawned(Box::new(RespawnedEvt {
            session_id: session_id.as_str().to_owned(),
            new_channel: new_channel.as_u32(),
            ts: wire_ts("session_event.respawned.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Renamed {
            session_id,
            custom_title,
            ts,
            ..
        } => Kind::Renamed(Box::new(RenamedEvt {
            session_id: session_id.as_str().to_owned(),
            // A cleared rename carries the empty string, which is what an absent field decodes to.
            custom_title: custom_title.clone(),
            ts: wire_ts("session_event.renamed.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Git {
            session_id,
            branch,
            remote,
            ts,
            ..
        } => Kind::Git(Box::new(GitEvt {
            session_id: session_id.as_str().to_owned(),
            branch: branch.clone(),
            // An absent remote means "leave the previously resolved one alone".
            remote: remote.clone(),
            ts: wire_ts("session_event.git.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Pr {
            session_id,
            number,
            state,
            checks,
            url,
            ts,
            ..
        } => Kind::Pr(Box::new(PrEvt {
            session_id: session_id.as_str().to_owned(),
            number: number
                .map(|value| int32_field("session_event.pr.number", value))
                .transpose()?,
            state: state.map(|value| value.as_str().to_owned()),
            checks: checks.map(|value| value.as_str().to_owned()),
            url: url.clone(),
            ts: wire_ts("session_event.pr.ts", *ts)?,
            ..Default::default()
        })),
        SessionEvent::Ports {
            session_id,
            ports,
            ts,
            ..
        } => {
            let encoded = ports
                .iter()
                .enumerate()
                .map(|(index, port)| int32_field(&format!("session_event.ports[{index}]"), *port))
                .collect::<ProtocolResult<Vec<i32>>>()?;
            // A `repeated int32` has no proto3 presence: an empty list and an absent field are one.
            Kind::Ports(Box::new(PortsEvt {
                session_id: session_id.as_str().to_owned(),
                ports: encoded,
                ts: wire_ts("session_event.ports.ts", *ts)?,
                ..Default::default()
            }))
        }
        SessionEvent::AgentReference {
            session_id,
            reference,
            ts,
            trace_id,
        } => Kind::AgentReference(Box::new(AgentReferenceEvt {
            session_id: session_id.as_str().to_owned(),
            // A message field, so this is presence and not an optional scalar: absent clears it.
            reference: reference
                .as_ref()
                .map(agent_conversation_reference_to_proto)
                .transpose()?
                .into(),
            ts: wire_ts("session_event.agent_reference.ts", *ts)?,
            trace_id: trace_id.as_ref().map(|value| value.as_str().to_owned()),
            ..Default::default()
        })),
    };
    Ok(SessionEventProto {
        event_id,
        kind: Some(kind),
        ..Default::default()
    })
}

/// Decode one event. `Ok(None)` is a frame carrying no kind this build knows, which
/// the caller drops while the link stays up; `Err` is a known kind arriving malformed.
pub fn proto_to_event(proto: &SessionEventProto) -> ProtocolResult<Option<DecodedEvent>> {
    let Some(kind) = &proto.kind else {
        return Ok(None);
    };
    let event = match kind {
        Kind::Opened(value) => SessionEvent::Opened {
            session_id: session_id("session_event.opened", &value.session_id)?,
            worker_fp: worker_fp("session_event.opened", &value.worker_fp)?,
            channel: channel("session_event.opened", value.channel)?,
            session_kind: session_kind_from_str(
                "session_event.opened.session_kind",
                &value.session_kind,
            )?,
            cwd: value.cwd.clone(),
            ts: event_ts("session_event.opened.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Closed(value) => SessionEvent::Closed {
            session_id: session_id("session_event.closed", &value.session_id)?,
            exit_code: value.exit_code.map(i64::from),
            ts: event_ts("session_event.closed.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Attached(value) => SessionEvent::Attached {
            session_id: session_id("session_event.attached", &value.session_id)?,
            ts: event_ts("session_event.attached.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Detached(value) => SessionEvent::Detached {
            session_id: session_id("session_event.detached", &value.session_id)?,
            ts: event_ts("session_event.detached.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Cwd(value) => SessionEvent::Cwd {
            session_id: session_id("session_event.cwd", &value.session_id)?,
            cwd: value.cwd.clone(),
            ts: event_ts("session_event.cwd.ts", value.ts)?,
            trace_id: None,
        },
        Kind::WorkspaceAssigned(value) => SessionEvent::WorkspaceAssigned {
            session_id: session_id("session_event.workspace_assigned", &value.session_id)?,
            workspace_id: value
                .workspace_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(|id| {
                    WorkspaceId::try_from(id)
                        .map_err(|error| error.within("session_event.workspace_assigned"))
                })
                .transpose()?,
            ts: event_ts("session_event.workspace_assigned.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Snapshot(value) => {
            let mut sessions = Vec::with_capacity(value.sessions.len());
            for (index, session) in value.sessions.iter().enumerate() {
                let path = format!("session_event.sessions[{index}]");
                sessions.push(session_from_proto(session).map_err(|error| error.within(&path))?);
            }
            SessionEvent::Snapshot {
                worker_fp: worker_fp("session_event.snapshot", &value.worker_fp)?,
                sessions,
                ts: event_ts("session_event.snapshot.ts", value.ts)?,
                trace_id: None,
            }
        }
        Kind::Respawned(value) => SessionEvent::Respawned {
            session_id: session_id("session_event.respawned", &value.session_id)?,
            new_channel: channel("session_event.respawned", value.new_channel)?,
            ts: event_ts("session_event.respawned.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Renamed(value) => SessionEvent::Renamed {
            session_id: session_id("session_event.renamed", &value.session_id)?,
            // A cleared rename carries the empty string, which the fold maps back to "no override".
            custom_title: value.custom_title.clone(),
            ts: event_ts("session_event.renamed.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Git(value) => SessionEvent::Git {
            session_id: session_id("session_event.git", &value.session_id)?,
            branch: value.branch.clone(),
            // The wire's two states are the union's two: absent leaves the prior remote alone.
            remote: value.remote.clone(),
            ts: event_ts("session_event.git.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Pr(value) => SessionEvent::Pr {
            session_id: session_id("session_event.pr", &value.session_id)?,
            number: value.number.map(i64::from),
            state: value
                .state
                .as_deref()
                .map(|state| pull_request_state_from_str("session_event.pr.state", state))
                .transpose()?,
            checks: value
                .checks
                .as_deref()
                .map(|checks| pull_request_checks_from_str("session_event.pr.checks", checks))
                .transpose()?,
            url: value.url.clone(),
            ts: event_ts("session_event.pr.ts", value.ts)?,
            trace_id: None,
        },
        Kind::Ports(value) => SessionEvent::Ports {
            session_id: session_id("session_event.ports", &value.session_id)?,
            // The repeated field has no presence, so an absent field decodes as the empty list.
            ports: value.ports.iter().map(|port| i64::from(*port)).collect(),
            ts: event_ts("session_event.ports.ts", value.ts)?,
            trace_id: None,
        },
        Kind::AgentReference(value) => SessionEvent::AgentReference {
            session_id: session_id("session_event.agent_reference", &value.session_id)?,
            reference: agent_conversation_reference_from_proto(value.reference.as_option())
                .map_err(|error| error.within("session_event.agent_reference"))?,
            ts: event_ts("session_event.agent_reference.ts", value.ts)?,
            trace_id: value
                .trace_id
                .as_deref()
                .map(|value| trace_id("session_event.agent_reference", value))
                .transpose()?,
        },
    };
    if matches!(event, SessionEvent::AgentReference { .. }) {
        recheck_event(&event)?;
    }
    Ok(Some(DecodedEvent {
        event_id: proto.event_id,
        event,
    }))
}
