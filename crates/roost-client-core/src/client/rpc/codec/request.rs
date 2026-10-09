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
    }
}
