//! The boundary between a folded session row and its generated protobuf form.
//! The worker link, the coordinator projector and every browser store cross
//! this boundary for a `snapshot` event, so the brand re-validation and the
//! absent-versus-null rules are pinned here rather than at each call site.
//!
//! `git_remote` is the only field whose three states the wire keeps apart:
//! absent (never resolved), present and null (resolved, no GitHub origin), and
//! present and set. A `git` event that omits the remote must leave the prior
//! value alone, so the adapter never invents a presence the message did not
//! carry. Every other nullable collapses to absent, exactly as the optional
//! protobuf field does.

use roost_proto::Session as PbSession;

use crate::wire::brand::{ChannelId, SessionId, WorkerFp, WorkspaceId};
use crate::wire::session::{
    PullRequestChecks, PullRequestState, Session, SessionKind, SessionStatus,
};
use crate::{ProtocolError, ProtocolResult};

/// A `string` the wire carries for a value the domain types as a closed enum.
/// A newer peer may send a value this build has never heard of, so the
/// mismatch is an error naming the field and never a default.
pub(crate) fn session_kind_from_str(field: &str, value: &str) -> ProtocolResult<SessionKind> {
    [SessionKind::Shell]
        .into_iter()
        .find(|kind| kind.as_str() == value)
        .ok_or_else(|| unknown(field, "session kind", value))
}

pub(crate) fn session_status_from_str(field: &str, value: &str) -> ProtocolResult<SessionStatus> {
    [SessionStatus::Open, SessionStatus::Closed]
        .into_iter()
        .find(|status| status.as_str() == value)
        .ok_or_else(|| unknown(field, "session status", value))
}

pub(crate) fn pull_request_state_from_str(
    field: &str,
    value: &str,
) -> ProtocolResult<PullRequestState> {
    [
        PullRequestState::Open,
        PullRequestState::Merged,
        PullRequestState::Closed,
        PullRequestState::Draft,
    ]
    .into_iter()
    .find(|state| state.as_str() == value)
    .ok_or_else(|| unknown(field, "pull request state", value))
}

pub(crate) fn pull_request_checks_from_str(
    field: &str,
    value: &str,
) -> ProtocolResult<PullRequestChecks> {
    [
        PullRequestChecks::Passing,
        PullRequestChecks::Failing,
        PullRequestChecks::Pending,
        PullRequestChecks::None,
    ]
    .into_iter()
    .find(|checks| checks.as_str() == value)
    .ok_or_else(|| unknown(field, "pull request checks", value))
}

fn unknown(field: &str, label: &str, value: &str) -> ProtocolError {
    ProtocolError::new(field, format!("unknown {label} {value:?}"))
}

/// A timestamp is `uint64` on the wire and `i64` in the domain, in both
/// directions. A value that does not fit is a decode or encode error: letting
/// it wrap would date a session to a year the wire never carried.
fn wire_timestamp(field: &str, value: i64) -> ProtocolResult<u64> {
    u64::try_from(value)
        .map_err(|_| ProtocolError::new(field, format!("must not be negative, got {value}")))
}

fn domain_timestamp(field: &str, value: u64) -> ProtocolResult<i64> {
    i64::try_from(value).map_err(|_| {
        ProtocolError::new(field, format!("must not exceed {}, got {value}", i64::MAX))
    })
}

/// A pull-request number, a listening port and a channel are all `int32` on
/// the wire, so a row holding a value outside that range cannot be described
/// by the message at all and is an error rather than a wrap.
fn int32_field_to_wire(field: &str, value: i64) -> ProtocolResult<i32> {
    i32::try_from(value)
        .map_err(|_| ProtocolError::new(field, format!("must fit in 32 bits, got {value}")))
}

/// Encode a session row. The caller owns the brands, so nothing is re-checked
/// here; the row is a value this crate already produced through the fold.
pub fn session_to_proto(session: &Session) -> ProtocolResult<PbSession> {
    let ports = session.ports.clone().unwrap_or_default();
    Ok(PbSession {
        id: session.id.as_str().to_owned(),
        worker_fp: session.worker_fp.as_str().to_owned(),
        channel: session.channel.as_u32(),
        kind: session.kind.as_str().to_owned(),
        cwd: session.cwd.clone(),
        spawn_cwd: session.spawn_cwd.clone(),
        workspace_id: session
            .workspace_id
            .as_ref()
            .map(|workspace| workspace.as_str().to_owned()),
        status: session.status.as_str().to_owned(),
        created_at: wire_timestamp("created_at", session.created_at)?,
        closed_at: session
            .closed_at
            .map(|closed| wire_timestamp("closed_at", closed))
            .transpose()?,
        custom_title: session.custom_title.clone(),
        git_branch: session.git_branch.clone(),
        // A resolved-but-empty remote and a never-resolved one both mean "no
        // value to send", which is what the absent field already says.
        git_remote: session.git_remote.clone().flatten(),
        pr_number: session
            .pr_number
            .map(|number| int32_field_to_wire("pr_number", number))
            .transpose()?,
        pr_state: session.pr_state.map(|state| state.as_str().to_owned()),
        pr_checks: session.pr_checks.map(|checks| checks.as_str().to_owned()),
        pr_url: session.pr_url.clone(),
        ports: ports
            .into_iter()
            .map(|port| int32_field_to_wire("ports", port))
            .collect::<ProtocolResult<Vec<i32>>>()?,
        ..Default::default()
    })
}

/// Decode a session row. Brands and the row's own limits are checked here, so a
/// row that reached a projector through this boundary is the same row a
/// directly parsed one would be.
pub fn session_from_proto(proto: &PbSession) -> ProtocolResult<Session> {
    let session = Session {
        id: SessionId::try_from(proto.id.as_str())?,
        worker_fp: WorkerFp::try_from(proto.worker_fp.as_str())?,
        channel: ChannelId::try_from(i64::from(proto.channel))?,
        kind: session_kind_from_str("kind", &proto.kind)?,
        cwd: proto.cwd.clone(),
        spawn_cwd: proto.spawn_cwd.clone(),
        // An empty workspace id is protobuf's own "no value" for this field,
        // and an orphan row is the sidebar's Inbox bucket rather than an error.
        workspace_id: proto
            .workspace_id
            .as_deref()
            .filter(|workspace| !workspace.is_empty())
            .map(WorkspaceId::try_from)
            .transpose()?,
        status: session_status_from_str("status", &proto.status)?,
        created_at: domain_timestamp("created_at", proto.created_at)?,
        closed_at: proto
            .closed_at
            .map(|closed| domain_timestamp("closed_at", closed))
            .transpose()?,
        custom_title: proto.custom_title.clone(),
        git_branch: proto.git_branch.clone(),
        git_remote: proto.git_remote.clone().map(Some),
        pr_number: proto.pr_number.map(i64::from),
        pr_state: proto
            .pr_state
            .as_deref()
            .map(|state| pull_request_state_from_str("pr_state", state))
            .transpose()?,
        pr_checks: proto
            .pr_checks
            .as_deref()
            .map(|checks| pull_request_checks_from_str("pr_checks", checks))
            .transpose()?,
        pr_url: proto.pr_url.clone(),
        // `ports` is a `repeated int32` with no proto3 presence, so an empty
        // list and an absent field are the same bytes on the wire. The
        // distinction the row needs — "not looked yet" against "looked, and
        // nothing is listening" — is therefore the empty list itself, and an
        // absent field decodes as the unresolved state.
        ports: (!proto.ports.is_empty())
            .then(|| proto.ports.iter().map(|port| i64::from(*port)).collect()),
    };
    session.check()?;
    Ok(session)
}
