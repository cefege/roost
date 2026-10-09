//! What one Connect unary call answered, already mapped off the wire.
//!
//! Owned by `effect`; produced by `client::rpc::codec::decode_rpc_response` (or
//! by the host as `Failed`), handed back as `ClientEvent::RpcResultReceived`,
//! and folded by `handle_sync::handle_rpc_result`. Row shapes are the ones v2's
//! hydrators build (`apps/web/src/store/sync-bootstrap-hydration.ts`).

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::wire::{McpRelay, SessionMap, Task, Worker, Workspace};

use crate::client::rpc::CallError;
use crate::search::global::GlobalSearchResponse;
use crate::store::browse_entries::BrowseEntry;
use crate::store::mutations::PairRequest;

/// One Connect unary response.
/// Only `PartialEq`: the rows it carries are wire types, which are `PartialEq`
/// and not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub enum RpcResult {
    /// The call failed. The client reports it; it does not retry on its own,
    /// because every call here is either idempotent (in which case the host's
    /// dial loop decides) or a ceremony step a human drives.
    Failed {
        /// Which call this answers.
        call_id: u64,
        /// What went wrong: the network, the coordinator's Connect refusal
        /// (code and auth layer, which `classifyAuthFailure` needs), or the codec.
        error: CallError,
    },
    /// `AuthCoordIdentity` succeeded: the two fields v2 keeps as
    /// `coord_identity` (`sync-bootstrap.ts:152-156`), plus the direct
    /// carrier's static STUN settings.
    CoordIdentity {
        /// Which call this answers.
        call_id: u64,
        /// The coordinator's build, for the drift badge.
        git_sha: String,
        /// The coordinator's public URL.
        public_url: String,
        /// `Some(urls)` when the coordinator's direct carrier is enabled, so a
        /// machine may gather before its grant lands; `None` when it is off.
        terminal_peer_stun_urls: Option<Vec<String>>,
        /// Whether built-in agent RPCs are configured.
        builtin_agent_enabled: bool,
    },
    /// `SessionsList` succeeded.
    SessionsList {
        /// Which call this answers.
        call_id: u64,
        /// The session rows that decoded. A row that failed the brand check is
        /// dropped with a warning, as v2 does, rather than failing the snapshot.
        sessions: SessionMap,
        /// The one-time terminal hydration token. `None` when the coordinator
        /// sent none or sent it empty — v2 treats both as missing and redials.
        terminal_snapshot_token: Option<String>,
    },
    /// `WorkersList` succeeded.
    WorkersList {
        /// Which call this answers.
        call_id: u64,
        /// The worker rows, keyed by fingerprint.
        workers: BTreeMap<String, Worker>,
        /// The fingerprints the coordinator can route to right now — the
        /// authoritative "online" signal, stronger than heartbeat freshness.
        routable_fps: BTreeSet<String>,
    },
    /// `WorkspacesList` succeeded.
    WorkspacesList {
        /// Which call this answers.
        call_id: u64,
        /// The workspace rows, keyed by workspace id.
        workspaces: BTreeMap<String, Workspace>,
    },
    /// `TasksList` succeeded.
    TasksList {
        /// Which call this answers.
        call_id: u64,
        /// The task rows, keyed by task id. A row whose JSON columns do not
        /// parse is dropped, as v2 does.
        tasks: BTreeMap<String, Task>,
    },
    /// `McpList` succeeded.
    McpList {
        /// Which call this answers.
        call_id: u64,
        /// The relay rows, keyed by relay id. A row whose config does not parse
        /// is dropped, as v2 does.
        relays: BTreeMap<String, McpRelay>,
    },
    /// `PairList` succeeded.
    PairList {
        /// Which call this answers.
        call_id: u64,
        /// The pending requests, keyed by ephemeral id.
        requests: BTreeMap<String, PairRequest>,
    },
    /// Agent conversation list and host connectivity.
    AgentChatList {
        call_id: u64,
        conversations: Vec<roost_protocol::wire::agent_chat::ConversationSummary>,
        host_connected: bool,
    },
    /// A pairing token was redeemed.
    PairTokenRedeemed {
        /// Which call this answers.
        call_id: u64,
    },
    /// `FilesListDir` answered with one listing.
    DirectoryListed {
        /// Which call this answers.
        call_id: u64,
        /// The machine that was read.
        worker_fp: String,
        /// The path that was asked for.
        path: String,
        /// Where the machine resolved it; the asked-for path when it said
        /// nothing (`browseDirectoryListing.ts:58`).
        resolved_path: String,
        /// The rows, in the machine's order.
        entries: Vec<BrowseEntry>,
    },
    /// `FilesMkdir` created a directory.
    DirectoryCreated {
        /// Which call this answers.
        call_id: u64,
        /// The machine written to.
        worker_fp: String,
        /// Where the directory ended up; the asked-for path when the machine
        /// said nothing (`browseNewFolder.ts:78`).
        resolved_path: String,
    },
    /// `SessionsSearchGlobal` answered with one page.
    ///
    /// Carries the `search_id` back as well as the page, because the answer is
    /// only meaningful against the identity that asked for it: a page arriving
    /// after the user cancelled, or after a second search replaced this one,
    /// must be dropped rather than appended. `next_cursor` is `None` when the
    /// coordinator has nothing more, and a page may be `truncated` at the
    /// coordinator's own caps and still carry a cursor.
    SearchPage {
        /// Which call this answers.
        call_id: u64,
        /// The search this page belongs to, as sent on the request.
        search_id: String,
        /// The page the coordinator returned.
        page: GlobalSearchResponse,
    },
    /// `SessionsCancelGlobalSearch` was acknowledged.
    GlobalSearchCancelled {
        /// Which call this answers.
        call_id: u64,
        /// The search the coordinator stopped.
        search_id: String,
    },
    /// `SessionsKill` answered; `accepted` is the coordinator's verdict.
    SessionKillAnswered {
        /// Which call this answers.
        call_id: u64,
        /// The session the kill targeted.
        session_id: String,
        /// Whether the kill was the forced one.
        force: bool,
        /// Whether the coordinator accepted it.
        accepted: bool,
    },
}

impl RpcResult {
    /// The call this answers.
    pub const fn call_id(&self) -> u64 {
        match self {
            Self::Failed { call_id, .. }
            | Self::CoordIdentity { call_id, .. }
            | Self::SessionsList { call_id, .. }
            | Self::WorkersList { call_id, .. }
            | Self::WorkspacesList { call_id, .. }
            | Self::TasksList { call_id, .. }
            | Self::McpList { call_id, .. }
            | Self::PairList { call_id, .. }
            | Self::PairTokenRedeemed { call_id }
            | Self::AgentChatList { call_id, .. }
            | Self::DirectoryListed { call_id, .. }
            | Self::DirectoryCreated { call_id, .. }
            | Self::SearchPage { call_id, .. }
            | Self::GlobalSearchCancelled { call_id, .. }
            | Self::SessionKillAnswered { call_id, .. } => *call_id,
        }
    }

    /// A short name for a log line.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Failed { .. } => "failed",
            Self::CoordIdentity { .. } => "coord_identity",
            Self::SessionsList { .. } => "sessions_list",
            Self::WorkersList { .. } => "workers_list",
            Self::WorkspacesList { .. } => "workspaces_list",
            Self::TasksList { .. } => "tasks_list",
            Self::McpList { .. } => "mcp_list",
            Self::PairList { .. } => "pair_list",
            Self::AgentChatList { .. } => "agent_chat_list",
            Self::PairTokenRedeemed { .. } => "pair_token_redeemed",
            Self::DirectoryListed { .. } => "directory_listed",
            Self::DirectoryCreated { .. } => "directory_created",
            Self::SearchPage { .. } => "search_page",
            Self::GlobalSearchCancelled { .. } => "global_search_cancelled",
            Self::SessionKillAnswered { .. } => "session_kill_answered",
        }
    }
}
