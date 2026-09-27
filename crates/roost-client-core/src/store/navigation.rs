//! Navigation search: one scalar index the search page, the sidebar filter, and
//! the command palette all read.
//!
//! A detached row. No store proxy, no live reference, no closure — a document is
//! built from the store's records once and read by every surface, so the three of
//! them cannot disagree about what a session is called or whether its machine is
//! reachable. `navigation-search.ts:2-4` states the same rule, and the reason it
//! matters is that a proxy handed to a subscriber reads the POST-write value
//! (`docs/FAILURE-INDEX.md:50`).
//!
//! The agent facts are an INPUT, not a derivation. v2 computes the level, the
//! attention state and the unseen flag inside `projectSession`
//! (`navigation-search.ts:97-184`), which reads as navigation's job and is not:
//! the level belongs to `client/agents/agentStatus.ts` and the seen revision to
//! `lib/agentSeen.ts`, and v2's own header says the status and seen owners stay
//! authoritative. So [`AgentStatusFacts`] is a carrier this module reads and never
//! populates, and there is no second level type in the crate for it to disagree
//! with.
//!
//! The query half — normalization, filtering, and the attention ordering — is in
//! `navigation::query`, because a filter is not a projection and the two are
//! changed for different reasons.
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

/// Why a row wants the operator's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationSearchAttention {
    /// An agent is blocked and cannot continue without a human.
    Blocked,
    /// An agent finished.
    Done,
}

impl NavigationSearchAttention {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }
}

/// Map the agent-status owner's level token onto the attention state a
/// navigation row shows.
///
/// THE ONE PLACE the two vocabularies meet, and a MAPPING rather than a second
/// derivation: the level, the unseen flag and the acknowledged revision belong to
/// `client::agents::agentStatus`, and re-deriving any of them here would be a
/// second answer to a question another module already owns. Five tokens in
/// (`blocked`, `working`, `done`, `idle`, `unknown`) and two states out, because
/// those are the only two that make a row want a human: an agent waiting on a
/// person, and an agent that has finished.
///
/// The attention ORDER is derived from the token too, not from the enum. A sort
/// keyed on the enum would still put a blocked row first today, and would keep
/// doing so after a fourth attention state arrived with a different order — with
/// neither this crate's tests nor the agent module's noticing.
pub fn attention_for_level_token(level_token: &str) -> Option<NavigationSearchAttention> {
    match level_token {
        "blocked" => Some(NavigationSearchAttention::Blocked),
        "done" => Some(NavigationSearchAttention::Done),
        _ => None,
    }
}

/// The rank an attention row sorts by, taken from the shared token vocabulary
/// and not from the enum's own order.
pub fn attention_rank(document: &NavigationSearchDocument) -> u8 {
    match document.agent_status.as_deref() {
        Some("blocked") => 0,
        Some("done") => 1,
        Some(_) => 2,
        None => 3,
    }
}

/// What the agent-status owner resolved for one session.
///
/// The projection reads these and derives nothing: the level, the attention state
/// and the unseen flag are all policy belonging to the module that owns agent
/// status, and a second derivation here would be a second answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentStatusFacts {
    /// The level as the owner spells it, for the search text. `None` when the
    /// session has no status.
    pub level_token: Option<String>,
    /// Whether the status wants attention, and of which kind.
    pub attention: Option<NavigationSearchAttention>,
    /// Whether the operator has not yet seen this status.
    pub unseen: bool,
    /// The agent's own id, for the search text.
    pub agent_id: Option<String>,
    /// The status message, for the card and the search text.
    pub message: Option<String>,
    /// When the worker observed it.
    pub updated_at_ms: Option<i64>,
    /// The BROWSER's arrival counter for this status, not a worker clock.
    ///
    /// Worker wall clocks are unsynchronized, so ordering by `updated_at_ms`
    /// would let one machine running minutes ahead own the top of the list.
    pub arrival: u64,
}

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
    /// The agent facts, by session, as the agent-status owner resolved them.
    pub agent_status: &'a BTreeMap<String, AgentStatusFacts>,
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
    /// The agent level as its owner spells it.
    pub agent_status: Option<String>,
    /// Whether the status wants attention, and of which kind.
    pub agent_attention: Option<NavigationSearchAttention>,
    /// Whether the operator has not seen it.
    pub agent_unseen: bool,
    /// The agent's id.
    pub agent_id: Option<String>,
    /// The status message.
    pub agent_message: Option<String>,
    /// When the worker observed the status. Display only; ordering uses arrival.
    pub agent_updated_at_ms: Option<i64>,
    /// The browser's arrival counter for the status.
    pub agent_arrival: u64,
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
/// heartbeating over the unary transport while its socket is down
/// (`sync-routable.ts:1-6`) — but before the first list there is nothing else to
/// ask.
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
