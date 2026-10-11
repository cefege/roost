//! Save-time checks for folder model-role rows. A row is keyed by the exact
//! worker and working directory; the saved document keeps only real overrides,
//! and every chain is checked the same way as a global role value.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::wire::agent_chat::FolderModelRoles;

use crate::error::AgentError;
use crate::records::Role;
use crate::roles;

/// Returns the rows to store: blank overrides and rows left with none are
/// dropped. Rows without a worker or directory, duplicate pairs, unknown role
/// names and invalid chains are rejected.
pub fn prepare_folder_rows(rows: &[FolderModelRoles]) -> Result<Vec<FolderModelRoles>, AgentError> {
    let mut keys = BTreeSet::new();
    let mut prepared = Vec::new();
    for row in rows {
        if row.worker_fp.trim().is_empty() || row.cwd.trim().is_empty() {
            return Err(AgentError::InvalidArgument(
                "a folder model override needs a worker and a working directory".into(),
            ));
        }
        if !keys.insert((row.worker_fp.as_str(), row.cwd.as_str())) {
            return Err(AgentError::InvalidArgument(format!(
                "duplicate model overrides for folder {}",
                row.cwd
            )));
        }
        let mut model_roles = BTreeMap::new();
        for (name, chain) in &row.model_roles {
            let role = Role::from_name(name)
                .ok_or_else(|| AgentError::InvalidArgument(format!("unknown role {name}")))?;
            if chain.trim().is_empty() {
                continue;
            }
            roles::validate_role_selector(role, chain).map_err(|error| {
                AgentError::InvalidArgument(format!("role {}: {error}", role.as_str()))
            })?;
            model_roles.insert(name.clone(), chain.clone());
        }
        if !model_roles.is_empty() {
            prepared.push(FolderModelRoles {
                worker_fp: row.worker_fp.clone(),
                cwd: row.cwd.clone(),
                model_roles,
            });
        }
    }
    Ok(prepared)
}
