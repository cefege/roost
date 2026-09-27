//! The session projection target: what the fold produces and what every client
//! renders. One row, one shape, for the coordinator's projector, the browser's
//! store, and the worker's own reconciliation alike.
//!
//! `spawn_cwd` is the one field that is a creation fact rather than live state:
//! it is captured from the `opened` cwd and never updated, because it is the
//! stable identity behind the `/t/:workerFp/*folderPath` URL while `cwd` drifts
//! as the shell `cd`s.
//!
//! Depends only on `brand` and `validate`. The event union and the fold that
//! fills this row live in `event`; a caller must go through that fold rather
//! than building rows itself.

use serde::{Deserialize, Deserializer, Serialize};

use crate::validate::integer_in_range;
use crate::wire::brand::{ChannelId, SessionId, WorkerFp, WorkspaceId};
use crate::{ProtocolError, ProtocolResult};

/// Every Roost session is a keeper-backed shell terminal. The set is closed on
/// purpose: a structured or unsupported kind is a protocol error, not something
/// a newer peer may add, because the coordinator allocates a keeper PTY for
/// every session it is told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Shell,
}

impl std::str::FromStr for SessionKind {
    type Err = ProtocolError;

    fn from_str(value: &str) -> ProtocolResult<Self> {
        [SessionKind::Shell]
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or_else(|| {
                ProtocolError::new("session_kind", format!("unknown session kind {value:?}"))
            })
    }
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Open,
    Closed,
}

impl std::str::FromStr for SessionStatus {
    type Err = ProtocolError;

    fn from_str(value: &str) -> ProtocolResult<Self> {
        [SessionStatus::Open, SessionStatus::Closed]
            .into_iter()
            .find(|status| status.as_str() == value)
            .ok_or_else(|| {
                ProtocolError::new(
                    "session_status",
                    format!("unknown session status {value:?}"),
                )
            })
    }
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// State of the GitHub pull request for the session's branch, resolved on the
/// worker through `gh pr list`. `draft` is a state of its own in that API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Merged,
    Closed,
    Draft,
}

impl std::str::FromStr for PullRequestState {
    type Err = ProtocolError;

    fn from_str(value: &str) -> ProtocolResult<Self> {
        [
            PullRequestState::Open,
            PullRequestState::Merged,
            PullRequestState::Closed,
            PullRequestState::Draft,
        ]
        .into_iter()
        .find(|state| state.as_str() == value)
        .ok_or_else(|| {
            ProtocolError::new("pr_state", format!("unknown pull request state {value:?}"))
        })
    }
}

impl PullRequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Draft => "draft",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestChecks {
    Passing,
    Failing,
    Pending,
    None,
}

impl std::str::FromStr for PullRequestChecks {
    type Err = ProtocolError;

    fn from_str(value: &str) -> ProtocolResult<Self> {
        [
            PullRequestChecks::Passing,
            PullRequestChecks::Failing,
            PullRequestChecks::Pending,
            PullRequestChecks::None,
        ]
        .into_iter()
        .find(|checks| checks.as_str() == value)
        .ok_or_else(|| {
            ProtocolError::new(
                "pr_checks",
                format!("unknown pull request checks {value:?}"),
            )
        })
    }
}

impl PullRequestChecks {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passing => "passing",
            Self::Failing => "failing",
            Self::Pending => "pending",
            Self::None => "none",
        }
    }
}

/// A session row. `git_*`, `pr_*` and `ports` are absent rather than null until
/// the worker has resolved them, so a client can tell "not looked yet" from
/// "looked, and there is nothing there".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub worker_fp: WorkerFp,
    pub channel: ChannelId,
    pub kind: SessionKind,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_cwd: Option<String>,
    /// Null = orphan, which is the sidebar's Inbox bucket.
    pub workspace_id: Option<WorkspaceId>,
    pub status: SessionStatus,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    /// User rename, a sticky override of the auto title. Null = no override.
    /// The worker does not track it, so the snapshot fold preserves the prior
    /// value rather than letting a reconnect clear it.
    pub custom_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Three states the wire keeps apart: absent (never resolved), present and
    /// null (resolved, no GitHub origin), and present and set (`owner/repo`). A
    /// `git` event that omits `remote` must leave the prior value alone, so the
    /// outer option carries presence and the inner one carries the value.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_or_null"
    )]
    pub git_remote: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_state: Option<PullRequestState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_checks: Option<PullRequestChecks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<i64>>,
}

/// Tell an absent field (`None`, supplied by `default`) from a present null
/// (`Some(None)`). Serde maps a JSON `null` onto the outer `Option` by itself,
/// which is exactly the distinction this field exists to keep.
fn deserialize_present_or_null<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

impl Session {
    /// Decode and check one session row. `value` is the already-decoded JSON.
    pub fn parse(value: serde_json::Value) -> ProtocolResult<Self> {
        let session: Session = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("session", error.to_string()))?;
        session.check()?;
        Ok(session)
    }

    /// The row's own limits, shared with the snapshot event so a session that
    /// arrives inside one is held to the same contract as one decoded alone.
    pub fn check(&self) -> ProtocolResult<()> {
        integer_in_range("created_at", self.created_at, 1, i64::MAX)?;
        if let Some(closed_at) = self.closed_at {
            integer_in_range("closed_at", closed_at, 1, i64::MAX)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened_json() -> serde_json::Value {
        serde_json::json!({
            "id": "00000000-0000-4000-8000-000000000001",
            "worker_fp": "a".repeat(64),
            "channel": 1,
            "kind": "shell",
            "cwd": "/tmp",
            "workspace_id": null,
            "status": "open",
            "created_at": 1,
            "closed_at": null,
            "custom_title": null,
        })
    }

    #[test]
    fn a_terminal_row_parses_and_keeps_its_absent_optionals_absent() {
        let session = Session::parse(opened_json()).unwrap();
        assert_eq!(session.spawn_cwd, None);
        assert_eq!(session.git_branch, None);
        assert_eq!(session.git_remote, None);
        assert_eq!(session.ports, None);
    }

    #[test]
    fn only_a_shell_session_kind_is_accepted() {
        for rejected in ["agent", "claude", "unsupported"] {
            let mut value = opened_json();
            value["kind"] = serde_json::json!(rejected);
            assert!(Session::parse(value).is_err(), "{rejected} was accepted");
        }
    }

    #[test]
    fn a_timestamp_must_be_a_positive_integer() {
        let mut zero = opened_json();
        zero["created_at"] = serde_json::json!(0);
        assert_eq!(Session::parse(zero).unwrap_err().field, "created_at");
        let mut fractional = opened_json();
        fractional["created_at"] = serde_json::json!(1.5);
        assert!(Session::parse(fractional).is_err());
    }

    #[test]
    fn an_absent_git_remote_and_a_null_one_are_different_states() {
        let mut value = opened_json();
        assert_eq!(Session::parse(value.clone()).unwrap().git_remote, None);

        value["git_remote"] = serde_json::json!(null);
        assert_eq!(
            Session::parse(value.clone()).unwrap().git_remote,
            Some(None),
            "an explicit null must not read as absent"
        );

        value["git_remote"] = serde_json::json!("owner/repo");
        assert_eq!(
            Session::parse(value).unwrap().git_remote,
            Some(Some("owner/repo".to_owned()))
        );
    }

    #[test]
    fn a_channel_the_proto_could_not_carry_is_rejected() {
        let mut value = opened_json();
        value["channel"] = serde_json::json!(-1);
        assert!(Session::parse(value).is_err());
    }

    #[test]
    fn announced_ports_are_whole_numbers() {
        let mut value = opened_json();
        value["ports"] = serde_json::json!([5174, 8080]);
        assert_eq!(Session::parse(value).unwrap().ports, Some(vec![5174, 8080]));
        let mut fractional = opened_json();
        fractional["ports"] = serde_json::json!([5174.5]);
        assert!(Session::parse(fractional).is_err());
    }
}
