//! Workspaces: the grouping bucket sessions hang off, and the delta that
//! updates one.
//!
//! A workspace IS a folder on the worker host, which is why panes spawned into
//! it inherit `folder_path` as their cwd — a row with an empty folder path
//! would advertise a grouping that cannot be opened, so the field is required
//! non-empty rather than merely present.
//!
//! `version` is the CAS counter a write is conditioned on, so it is part of the
//! row and not derived state.

use serde::{Deserialize, Serialize};

use crate::validate::{integer_in_range, non_empty, nonnegative};
use crate::wire::brand::{SessionId, WorkerFp, WorkspaceId};
use crate::{ProtocolError, ProtocolResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    /// Pinned to one worker: a workspace's folder only exists on that machine.
    pub worker_fp: WorkerFp,
    pub name: String,
    pub folder_path: String,
    pub color: Option<String>,
    pub position: i64,
    /// The `If-Match` counter a write is conditioned on.
    pub version: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub session_ids: Vec<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceDelta {
    Created {
        workspace: Workspace,
    },
    Updated {
        workspace: Workspace,
    },
    Deleted {
        id: WorkspaceId,
    },
    /// Membership moves without rewriting the row, so the version the write
    /// produced is carried on the delta rather than read back from the row.
    #[serde(rename = "sessions-set")]
    SessionsSet {
        id: WorkspaceId,
        session_ids: Vec<SessionId>,
        version: i64,
    },
}

impl Workspace {
    pub fn parse(value: serde_json::Value) -> ProtocolResult<Self> {
        let workspace: Workspace = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("workspace", error.to_string()))?;
        workspace.check()?;
        Ok(workspace)
    }

    /// The row's own limits, shared with the delta so a `created` or an
    /// `updated` carries the same guarantees whether it arrives alone or
    /// wrapped in one.
    pub fn check(&self) -> ProtocolResult<()> {
        non_empty("workspace.name", &self.name)?;
        non_empty("workspace.folder_path", &self.folder_path)?;
        nonnegative("workspace.position", self.position)?;
        nonnegative("workspace.version", self.version)?;
        integer_in_range("workspace.created_at_ms", self.created_at_ms, 1, i64::MAX)?;
        integer_in_range("workspace.updated_at_ms", self.updated_at_ms, 1, i64::MAX)?;
        Ok(())
    }
}

impl WorkspaceDelta {
    pub fn parse(value: serde_json::Value) -> ProtocolResult<Self> {
        let delta: WorkspaceDelta = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("workspace_delta", error.to_string()))?;
        match &delta {
            WorkspaceDelta::Created { workspace } | WorkspaceDelta::Updated { workspace } => {
                workspace.check()?;
            }
            WorkspaceDelta::SessionsSet { version, .. } => {
                nonnegative("workspace_delta.version", *version)?;
            }
            WorkspaceDelta::Deleted { .. } => {}
        }
        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_json() -> serde_json::Value {
        serde_json::json!({
            "id": "00000000-0000-4000-8000-0000000000a1",
            "worker_fp": "b".repeat(64),
            "name": "Server",
            "folder_path": "/srv/app",
            "color": null,
            "position": 0,
            "version": 3,
            "created_at_ms": 10,
            "updated_at_ms": 20,
            "session_ids": ["00000000-0000-4000-8000-000000000001"],
        })
    }

    #[test]
    fn a_workspace_row_parses() {
        let workspace = Workspace::parse(workspace_json()).unwrap();
        assert_eq!(workspace.session_ids.len(), 1);
        assert_eq!(workspace.color, None);
    }

    #[test]
    fn a_workspace_with_no_name_or_no_folder_is_rejected() {
        for field in ["name", "folder_path"] {
            let mut value = workspace_json();
            value[field] = serde_json::json!("");
            assert_eq!(
                Workspace::parse(value).unwrap_err().field,
                format!("workspace.{field}")
            );
        }
    }

    #[test]
    fn position_and_version_cannot_be_negative() {
        for field in ["position", "version"] {
            let mut value = workspace_json();
            value[field] = serde_json::json!(-1);
            assert_eq!(
                Workspace::parse(value).unwrap_err().field,
                format!("workspace.{field}")
            );
        }
    }

    #[test]
    fn the_delta_discriminates_on_kind_and_keeps_its_tag_spelling() {
        let created = WorkspaceDelta::parse(serde_json::json!({
            "kind": "created",
            "workspace": workspace_json(),
        }))
        .unwrap();
        assert!(matches!(created, WorkspaceDelta::Created { .. }));

        let membership = WorkspaceDelta::parse(serde_json::json!({
            "kind": "sessions-set",
            "id": "00000000-0000-4000-8000-0000000000a1",
            "session_ids": [],
            "version": 4,
        }))
        .unwrap();
        assert!(matches!(
            membership,
            WorkspaceDelta::SessionsSet { version: 4, .. }
        ));
        assert_eq!(
            serde_json::to_value(&membership).unwrap()["kind"],
            serde_json::json!("sessions-set")
        );
    }

    #[test]
    fn a_delta_cannot_smuggle_an_unchecked_row_past_the_contract() {
        let broken = WorkspaceDelta::parse(serde_json::json!({
            "kind": "created",
            "workspace": {
                "id": "00000000-0000-4000-8000-0000000000a1",
                "worker_fp": "b".repeat(64),
                "name": "",
                "folder_path": "/srv/app",
                "color": null,
                "position": 0,
                "version": 1,
                "created_at_ms": 10,
                "updated_at_ms": 20,
                "session_ids": [],
            },
        }));
        assert_eq!(broken.unwrap_err().field, "workspace.name");
    }
}
