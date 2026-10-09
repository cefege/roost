//! Response bytes → `RpcResult`, reading exactly what v2 reads from each answer.
//!
//! Called by the host's pump on a successful unary answer. The body must decode
//! as the method's response message or the whole answer is an `RpcCodecError`;
//! inside a message that did decode, a ROW that fails its brand or JSON check is
//! dropped with a warning, as v2's hydrators drop it
//! (`sync-bootstrap-hydration.ts:64-74,160-172`), so one bad database row cannot
//! wedge a domain in a retry loop.

use std::collections::BTreeMap;

use roost_proto::{
    AuthCoordIdentityResponse, AuthRedeemBrowserResponse, FilesListDirResponse, FilesMkdirResponse,
    McpListResponse, PairListResponse, SessionsCancelGlobalSearchResponse, SessionsListResponse,
    SessionsSearchGlobalResponse, TasksListResponse, WorkersListResponse, WorkspacesListResponse,
};
use roost_protocol::ProtocolResult;
use roost_protocol::wire::{SessionMap, session_from_proto};

use super::search_page::search_page_from_proto;
use super::wire_rows::{
    mcp_relay_from_proto, pair_request_from_proto, task_from_proto, worker_from_proto,
    workspace_from_proto,
};
use super::{RpcCodecError, decode_message as decode};
use crate::client::rpc::calls::sessions::KillSession;
use crate::client::rpc::methods::connect_method;
use crate::client::rpc::unary::UnaryMethod;
use crate::effect::{RpcCall, RpcResult};
use crate::store::browse_entries::BrowseEntry;

/// Decode the answer to `call`.
pub fn decode_rpc_response(call: &RpcCall, body: &[u8]) -> Result<RpcResult, RpcCodecError> {
    let method = connect_method(call);
    Ok(match call {
        RpcCall::CoordIdentity { call_id } => {
            let response: AuthCoordIdentityResponse = decode(method, body)?;
            RpcResult::CoordIdentity {
                call_id: *call_id,
                git_sha: response.git_sha,
                public_url: response.public_url,
                terminal_peer_stun_urls: response
                    .terminal_peer_enabled
                    .then_some(response.terminal_peer_stun_urls),
                builtin_agent_enabled: response.builtin_agent_enabled,
            }
        }
        RpcCall::AgentChatList { call_id } => {
            let response: roost_proto::AgentChatListResponse = decode(method, body)?;
            let conversations = response
                .conversations
                .iter()
                .map(roost_protocol::wire::agent_chat::conversation_from_proto)
                .collect();
            RpcResult::AgentChatList {
                call_id: *call_id,
                conversations,
                host_connected: response.host_connected,
            }
        }
        RpcCall::SessionsList { call_id, .. } => {
            let response: SessionsListResponse = decode(method, body)?;
            let mut sessions = SessionMap::new();
            for row in &response.sessions {
                if let Some(session) = keep_row(method, &row.id, session_from_proto(row)) {
                    sessions.insert(session.id.clone(), session);
                }
            }
            RpcResult::SessionsList {
                call_id: *call_id,
                sessions,
                // v2 tests `!response.syncSnapshotToken`: empty is missing.
                terminal_snapshot_token: response
                    .sync_snapshot_token
                    .filter(|token| !token.is_empty()),
            }
        }
        RpcCall::WorkersList { call_id } => {
            let response: WorkersListResponse = decode(method, body)?;
            RpcResult::WorkersList {
                call_id: *call_id,
                workers: keyed_rows(method, &response.workers, |row| &row.fp, worker_from_proto),
                routable_fps: response.routable_fps.into_iter().collect(),
            }
        }
        RpcCall::WorkspacesList { call_id } => {
            let response: WorkspacesListResponse = decode(method, body)?;
            RpcResult::WorkspacesList {
                call_id: *call_id,
                workspaces: keyed_rows(
                    method,
                    &response.workspaces,
                    |row| &row.id,
                    workspace_from_proto,
                ),
            }
        }
        RpcCall::TasksList { call_id } => {
            let response: TasksListResponse = decode(method, body)?;
            RpcResult::TasksList {
                call_id: *call_id,
                tasks: keyed_rows(method, &response.tasks, |row| &row.id, task_from_proto),
            }
        }
        RpcCall::McpList { call_id } => {
            let response: McpListResponse = decode(method, body)?;
            RpcResult::McpList {
                call_id: *call_id,
                relays: keyed_rows(
                    method,
                    &response.relays,
                    |row| &row.id,
                    mcp_relay_from_proto,
                ),
            }
        }
        RpcCall::PairList { call_id } => {
            let response: PairListResponse = decode(method, body)?;
            RpcResult::PairList {
                call_id: *call_id,
                requests: keyed_rows(
                    method,
                    &response.requests,
                    |row| &row.ephemeral_id,
                    pair_request_from_proto,
                ),
            }
        }
        RpcCall::RedeemPairToken { call_id, .. } => {
            let _: AuthRedeemBrowserResponse = decode(method, body)?;
            RpcResult::PairTokenRedeemed { call_id: *call_id }
        }
        RpcCall::FilesListDir {
            call_id,
            worker_fp,
            path,
        } => {
            let response: FilesListDirResponse = decode(method, body)?;
            let mut entries = Vec::with_capacity(response.entries.len());
            for entry in &response.entries {
                if let Some(mtime_ms) = keep_row(method, &entry.name, signed_mtime(entry.mtime_ms))
                {
                    entries.push(BrowseEntry {
                        name: entry.name.clone(),
                        is_dir: entry.is_dir,
                        mtime_ms,
                    });
                }
            }
            RpcResult::DirectoryListed {
                call_id: *call_id,
                worker_fp: worker_fp.clone(),
                path: path.clone(),
                resolved_path: or_requested(response.resolved_path, path),
                entries,
            }
        }
        RpcCall::FilesMkdir {
            call_id,
            worker_fp,
            path,
        } => {
            let response: FilesMkdirResponse = decode(method, body)?;
            RpcResult::DirectoryCreated {
                call_id: *call_id,
                worker_fp: worker_fp.clone(),
                resolved_path: or_requested(response.resolved_path, path),
            }
        }
        RpcCall::SessionsSearchGlobal {
            call_id, search_id, ..
        } => {
            let response: SessionsSearchGlobalResponse = decode(method, body)?;
            RpcResult::SearchPage {
                call_id: *call_id,
                search_id: search_id.clone(),
                page: search_page_from_proto(&response),
            }
        }
        RpcCall::SessionsCancelGlobalSearch { call_id, search_id } => {
            let _: SessionsCancelGlobalSearchResponse = decode(method, body)?;
            RpcResult::GlobalSearchCancelled {
                call_id: *call_id,
                search_id: search_id.clone(),
            }
        }
        RpcCall::SessionsKill {
            call_id,
            session_id,
            force,
        } => RpcResult::SessionKillAnswered {
            call_id: *call_id,
            session_id: session_id.clone(),
            force: *force,
            accepted: KillSession::decode_response(body)?,
        },
    })
}

/// Convert every row, keyed by its id; a later duplicate replaces an earlier
/// one, as v2's record assignment does.
fn keyed_rows<Pb, Row>(
    method: &'static str,
    rows: &[Pb],
    key: impl Fn(&Pb) -> &String,
    convert: impl Fn(&Pb) -> ProtocolResult<Row>,
) -> BTreeMap<String, Row> {
    rows.iter()
        .filter_map(|row| {
            keep_row(method, key(row), convert(row)).map(|kept| (key(row).clone(), kept))
        })
        .collect()
}

fn keep_row<Row>(
    method: &'static str,
    row_key: &str,
    converted: ProtocolResult<Row>,
) -> Option<Row> {
    converted
        .map_err(|error| {
            tracing::warn!(
                target: "sync",
                method,
                row = row_key,
                %error,
                "list row failed to decode; dropped from the snapshot"
            );
        })
        .ok()
}

fn signed_mtime(mtime_ms: u64) -> ProtocolResult<i64> {
    i64::try_from(mtime_ms).map_err(|_| {
        roost_protocol::ProtocolError::new("files.entry.mtime_ms", "does not fit a signed field")
    })
}

fn or_requested(resolved_path: String, requested: &str) -> String {
    if resolved_path.is_empty() {
        requested.to_string()
    } else {
        resolved_path
    }
}
