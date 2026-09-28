//! The sessions and workspaces this tab's smoke calls created, carried across a
//! reload in `sessionStorage`, and the scoped cleanup that removes exactly those
//! and never a live resource. Native; `smoke::dispatch` owns one ledger and
//! drives `cleanup_created` over `CoordRpc`. Ports
//! `apps/web/src/smoke/smokeCreatedResources.ts`.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

/// The `sessionStorage` key the ledger survives a reload under.
pub const CREATED_RESOURCES_KEY: &str = "roostSmoke.created.v1";

/// Insertion-ordered ids, each once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreatedResources {
    sessions: Vec<String>,
    workspaces: Vec<String>,
}

impl CreatedResources {
    /// The ledger a previous document left, or empty when there is none or it
    /// does not parse — privacy mode must not prevent smoke setup.
    pub fn restore(raw: Option<&str>) -> Self {
        let mut restored = Self::default();
        let Some(Value::Object(carried)) = raw.and_then(|text| serde_json::from_str(text).ok())
        else {
            return restored;
        };
        let (Some(sessions), Some(workspaces)) =
            (carried.get("sessions"), carried.get("workspaces"))
        else {
            return restored;
        };
        for id in ids_of(sessions) {
            restored.track_session(&id);
        }
        for id in ids_of(workspaces) {
            restored.track_workspace(&id);
        }
        restored
    }

    /// The persisted form.
    pub fn encode(&self) -> String {
        serde_json::json!({ "sessions": self.sessions, "workspaces": self.workspaces }).to_string()
    }

    pub fn track_session(&mut self, session_id: &str) {
        if !self.sessions.iter().any(|known| known == session_id) {
            self.sessions.push(session_id.to_owned());
        }
    }

    pub fn track_workspace(&mut self, workspace_id: &str) {
        if !self.workspaces.iter().any(|known| known == workspace_id) {
            self.workspaces.push(workspace_id.to_owned());
        }
    }

    /// Everything recorded, emptying the ledger.
    pub fn take_all(&mut self) -> (Vec<String>, Vec<String>) {
        (
            std::mem::take(&mut self.sessions),
            std::mem::take(&mut self.workspaces),
        )
    }
}

/// `String(value)` for an id carried in the ledger.
fn ids_of(value: &Value) -> Vec<String> {
    let Value::Array(items) = value else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| match item {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect()
}

/// The first free name for a workspace: the folder's basename (`~` for none),
/// then `"<base> 2"`, `"<base> 3"`, … among this worker's workspace names.
pub fn free_workspace_name(existing: &[String], basename: Option<&str>) -> String {
    let base = basename.filter(|name| !name.is_empty()).unwrap_or("~");
    let mut name = base.to_owned();
    let mut suffix = 2;
    while existing.contains(&name) {
        name = format!("{base} {suffix}");
        suffix += 1;
    }
    name
}

/// What `cleanupCreated()` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub killed_sessions: Vec<String>,
    pub deleted_workspaces: Vec<String>,
    pub errors: Vec<String>,
}

/// The coordinator calls cleanup makes.
pub trait CleanupRpc {
    fn kill_session(&self, session_id: &str) -> impl Future<Output = Result<(), String>>;
    /// Workspace id → version, as the coordinator lists them now.
    fn workspace_versions(&self) -> impl Future<Output = Result<BTreeMap<String, u64>, String>>;
    fn delete_workspace(
        &self,
        workspace_id: &str,
        version: u64,
    ) -> impl Future<Output = Result<(), String>>;
}

/// Kill every recorded session, then delete every recorded workspace that
/// still exists, re-reading its version on each of two attempts so a version
/// bumped by the kill's cascade does not fail the delete.
pub async fn cleanup_created<R: CleanupRpc>(
    rpc: &R,
    sessions: Vec<String>,
    workspaces: Vec<String>,
) -> CleanupReport {
    let mut report = CleanupReport::default();
    for session_id in sessions {
        match rpc.kill_session(&session_id).await {
            Ok(()) => report.killed_sessions.push(session_id),
            Err(error) => report.errors.push(format!("kill {session_id}: {error}")),
        }
    }
    for workspace_id in workspaces {
        for attempt in 0..2 {
            let deleted = match rpc.workspace_versions().await {
                Ok(versions) => match versions.get(&workspace_id) {
                    None => break,
                    Some(version) => rpc.delete_workspace(&workspace_id, *version).await,
                },
                Err(error) => Err(error),
            };
            match deleted {
                Ok(()) => {
                    report.deleted_workspaces.push(workspace_id.clone());
                    break;
                }
                Err(error) if attempt == 1 => {
                    report
                        .errors
                        .push(format!("delete workspace {workspace_id}: {error}"));
                }
                Err(_) => {}
            }
        }
    }
    tracing::info!(
        target: "smoke",
        killed = report.killed_sessions.len(),
        deleted = report.deleted_workspaces.len(),
        errors = report.errors.len(),
        "created smoke resources cleaned"
    );
    report
}
