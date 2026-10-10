//! The Connect unary calls the client can ask the host to make.
//!
//! Owned by `effect`; emitted by the core as `Effect::Rpc` and by a host's
//! domain hydrator (`hydration_call`). Encoded by `client::rpc::codec`, named on
//! the wire by `client::rpc::methods`. The per-domain hydration calls are v2's
//! `apps/web/src/store/sync-bootstrap-hydration.ts:52-203`.

use crate::sync::link::SyncDomain;

/// One Connect unary call.
///
/// A closed set on purpose: an open-ended request enum is how a client crate
/// grows a transport. Adding a member is a deliberate edit, and adding one is
/// also the moment to decide whether the state machine needs a new event for the
/// answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcCall {
    /// `AuthCoordIdentity` — which coordinator build and URL this is.
    CoordIdentity {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `SessionsList` — the terminal domain's snapshot, and the source of the
    /// one-time terminal snapshot token.
    SessionsList {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The Sync socket the snapshot is bound to. With it the coordinator
        /// binds the returned ids to that socket and issues the snapshot token
        /// `domain_ready` presents (`coordinator.proto:117-125`); without it the
        /// call is v2's pre-barrier probe, which only classifies access and
        /// must never be published.
        sync_socket_id: Option<String>,
    },
    /// `WorkersList` — the worker registry and which of it is routable now.
    WorkersList {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `WorkspacesList` — the workspaces domain's snapshot.
    WorkspacesList {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `TasksList` — the tasks domain's snapshot, every state.
    TasksList {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `McpList` — the MCP relay registry's snapshot.
    McpList {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `PairList` — the pending tap-to-pair requests' snapshot.
    PairList {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `AuthRedeemBrowser` — spend a one-time pairing grant on this device's key.
    RedeemPairToken {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The token the operator pasted or the `#pair=` fragment carried.
        token: String,
        /// This device's public key, standard base64.
        ssh_pubkey_b64: String,
        /// What the device list will call this browser.
        label: String,
    },
    /// `FilesListDir` — one directory listing for the file viewer.
    FilesListDir {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The machine to read, by its registry fingerprint.
        worker_fp: String,
        /// The directory path, absolute on that machine.
        path: String,
    },
    /// `FilesMkdir` — create one directory. The wire has no recursive flag
    /// (`coordinator.proto:816`); the worker decides how missing parents fare.
    FilesMkdir {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The machine to write to, by its registry fingerprint.
        worker_fp: String,
        /// The directory path to create.
        path: String,
    },
    /// `SessionsSearchGlobal` — one page of a fleet-wide content search.
    ///
    /// Unary and cursor-paged, NOT a stream. The answer carries the page and
    /// the `next_cursor` that asks for the one after it
    /// (`protocol/proto/roost/v1/coordinator.proto:960`), and the REQUEST
    /// already carries a `cursor` — a continuation is this same method again,
    /// which is only meaningful because the first answer came back inline.
    /// One page in flight at a time, because the coordinator's cursor is a
    /// single continuation and two pages sharing it would interleave.
    SessionsSearchGlobal {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The identity the coordinator cancels this search under. **The
        /// CLIENT mints it and sends it on the request** — the start does not
        /// return it, so a client that treated the answer as the handle would
        /// have nothing to cancel with.
        search_id: String,
        /// The text to find.
        query: String,
        /// Whether a match respects case.
        case_sensitive: bool,
        /// Where to resume, or `None` for the first page.
        cursor: Option<String>,
        /// The coordinator's own caps, never this client's.
        max_sessions: u32,
        max_rows_per_session: u32,
        max_matches: u32,
    },
    /// `SessionsCancelGlobalSearch` — stop a search already running.
    SessionsCancelGlobalSearch {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The search to cancel: the `search_id` this client minted and sent on
        /// the request (`coordinator.proto:409`).
        search_id: String,
    },
    /// `SessionsKill` — end a session's PTY once its close's undo window has
    /// run out (`closeSession.ts` `killAfterUndo`).
    SessionsKill {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The session to kill.
        session_id: String,
        /// Skip the graceful hangup; sent only after a graceful kill was refused.
        force: bool,
    },
    /// `AgentChatList` — all conversations and host connectivity.
    AgentChatList { call_id: u64 },
    /// `AgentChatDelete` — delete an agent conversation after its undo window.
    AgentChatDelete {
        call_id: u64,
        conversation_id: String,
    },
}

/// The bootstrap snapshot call one domain's hydrator makes for
/// `Effect::HydrateDomain`.
///
/// `None` for the audit domain: it is lazy in v2 (`registerLazySyncDomain`,
/// `sync-domain-hydration.ts:41-64`), hydrated by the audit surface that
/// subscribed to it and never by bootstrap. The terminal domain is the only one
/// bound to the socket, which is what makes its answer carry the snapshot token.
pub fn hydration_call(domain: SyncDomain, call_id: u64, sync_socket_id: &str) -> Option<RpcCall> {
    match domain {
        SyncDomain::Terminal => Some(RpcCall::SessionsList {
            call_id,
            sync_socket_id: Some(sync_socket_id.to_string()),
        }),
        SyncDomain::Workers => Some(RpcCall::WorkersList { call_id }),
        SyncDomain::Workspaces => Some(RpcCall::WorkspacesList { call_id }),
        SyncDomain::Tasks => Some(RpcCall::TasksList { call_id }),
        SyncDomain::Mcp => Some(RpcCall::McpList { call_id }),
        SyncDomain::Pair => Some(RpcCall::PairList { call_id }),
        SyncDomain::Agent => Some(RpcCall::AgentChatList { call_id }),
        SyncDomain::Audit => None,
    }
}
