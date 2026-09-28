//! Every `RpcCall` encodes to the request message its method declares, carrying
//! exactly the fields v2's call sites send — decoded back with the same
//! generated type the coordinator's Connect handler decodes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::RpcCall;
use roost_client_core::client::rpc::{connect_method, encode_rpc_request};
use roost_client_core::effect::hydration_call;
use roost_client_core::sync::link::SyncDomain;
use roost_proto::buffa::Message;
use roost_proto::{
    AuthRedeemBrowserRequest, FilesListDirRequest, FilesMkdirRequest,
    SessionsCancelGlobalSearchRequest, SessionsListRequest, SessionsSearchGlobalRequest,
    TasksListRequest,
};

fn body(call: &RpcCall) -> Vec<u8> {
    encode_rpc_request(call).expect("every call encodes")
}

#[test]
fn a_hydration_sessions_list_binds_the_snapshot_to_its_sync_socket() {
    // v2 sync-bootstrap-hydration.ts:56 — `sessionsList({ syncSocketId })`.
    let bound = SessionsListRequest::decode_from_slice(&body(&RpcCall::SessionsList {
        call_id: 1,
        sync_socket_id: Some("socket-7".to_owned()),
    }))
    .unwrap();
    assert_eq!(bound.sync_socket_id.as_deref(), Some("socket-7"));
    assert_eq!(bound.worker_fp, None);
    assert_eq!(
        bound.status, None,
        "the default 'open' filter is the coordinator's"
    );

    // The pre-barrier probe (sync-bootstrap.ts:204) names no socket, so the
    // coordinator mints no snapshot token for it.
    let probe = SessionsListRequest::decode_from_slice(&body(&RpcCall::SessionsList {
        call_id: 2,
        sync_socket_id: None,
    }))
    .unwrap();
    assert_eq!(probe.sync_socket_id, None);
}

#[test]
fn the_list_and_identity_calls_send_an_empty_request() {
    for call in [
        RpcCall::CoordIdentity { call_id: 1 },
        RpcCall::WorkersList { call_id: 2 },
        RpcCall::WorkspacesList { call_id: 3 },
        RpcCall::McpList { call_id: 4 },
        RpcCall::PairList { call_id: 5 },
    ] {
        assert!(
            body(&call).is_empty(),
            "{} sends fields v2 never sent",
            connect_method(&call)
        );
    }
    // The tasks snapshot is every task: no state filter.
    let tasks =
        TasksListRequest::decode_from_slice(&body(&RpcCall::TasksList { call_id: 6 })).unwrap();
    assert_eq!(tasks.state, None);
}

#[test]
fn a_pair_token_redemption_presents_the_token_key_and_label() {
    // v2 redeemPairToken.test.ts "a successful HTTPS redemption needs no
    // coordinator key assertion": the call carries exactly these three.
    let request = AuthRedeemBrowserRequest::decode_from_slice(&body(&RpcCall::RedeemPairToken {
        call_id: 1,
        token: "tok-success".to_owned(),
        ssh_pubkey_b64: "dGVzdC1wdWJsaWMta2V5".to_owned(),
        label: "Test browser".to_owned(),
    }))
    .unwrap();
    assert_eq!(request.token, "tok-success");
    assert_eq!(request.ssh_pubkey_b64, "dGVzdC1wdWJsaWMta2V5");
    assert_eq!(request.label, "Test browser");
}

#[test]
fn the_file_calls_name_the_machine_and_the_path() {
    let listing = FilesListDirRequest::decode_from_slice(&body(&RpcCall::FilesListDir {
        call_id: 1,
        worker_fp: "fp-1".to_owned(),
        path: "/repo/src".to_owned(),
    }))
    .unwrap();
    assert_eq!(
        (listing.worker_fp.as_str(), listing.path.as_str()),
        ("fp-1", "/repo/src")
    );

    let mkdir = FilesMkdirRequest::decode_from_slice(&body(&RpcCall::FilesMkdir {
        call_id: 2,
        worker_fp: "fp-2".to_owned(),
        path: "/repo/new".to_owned(),
    }))
    .unwrap();
    assert_eq!(
        (mkdir.worker_fp.as_str(), mkdir.path.as_str()),
        ("fp-2", "/repo/new")
    );
}

#[test]
fn a_global_search_page_carries_the_client_minted_id_and_the_caps() {
    let request =
        SessionsSearchGlobalRequest::decode_from_slice(&body(&RpcCall::SessionsSearchGlobal {
            call_id: 1,
            search_id: "search-1".to_owned(),
            query: "TODO".to_owned(),
            case_sensitive: true,
            cursor: Some("cursor-2".to_owned()),
            max_sessions: 8,
            max_rows_per_session: 64,
            max_matches: 200,
        }))
        .unwrap();
    assert_eq!(request.search_id, "search-1");
    assert_eq!(request.query, "TODO");
    assert!(request.case_sensitive);
    assert_eq!(request.cursor.as_deref(), Some("cursor-2"));
    assert_eq!(
        (
            request.max_sessions,
            request.max_rows_per_session,
            request.max_matches
        ),
        (8, 64, 200)
    );

    let cancel = SessionsCancelGlobalSearchRequest::decode_from_slice(&body(
        &RpcCall::SessionsCancelGlobalSearch {
            call_id: 2,
            search_id: "search-1".to_owned(),
        },
    ))
    .unwrap();
    assert_eq!(cancel.search_id, "search-1");
}

#[test]
fn each_domain_hydrates_through_v2s_list_call() {
    let method = |domain| hydration_call(domain, 1, "socket-1").map(|call| connect_method(&call));
    assert_eq!(method(SyncDomain::Terminal), Some("SessionsList"));
    assert_eq!(method(SyncDomain::Workers), Some("WorkersList"));
    assert_eq!(method(SyncDomain::Workspaces), Some("WorkspacesList"));
    assert_eq!(method(SyncDomain::Tasks), Some("TasksList"));
    assert_eq!(method(SyncDomain::Mcp), Some("McpList"));
    assert_eq!(method(SyncDomain::Pair), Some("PairList"));
    assert_eq!(
        method(SyncDomain::Audit),
        None,
        "audit is lazy, never a bootstrap hydrator"
    );
    // Only the terminal snapshot is bound to the socket.
    assert_eq!(
        hydration_call(SyncDomain::Terminal, 9, "socket-1"),
        Some(RpcCall::SessionsList {
            call_id: 9,
            sync_socket_id: Some("socket-1".to_owned()),
        })
    );
}
