//! The (machine, folder) buckets the sidebar's Folders panel and the home
//! folder grid render, and the query filter over them. Ports
//! `apps/web/src/lib/folderGroups.ts` and the folder helpers of
//! `apps/web/src/lib/folderKey.ts` it reads (`folderPathOf`,
//! `workspaceForFolder`, `folderDisplayName`). Pure reads of the store; the
//! folder key is `store::paths::folder_key_of`, the same key the deck uses.

use roost_protocol::wire::{PullRequestChecks, PullRequestState, Session, SessionKind, Workspace};

use crate::client::agents::status_policy::{
    AgentStatusRollup, derive_agent_status_level, fold_agent_status_levels,
};
use crate::store::Store;
use crate::store::navigation::query::normalize_navigation_search_query;
use crate::store::navigation::worker_online;
use crate::store::paths::WorkerPaths;
use crate::store::pending_close::is_pending_close;
use crate::store::selectors::{all_sessions, session_folder_key};
use crate::store::sidebar::format::short_server_label;

/// The subtitle a folder on an unreachable machine shows.
pub const OFFLINE_SUBTITLE: &str = "Machine offline — reopen to refresh";

/// The lead session's pull request, shaped for the row badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBadge {
    /// The PR number.
    pub number: i64,
    /// Open, merged, closed or draft; `open` when the worker did not say.
    pub state: PullRequestState,
    /// The checks rollup; `none` when the worker did not say.
    pub checks: PullRequestChecks,
    /// The PR's URL; empty when unknown, which the badge reads as "not a link".
    pub url: String,
}

/// One folder row: plain scalars, so a rebuild compares by value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderGroup {
    /// `folder_key_of` of the bucket.
    pub key: String,
    /// The workspace name, or the folder's basename, or the whole path.
    pub name: String,
    /// The machine's short label.
    pub server: String,
    /// The machine the bucket lives on.
    pub spawn_fp: String,
    /// The bucket's folder (the live cwd of its first session).
    pub spawn_cwd: String,
    /// Whether the machine is reachable now.
    pub online: bool,
    /// The offline explanation; empty when reachable.
    pub subtitle: String,
    /// The newest terminal activity, falling back to creation.
    pub latest_activity: i64,
    /// The most recently active session: the row's click target.
    pub lead_id: String,
    /// Every session in the bucket.
    pub session_ids: Vec<String>,
    /// The lead's pull request badge.
    pub pr: Option<PrBadge>,
    /// The lead's git branch.
    pub branch: Option<String>,
    /// The union of listening ports across the bucket, ascending.
    pub ports: Vec<i64>,
    /// The machine's reachable address, for port click-through.
    pub reach_addr: Option<String>,
    /// The highest agent level present and the per-level counts.
    pub agent_status: AgentStatusRollup,
}

/// The glyph a checks rollup shows beside `#123`; `none` shows none.
pub const fn pr_check_glyph(checks: PullRequestChecks) -> &'static str {
    match checks {
        PullRequestChecks::Passing => "✓",
        PullRequestChecks::Failing => "✕",
        PullRequestChecks::Pending => "•",
        PullRequestChecks::None => "",
    }
}

/// The colour token the checks glyph paints in.
pub const fn pr_check_color(checks: PullRequestChecks) -> &'static str {
    match checks {
        PullRequestChecks::Passing => "var(--color-ok)",
        PullRequestChecks::Failing => "var(--color-warn)",
        PullRequestChecks::Pending | PullRequestChecks::None => "var(--text-lo)",
    }
}

/// Terminals live in their current cwd.
pub fn folder_path_of(session: &Session) -> &str {
    &session.cwd
}

/// The workspace backing a (machine, folder) bucket, resolved by folder and
/// never by `session.workspace_id`, so a rename shows whichever session leads.
pub fn workspace_for_folder<'store>(
    store: &'store Store,
    paths: &dyn WorkerPaths,
    worker_fp: &str,
    folder_path: &str,
) -> Option<&'store Workspace> {
    let worker_os = worker_os(store, worker_fp);
    store.workspaces.values().find(|workspace| {
        workspace.worker_fp.as_str() == worker_fp
            && paths.same_folder(worker_os, &workspace.folder_path, folder_path)
    })
}

/// A folder row's label: its workspace's name when renamed, else the folder's
/// basename, else the whole path.
pub fn folder_display_name(store: &Store, paths: &dyn WorkerPaths, session: &Session) -> String {
    let path = folder_path_of(session);
    let worker_fp = session.worker_fp.as_str();
    if let Some(workspace) = workspace_for_folder(store, paths, worker_fp, path)
        && !workspace.name.is_empty()
    {
        return workspace.name.clone();
    }
    paths
        .basename(worker_os(store, worker_fp), path)
        .filter(|basename| !basename.is_empty())
        .unwrap_or_else(|| path.to_owned())
}

/// Bucket every live shell session by folder, newest activity first.
pub fn build_folder_groups(
    store: &Store,
    paths: &dyn WorkerPaths,
    now_ms: i64,
) -> Vec<FolderGroup> {
    build_folder_groups_from(store, paths, now_ms, &all_sessions(store))
}

/// [`build_folder_groups`] over an explicit session list.
pub fn build_folder_groups_from(
    store: &Store,
    paths: &dyn WorkerPaths,
    now_ms: i64,
    sessions: &[&Session],
) -> Vec<FolderGroup> {
    let mut buckets: Vec<(String, Vec<&Session>)> = Vec::new();
    for session in sessions.iter().copied().filter(|session| {
        session.kind == SessionKind::Shell && !is_pending_close(store, session.id.as_str())
    }) {
        let key = session_folder_key(store, paths, session);
        match buckets.iter_mut().find(|(bucket_key, _)| *bucket_key == key) {
            Some((_, members)) => members.push(session),
            None => buckets.push((key, vec![session])),
        }
    }
    let mut groups: Vec<FolderGroup> = buckets
        .into_iter()
        .filter_map(|(key, members)| group_of(store, paths, now_ms, key, &members))
        .collect();
    groups.sort_by(|left, right| right.latest_activity.cmp(&left.latest_activity));
    groups
}

/// Keep a folder when every normalized query term occurs in its name, machine
/// or path. An empty query keeps every folder in order.
pub fn filter_folder_groups(groups: &[FolderGroup], query: &str) -> Vec<FolderGroup> {
    let normalized = normalize_navigation_search_query(query);
    if normalized.is_empty() {
        return groups.to_vec();
    }
    let terms: Vec<&str> = normalized.split(' ').collect();
    groups
        .iter()
        .filter(|group| {
            let text = normalize_navigation_search_query(
                &[group.name.as_str(), &group.server, &group.spawn_cwd].join("\n"),
            );
            terms.iter().all(|term| text.contains(term))
        })
        .cloned()
        .collect()
}

fn group_of(
    store: &Store,
    paths: &dyn WorkerPaths,
    now_ms: i64,
    key: String,
    members: &[&Session],
) -> Option<FolderGroup> {
    let head = *members.first()?;
    let mut lead = head;
    for candidate in members {
        if recency_of(store, candidate) > recency_of(store, lead) {
            lead = candidate;
        }
    }
    let worker = store.workers.get(head.worker_fp.as_str());
    let online = worker.is_some_and(|worker| {
        worker_online(worker, store.routable_worker_fps.as_ref(), now_ms)
    });
    let mut ports: Vec<i64> = members
        .iter()
        .flat_map(|session| session.ports.iter().flatten().copied())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    let agent_status = fold_agent_status_levels(members.iter().map(|session| {
        let status = store.agent_status.status(&session.id);
        derive_agent_status_level(
            status,
            status.map(|status| store.agent_seen.acknowledged_revision(status)),
        )
    }));
    let fallback_label: String = head.worker_fp.as_str().chars().take(6).collect();
    Some(FolderGroup {
        key,
        name: folder_display_name(store, paths, head),
        server: short_server_label(worker.map_or(fallback_label.as_str(), |w| w.label.as_str())),
        spawn_fp: head.worker_fp.to_string(),
        spawn_cwd: folder_path_of(head).to_owned(),
        online,
        subtitle: if online { String::new() } else { OFFLINE_SUBTITLE.to_owned() },
        latest_activity: members
            .iter()
            .map(|session| recency_of(store, session))
            .max()
            .unwrap_or(head.created_at),
        lead_id: lead.id.to_string(),
        session_ids: members.iter().map(|session| session.id.to_string()).collect(),
        pr: lead.pr_number.map(|number| PrBadge {
            number,
            state: lead.pr_state.unwrap_or(PullRequestState::Open),
            checks: lead.pr_checks.unwrap_or(PullRequestChecks::None),
            url: lead.pr_url.clone().unwrap_or_default(),
        }),
        branch: lead.git_branch.clone(),
        ports,
        reach_addr: worker.and_then(|worker| worker.reachable_addr.clone()),
        agent_status,
    })
}

/// Last terminal byte, falling back to creation before the coordinator has
/// seen one.
fn recency_of(store: &Store, session: &Session) -> i64 {
    store
        .last_activity_ms
        .get(session.id.as_str())
        .copied()
        .unwrap_or(0)
        .max(session.created_at)
}

fn worker_os<'store>(store: &'store Store, worker_fp: &str) -> Option<&'store str> {
    store.workers.get(worker_fp).map(|worker| worker.os.as_str())
}
