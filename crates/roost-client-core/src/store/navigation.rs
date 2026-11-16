//! Navigation search: one scalar index the search page, the sidebar filter, and
//! the command palette all read.
//!
//! A detached row. No store proxy, no live reference, no closure — a document is
//! built from the store's records once and read by every surface, so the three of
//! them cannot disagree about what a session is called or whether its machine is
//! reachable. The reason it matters is that a proxy handed to a subscriber
//! reads the POST-write value (`docs/FAILURE-INDEX.md:50`).
//!
//! DEVIATION: the query normalizer does not apply NFKC. `unicode-normalization`
//! is not in this crate's dependency set, and a search index that lowercased one
//! side of a query and normalised the other would be worse than one that
//! consistently does neither.

pub mod projection;
pub mod query;

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::wire::{SessionMap, Worker};

use crate::store::optimistic_spawn::ClientOnlySession;
use crate::store::paths::WorkerPaths;

pub use projection::project_navigation_search_documents;

/// The freshness fallback for worker reachability, in milliseconds.
pub const WORKER_FRESHNESS_MS: i64 = 90_000;

/// Everything the projection reads, named.
#[derive(Debug)]
pub struct NavigationSources<'a> {
    /// The authoritative session rows.
    pub sessions: &'a SessionMap,
    /// The worker registry, by fingerprint.
    pub workers: &'a BTreeMap<String, Worker>,
    /// The workspaces, for a folder's name.
    pub workspaces: &'a [roost_protocol::wire::Workspace],
    /// The OSC-0/OSC-2 title the terminal core published, by session.
    pub terminal_titles: &'a BTreeMap<String, String>,
    /// The coordinator's last-activity stamp, by session.
    pub last_activity_ms: &'a BTreeMap<String, i64>,
    /// The fingerprints the coordinator can reach RIGHT NOW, or `None` before the
    /// first list. `None` is not "nothing is reachable": it is "ask the heartbeat
    /// instead", so a pre-bootstrap render is not all-offline.
    pub routable_worker_fps: Option<&'a BTreeSet<String>>,
    /// The host's reading of the clock, for the freshness fallback.
    pub now_ms: i64,
    /// The path codec, for folder identity and a folder's display name.
    pub paths: &'a dyn WorkerPaths,
    /// The placeholders this browser is holding, which are sessions the
    /// coordinator has not published yet.
    pub client_only: &'a [ClientOnlySession],
}

/// One row. Detached, scalar, and owned by nobody but its caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationSearchDocument {
    /// The session, or the placeholder this browser minted for it.
    pub session_id: String,
    /// Where the row navigates to.
    pub href: String,
    /// What the row shows as its title.
    pub display_title: String,
    /// The user's own rename, when there is one.
    pub custom_title: Option<String>,
    /// The terminal's own OSC title, when the program set one.
    pub terminal_title: Option<String>,
    /// The live folder, which follows a `cd`.
    pub cwd: String,
    /// The immutable spawn folder.
    pub spawn_cwd: Option<String>,
    /// The workspace the folder belongs to.
    pub workspace_id: Option<String>,
    /// That workspace's name, when it has been renamed.
    pub workspace_name: Option<String>,
    /// The stable per-(worker, folder) key.
    pub folder_key: String,
    /// What the machine is called.
    pub worker_label: String,
    /// The machine's fingerprint.
    pub worker_fp: String,
    /// The branch, when Git reported one.
    pub git_branch: Option<String>,
    /// The origin, distinguishing "never resolved" from "resolved, no origin".
    pub git_remote: Option<String>,
    /// The pull request's number.
    pub pull_request_number: Option<i64>,
    /// The pull request's state.
    pub pull_request_state: Option<roost_protocol::wire::PullRequestState>,
    /// The pull request's checks.
    pub pull_request_checks: Option<roost_protocol::wire::PullRequestChecks>,
    /// The pull request's URL.
    pub pull_request_url: Option<String>,
    /// The listening ports, as the searchable `:8080 :9000` label.
    pub port_label: Option<String>,
    /// Every field, normalized once, lowercased, and joined. What the filter
    /// matches; not displayed.
    pub search_text: String,
    /// What the row sorts by. Coord-stamped for an open session, its own
    /// `created_at` otherwise.
    pub activity_at: i64,
    /// Whether an operator could actually get to this row: open, on a known
    /// machine, and that machine reachable now.
    pub available: bool,
    /// Whether this browser minted the row and the coordinator has not published
    /// it yet.
    pub client_only: bool,
}

/// Where a session's row navigates to.
pub fn session_href(session_id: &str) -> String {
    format!("/s/{session_id}")
}

/// Whether a worker is usable right now.
///
/// The routable set when there is one, and heartbeat freshness before the first
/// list. Reachability is not heartbeat freshness in general — a worker can keep
/// heartbeating over the unary transport while its socket is down — but before
/// the first list there is nothing else to ask.
pub fn worker_online(
    worker: &Worker,
    routable_worker_fps: Option<&BTreeSet<String>>,
    now_ms: i64,
) -> bool {
    match routable_worker_fps {
        Some(routable) => routable.contains(worker.fp.as_str()),
        None => now_ms.saturating_sub(worker.last_seen_ms) < WORKER_FRESHNESS_MS,
    }
}
