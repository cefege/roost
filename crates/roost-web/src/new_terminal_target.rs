//! Which machine a new terminal opens on: the online machines in menu order,
//! and the default among them. Ports `apps/web/src/lib/newTerminalTarget.ts`;
//! the sidebar's new-terminal bar and the home landing's compact affordance
//! read it, so the online filter and the route preference stay one answer.

use roost_client_core::Store;
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::selectors::all_sessions;
use roost_protocol::wire::Worker;

/// Online machines sorted by label, case-insensitively, then by fingerprint.
pub fn online_workers_by_label(store: &Store, now_ms: i64) -> Vec<&Worker> {
    let mut workers: Vec<&Worker> = store
        .workers
        .values()
        .filter(|worker| worker_online(worker, store.routable_worker_fps.as_ref(), now_ms))
        .collect();
    workers.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.fp.as_str().cmp(right.fp.as_str()))
    });
    workers
}

/// The route's machine when it is online, else the machine of the newest
/// session that is online, else the first online machine, else none.
///
/// `active_worker_fp` is the worker of the session the current route names
/// (`route_session::active_session_for_path`), passed in so this stays a pure
/// read of the store.
pub fn default_new_terminal_worker_fp(
    store: &Store,
    now_ms: i64,
    active_worker_fp: Option<&str>,
) -> Option<String> {
    let workers = online_workers_by_label(store, now_ms);
    let is_online = |fp: &str| workers.iter().any(|worker| worker.fp.as_str() == fp);
    if let Some(active) = active_worker_fp.filter(|fp| is_online(fp)) {
        return Some(active.to_owned());
    }
    let mut sessions = all_sessions(store);
    sessions.sort_by_key(|session| std::cmp::Reverse(session.created_at));
    sessions
        .iter()
        .map(|session| session.worker_fp.as_str())
        .find(|fp| is_online(fp))
        .or_else(|| workers.first().map(|worker| worker.fp.as_str()))
        .map(str::to_owned)
}

/// The machine a new terminal opens on: the user's pick while it stays online,
/// else the default.
pub fn effective_new_terminal_target(
    selected: Option<&str>,
    online: &[&Worker],
    default: Option<String>,
) -> Option<String> {
    selected
        .filter(|fp| online.iter().any(|worker| worker.fp.as_str() == *fp))
        .map(str::to_owned)
        .or(default)
}
