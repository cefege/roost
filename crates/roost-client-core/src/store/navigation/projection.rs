//! The per-session and per-placeholder projections, and the small helpers they
//! share.
//!
//! Split from the parent because a document is DATA and a projection is a RULE,
//! and the rule is where a new field or a new title fallback actually lands. The
//! types stay in the parent so the query half and the palette both read one
//! definition of a row.

use roost_protocol::wire::{Session, SessionStatus};

use super::query;
use super::{NavigationSearchDocument, NavigationSources, session_href, worker_online};
use crate::store::optimistic_spawn::ClientOnlySession;
use crate::store::paths::{WorkerPaths, folder_key_of};

/// Project every row the client can see, most recently active first.
pub fn project_navigation_search_documents(
    sources: &NavigationSources<'_>,
) -> Vec<NavigationSearchDocument> {
    let mut documents: Vec<NavigationSearchDocument> = sources
        .sessions
        .values()
        .map(|session| project_session(session, sources))
        .collect();
    documents.extend(
        sources
            .client_only
            .iter()
            .map(|placeholder| project_client_only(placeholder, sources)),
    );
    documents.sort_by(compare_by_activity);
    documents
}

fn project_session(session: &Session, sources: &NavigationSources<'_>) -> NavigationSearchDocument {
    let worker = sources.workers.get(session.worker_fp.as_str());
    let worker_os = worker.map(|row| row.os.as_str());
    let facts = sources
        .agent_status
        .get(session.id.as_str())
        .cloned()
        .unwrap_or_default();
    let custom_title = clean(session.custom_title.as_deref());
    let terminal_title = clean(
        sources
            .terminal_titles
            .get(session.id.as_str())
            .map(String::as_str),
    );
    let spawn_cwd = clean(session.spawn_cwd.as_deref());
    let workspace =
        workspace_for_folder(sources, worker_os, session.worker_fp.as_str(), &session.cwd);
    let workspace_name = clean(workspace.map(|row| row.name.as_str()));
    let worker_label = clean(worker.map(|row| row.label.as_str()))
        .unwrap_or_else(|| session.worker_fp.to_string());
    let git_branch = clean(session.git_branch.as_deref());
    let git_remote = clean(
        session
            .git_remote
            .as_ref()
            .and_then(|remote| remote.as_deref()),
    );
    let pull_request_url = clean(session.pr_url.as_deref());
    let port_label = port_label(session.ports.as_deref());
    let is_open = session.status == SessionStatus::Open;
    let available = is_open
        && worker
            .is_some_and(|row| worker_online(row, sources.routable_worker_fps, sources.now_ms));
    let display_title = session_title(
        custom_title.as_deref(),
        terminal_title.as_deref(),
        sources.paths,
        worker_os,
        &session.cwd,
    );
    let search_text = search_text(&[
        Some(session.id.as_str()),
        Some(display_title.as_str()),
        custom_title.as_deref(),
        terminal_title.as_deref(),
        Some(session.cwd.as_str()),
        spawn_cwd.as_deref(),
        workspace_name.as_deref(),
        Some(worker_label.as_str()),
        Some(session.worker_fp.as_str()),
        git_branch.as_deref(),
        git_remote.as_deref(),
        session
            .pr_number
            .map(|number| number.to_string())
            .as_deref(),
        session
            .pr_number
            .map(|number| format!("#{number}"))
            .as_deref(),
        session.pr_state.map(|state| state.as_str()),
        session.pr_checks.map(|checks| checks.as_str()),
        pull_request_url.as_deref(),
        port_label.as_deref(),
        facts.agent_id.as_deref(),
        facts.level_token.as_deref(),
        facts.message.as_deref(),
        Some(if available {
            "available online"
        } else {
            "unavailable offline"
        }),
    ]);
    NavigationSearchDocument {
        session_id: session.id.to_string(),
        href: session_href(session.id.as_str()),
        display_title,
        custom_title,
        terminal_title,
        cwd: session.cwd.clone(),
        workspace_id: workspace.map(|row| row.id.to_string()),
        spawn_cwd,
        workspace_name,
        folder_key: folder_key_of(
            sources.paths,
            worker_os,
            session.worker_fp.as_str(),
            &session.cwd,
        ),
        worker_label,
        worker_fp: session.worker_fp.to_string(),
        git_branch,
        git_remote,
        pull_request_number: session.pr_number,
        pull_request_state: session.pr_state,
        pull_request_checks: session.pr_checks,
        pull_request_url,
        port_label,
        search_text,
        activity_at: if is_open {
            sources
                .last_activity_ms
                .get(session.id.as_str())
                .copied()
                .unwrap_or(session.created_at)
        } else {
            session.closed_at.unwrap_or(session.created_at)
        },
        available,
        agent_status: facts.level_token.clone(),
        agent_attention: facts.attention,
        agent_unseen: facts.unseen,
        agent_id: facts.agent_id.clone(),
        agent_message: facts.message.clone(),
        agent_updated_at_ms: facts.updated_at_ms,
        agent_arrival: facts.arrival,
        client_only: false,
    }
}

/// A placeholder this browser minted. Every field the coordinator has not
/// published yet is absent rather than invented.
fn project_client_only(
    placeholder: &ClientOnlySession,
    sources: &NavigationSources<'_>,
) -> NavigationSearchDocument {
    let worker = sources.workers.get(placeholder.worker_fp.as_str());
    let worker_os = worker.map(|row| row.os.as_str());
    let worker_label = clean(worker.map(|row| row.label.as_str()))
        .unwrap_or_else(|| placeholder.worker_fp.clone());
    let display_title = session_title(None, None, sources.paths, worker_os, &placeholder.cwd);
    let workspace =
        workspace_for_folder(sources, worker_os, &placeholder.worker_fp, &placeholder.cwd);
    let search_text = search_text(&[
        Some(placeholder.id.as_str()),
        Some(display_title.as_str()),
        Some(placeholder.worker_fp.as_str()),
        Some(&placeholder.cwd),
    ]);
    NavigationSearchDocument {
        session_id: placeholder.id.clone(),
        href: session_href(&placeholder.id),
        display_title,
        custom_title: None,
        terminal_title: None,
        cwd: placeholder.cwd.clone(),
        workspace_id: workspace.map(|row| row.id.to_string()),
        spawn_cwd: clean(Some(placeholder.spawn_cwd.as_str())),
        workspace_name: clean(workspace.map(|row| row.name.as_str())),
        folder_key: folder_key_of(
            sources.paths,
            worker_os,
            &placeholder.worker_fp,
            &placeholder.cwd,
        ),
        worker_label,
        worker_fp: placeholder.worker_fp.clone(),
        git_branch: None,
        git_remote: None,
        pull_request_number: None,
        pull_request_state: None,
        pull_request_checks: None,
        pull_request_url: None,
        port_label: None,
        search_text,
        activity_at: placeholder.created_at_ms,
        // A placeholder is on a machine this browser just asked, and there is
        // nothing authoritative to contradict it yet.
        available: true,
        agent_status: None,
        agent_attention: None,
        agent_unseen: false,
        agent_id: None,
        agent_message: None,
        agent_updated_at_ms: None,
        agent_arrival: 0,
        client_only: true,
    }
}

/// The workspace backing a (worker, folder) bucket.
///
/// Resolved by FOLDER and not by `session.workspace_id`, which is what makes a
/// folder's custom name show regardless of which session leads the row, and what
/// re-attaches the name when a `cd`'d terminal returns to its workspace's folder.
fn workspace_for_folder<'a>(
    sources: &'a NavigationSources<'a>,
    worker_os: Option<&'a str>,
    worker_fp: &str,
    folder_path: &str,
) -> Option<&'a roost_protocol::wire::Workspace> {
    sources.workspaces.iter().find(|row| {
        row.worker_fp.as_str() == worker_fp
            && sources
                .paths
                .same_folder(worker_os, &row.folder_path, folder_path)
    })
}

/// What a session is called: the user's rename, then the program's title, then
/// the folder's own name.
fn session_title(
    custom_title: Option<&str>,
    terminal_title: Option<&str>,
    paths: &dyn WorkerPaths,
    worker_os: Option<&str>,
    cwd: &str,
) -> String {
    if let Some(title) = custom_title.or(terminal_title) {
        return title.to_owned();
    }
    paths
        .basename(worker_os, cwd)
        .filter(|basename| !basename.is_empty())
        .unwrap_or_else(|| "shell".to_owned())
}

fn port_label(ports: Option<&[i64]>) -> Option<String> {
    let mut ports: Vec<i64> = ports
        .unwrap_or_default()
        .iter()
        .copied()
        .filter(|port| *port > 0)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    if ports.is_empty() {
        return None;
    }
    Some(
        ports
            .into_iter()
            .map(|port| format!(":{port}"))
            .collect::<Vec<String>>()
            .join(" "),
    )
}

fn search_text(parts: &[Option<&str>]) -> String {
    let joined: Vec<&str> = parts.iter().filter_map(|part| *part).collect();
    query::normalize_navigation_search_query(&joined.join("\n"))
}

/// Trim, and read as absent when nothing is left.
fn clean(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Newest first, and the session id breaks a tie so the order is TOTAL: two rows
/// stamped in the same millisecond must still have one order, or a filter run
/// reshuffles the list under the reader's finger.
fn compare_by_activity(
    left: &NavigationSearchDocument,
    right: &NavigationSearchDocument,
) -> std::cmp::Ordering {
    right
        .activity_at
        .cmp(&left.activity_at)
        .then_with(|| left.session_id.cmp(&right.session_id))
}
