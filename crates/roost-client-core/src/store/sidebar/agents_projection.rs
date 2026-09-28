//! The Agents panel's projection: navigation documents filtered by the query,
//! kept only for open shell sessions whose retained agent status reads as a
//! known level, grouped under the Folders panel's folder order, and ordered by
//! attention inside each folder. Ports
//! `apps/web/src/components/sidebar/sidebarAgentsProjection.ts`; `SidebarAgents`
//! renders it.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::wire::{AgentStatus, SessionKind, SessionStatus};

use crate::client::agents::status_policy::{
    AgentStatusLevel, agent_status_presentation, derive_agent_status_level,
};
use crate::store::Store;
use crate::store::navigation::NavigationSearchDocument;
use crate::store::navigation::query::filter_navigation_search_documents;
use crate::store::paths::WorkerPaths;
use crate::store::selectors::{session_by_id, session_folder_key};
use crate::store::sidebar::folder_groups::FolderGroup;

/// One agent row.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarAgentRow {
    /// The row's navigation document.
    pub document: NavigationSearchDocument,
    /// The retained status behind it.
    pub status: AgentStatus,
    /// Its level; never `Unknown`.
    pub level: AgentStatusLevel,
}

/// One folder's agent rows.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarAgentGroup {
    /// The folder, as the Folders panel ordered it.
    pub folder: FolderGroup,
    /// Highest attention first, then by session id.
    pub rows: Vec<SidebarAgentRow>,
}

/// Group the query's agent rows under `folder_groups`, in that order.
pub fn project_sidebar_agent_groups(
    store: &Store,
    paths: &dyn WorkerPaths,
    documents: &[NavigationSearchDocument],
    query: &str,
    folder_groups: &[FolderGroup],
) -> Vec<SidebarAgentGroup> {
    let mut rows_by_folder: BTreeMap<String, Vec<SidebarAgentRow>> = BTreeMap::new();
    for document in filter_navigation_search_documents(documents, query) {
        let Some(session) = session_by_id(store, &document.session_id) else {
            continue;
        };
        if session.kind != SessionKind::Shell || session.status != SessionStatus::Open {
            continue;
        }
        let Some(status) = store.agent_status.status(&session.id) else {
            continue;
        };
        let level = derive_agent_status_level(
            Some(status),
            Some(store.agent_seen.acknowledged_revision(status)),
        );
        if level == AgentStatusLevel::Unknown {
            continue;
        }
        rows_by_folder
            .entry(session_folder_key(store, paths, session))
            .or_default()
            .push(SidebarAgentRow {
                document: document.clone(),
                status: status.clone(),
                level,
            });
    }
    folder_groups
        .iter()
        .filter_map(|folder| {
            let members: BTreeSet<&str> = folder.session_ids.iter().map(String::as_str).collect();
            let mut rows: Vec<SidebarAgentRow> = rows_by_folder
                .remove(&folder.key)?
                .into_iter()
                .filter(|row| members.contains(row.document.session_id.as_str()))
                .collect();
            rows.sort_by(|left, right| {
                agent_status_presentation(right.level)
                    .priority
                    .cmp(&agent_status_presentation(left.level).priority)
                    .then_with(|| left.document.session_id.cmp(&right.document.session_id))
            });
            (!rows.is_empty()).then(|| SidebarAgentGroup {
                folder: folder.clone(),
                rows,
            })
        })
        .collect()
}
