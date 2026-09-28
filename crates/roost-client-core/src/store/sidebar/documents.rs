//! The store's navigation documents: `store::navigation`'s projection fed from
//! the live store, with the agent facts resolved by their owners (the status
//! policy and the seen ledger). Ports v2 `navigationSearchDocuments()` from
//! `apps/web/src/store/navigation-search.ts`; the Agents panel reads it, and
//! any other surface that needs the live index calls the same function.

use std::collections::BTreeMap;

use roost_protocol::wire::Workspace;

use crate::client::agents::status_policy::{
    agent_status_completion_unseen, agent_status_level_token_for,
};
use crate::store::Store;
use crate::store::navigation::{
    AgentStatusFacts, NavigationSearchDocument, NavigationSources, attention_for_level_token,
    project_navigation_search_documents,
};
use crate::store::paths::WorkerPaths;

/// Every row the client can see, most recently active first.
pub fn store_navigation_documents(
    store: &Store,
    paths: &dyn WorkerPaths,
    now_ms: i64,
) -> Vec<NavigationSearchDocument> {
    let agent_status = store_agent_facts(store);
    let workspaces: Vec<Workspace> = store.workspaces.values().cloned().collect();
    let client_only = store.spawns.client_only_sessions();
    project_navigation_search_documents(&NavigationSources {
        sessions: store.sessions.sessions(),
        workers: &store.workers,
        workspaces: &workspaces,
        terminal_titles: &store.terminal_titles,
        last_activity_ms: &store.last_activity_ms,
        agent_status: &agent_status,
        routable_worker_fps: store.routable_worker_fps.as_ref(),
        now_ms,
        paths,
        client_only: &client_only,
    })
}

/// The agent facts per session, as the status owners resolve them.
pub fn store_agent_facts(store: &Store) -> BTreeMap<String, AgentStatusFacts> {
    store
        .agent_status
        .statuses()
        .iter()
        .map(|(session_id, status)| {
            let acknowledged = Some(store.agent_seen.acknowledged_revision(status));
            let level_token = agent_status_level_token_for(Some(status), acknowledged);
            let facts = AgentStatusFacts {
                level_token: Some(level_token.to_owned()),
                attention: attention_for_level_token(level_token),
                unseen: agent_status_completion_unseen(status, acknowledged),
                agent_id: Some(status.common.agent_id.as_str().to_owned()),
                message: status.common.message.clone(),
                updated_at_ms: Some(status.common.updated_at),
                arrival: store.agent_status.arrival(session_id),
            };
            (session_id.to_string(), facts)
        })
        .collect()
}
