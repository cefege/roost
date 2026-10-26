//! `RpcCall` → the method's request message bytes.
//!
//! Called by the host's pump before `ConnectClient::dispatch`. The field
//! mapping is the one v2's call sites send (`sync-bootstrap-hydration.ts:56`,
//! `redeemPairToken.ts:36-40`, `browseDirectoryListing.ts:53`,
//! `browseNewFolder.ts:72-75`); a list request v2 sends empty goes out empty.

use roost_proto::{
    AuthCoordIdentityRequest, AuthRedeemBrowserRequest, FilesListDirRequest, FilesMkdirRequest,
    McpListRequest, PairListRequest, SessionsCancelGlobalSearchRequest, SessionsListRequest,
    SessionsSearchGlobalRequest, TasksListRequest, WorkersListRequest, WorkspacesListRequest,
};

use super::{RpcCodecError, encode_message as encode};
use crate::client::client_state::ClientStateEntry;
use crate::client::rpc::calls::sessions::KillSession;
use crate::client::rpc::methods::connect_method;
use crate::client::rpc::unary::UnaryMethod;
use crate::effect::RpcCall;

/// Encode the request body for one call.
pub fn encode_rpc_request(call: &RpcCall) -> Result<Vec<u8>, RpcCodecError> {
    let method = connect_method(call);
    match call {
        RpcCall::AgentChatList { .. } => {
            encode(method, &roost_proto::AgentChatListRequest::default())
        }
        RpcCall::CoordIdentity { .. } => encode(method, &AuthCoordIdentityRequest::default()),
        RpcCall::SessionsList { sync_socket_id, .. } => encode(
            method,
            &SessionsListRequest {
                sync_socket_id: sync_socket_id.clone(),
                ..Default::default()
            },
        ),
        RpcCall::WorkersList { .. } => encode(method, &WorkersListRequest::default()),
        RpcCall::WorkspacesList { .. } => encode(method, &WorkspacesListRequest::default()),
        // No state filter: the tasks domain snapshot is every task.
        RpcCall::TasksList { .. } => encode(method, &TasksListRequest::default()),
        RpcCall::McpList { .. } => encode(method, &McpListRequest::default()),
        RpcCall::PairList { .. } => encode(method, &PairListRequest::default()),
        RpcCall::RedeemPairToken {
            token,
            ssh_pubkey_b64,
            label,
            ..
        } => encode(
            method,
            &AuthRedeemBrowserRequest {
                token: token.clone(),
                ssh_pubkey_b64: ssh_pubkey_b64.clone(),
                label: label.clone(),
                ..Default::default()
            },
        ),
        RpcCall::FilesListDir {
            worker_fp, path, ..
        } => encode(
            method,
            &FilesListDirRequest {
                worker_fp: worker_fp.clone(),
                path: path.clone(),
                ..Default::default()
            },
        ),
        RpcCall::FilesMkdir {
            worker_fp, path, ..
        } => encode(
            method,
            &FilesMkdirRequest {
                worker_fp: worker_fp.clone(),
                path: path.clone(),
                ..Default::default()
            },
        ),
        RpcCall::SessionsSearchGlobal {
            search_id,
            query,
            case_sensitive,
            cursor,
            max_sessions,
            max_rows_per_session,
            max_matches,
            ..
        } => encode(
            method,
            &SessionsSearchGlobalRequest {
                query: query.clone(),
                case_sensitive: *case_sensitive,
                search_id: search_id.clone(),
                cursor: cursor.clone(),
                max_sessions: *max_sessions,
                max_rows_per_session: *max_rows_per_session,
                max_matches: *max_matches,
                ..Default::default()
            },
        ),
        RpcCall::SessionsCancelGlobalSearch { search_id, .. } => encode(
            method,
            &SessionsCancelGlobalSearchRequest {
                search_id: search_id.clone(),
                ..Default::default()
            },
        ),
        RpcCall::SessionsKill {
            session_id, force, ..
        } => KillSession {
            session_id: session_id.clone(),
            force: *force,
        }
        .encode_request(),
        RpcCall::AgentChatDelete {
            conversation_id, ..
        } => {
            use super::super::calls::agent_chat::DeleteAgentChat;
            DeleteAgentChat {
                conversation_id: conversation_id.clone(),
            }
            .encode_request()
        }
        RpcCall::ClientStateList { .. } => {
            encode(method, &roost_proto::ClientStateListRequest::default())
        }
        RpcCall::ClientStateSet { entries, .. } => encode(
            method,
            &roost_proto::ClientStateSetRequest {
                entries: entries.iter().map(client_state_entry_to_proto).collect(),
                __buffa_unknown_fields: Default::default(),
            },
        ),
    }
}

/// The synced entry off the core's type and onto the wire.
fn client_state_entry_to_proto(entry: &ClientStateEntry) -> roost_proto::ClientStateEntry {
    roost_proto::ClientStateEntry {
        key: entry.key.clone(),
        value: entry.value.clone(),
        updated_ms: entry.updated_ms,
        __buffa_unknown_fields: Default::default(),
    }
}
