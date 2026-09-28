//! Every call's answer decodes into the `RpcResult` the store folds, reading the
//! fields v2 reads; a body that is not the method's response is an error for
//! every call, never an empty answer.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::rpc::{RpcCodecError, connect_method, decode_rpc_response};
use roost_client_core::search::global::GlobalSearchPartialReason;
use roost_client_core::store::browse_entries::BrowseEntry;
use roost_client_core::{RpcCall, RpcResult};
use roost_proto::buffa::Message;
use roost_proto::{
    AuthCoordIdentityResponse, FilesListDirEntry, FilesListDirResponse, FilesMkdirResponse,
    GlobalSearchPartialReason as PbReason, Session as PbSession, SessionsKillResponse,
    SessionsListResponse, SessionsSearchGlobalMatch, SessionsSearchGlobalPartial,
    SessionsSearchGlobalResponse,
};

const SESSION: &str = "00000000-0000-4000-8000-00000000000a";

fn session_row(id: &str) -> PbSession {
    PbSession {
        id: id.to_owned(),
        worker_fp: "a".repeat(64),
        channel: 1,
        kind: "shell".to_owned(),
        cwd: "/x".to_owned(),
        status: "open".to_owned(),
        created_at: 1,
        ..Default::default()
    }
}

fn sessions_call() -> RpcCall {
    RpcCall::SessionsList {
        call_id: 4,
        sync_socket_id: Some("socket-1".to_owned()),
    }
}

#[test]
fn the_identity_answer_is_the_build_and_url_v2_keeps() {
    let bytes = AuthCoordIdentityResponse {
        git_sha: "abc123".to_owned(),
        public_url: "https://roost.example".to_owned(),
        instance_id: "instance-1".to_owned(),
        ..Default::default()
    }
    .encode_to_vec();
    assert_eq!(
        decode_rpc_response(&RpcCall::CoordIdentity { call_id: 3 }, &bytes).unwrap(),
        RpcResult::CoordIdentity {
            call_id: 3,
            git_sha: "abc123".to_owned(),
            public_url: "https://roost.example".to_owned(),
        }
    );
}

#[test]
fn the_sessions_snapshot_carries_its_rows_and_the_one_time_token() {
    let bytes = SessionsListResponse {
        sessions: vec![session_row(SESSION)],
        sync_snapshot_token: Some("snapshot-token-1".to_owned()),
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::SessionsList {
        call_id,
        sessions,
        terminal_snapshot_token,
    } = decode_rpc_response(&sessions_call(), &bytes).unwrap()
    else {
        panic!("a SessionsList answer must decode as SessionsList");
    };
    assert_eq!(call_id, 4);
    assert_eq!(terminal_snapshot_token.as_deref(), Some("snapshot-token-1"));
    assert_eq!(sessions.len(), 1);
    let (id, session) = sessions.iter().next().unwrap();
    assert_eq!(id.as_str(), SESSION);
    assert_eq!(session.cwd, "/x");
}

#[test]
fn an_empty_snapshot_token_is_a_missing_one() {
    // v2 tests `!response.syncSnapshotToken`: "" and absent both mean the
    // terminal domain cannot be made ready from this snapshot.
    for token in [None, Some(String::new())] {
        let bytes = SessionsListResponse {
            sync_snapshot_token: token,
            ..Default::default()
        }
        .encode_to_vec();
        let RpcResult::SessionsList {
            terminal_snapshot_token,
            ..
        } = decode_rpc_response(&sessions_call(), &bytes).unwrap()
        else {
            panic!("a SessionsList answer must decode as SessionsList");
        };
        assert_eq!(terminal_snapshot_token, None);
    }
}

#[test]
fn a_session_row_that_fails_its_brand_is_dropped_and_the_rest_kept() {
    // v2 sync-bootstrap-hydration.ts:64-74 drops the row and publishes the rest.
    let bytes = SessionsListResponse {
        sessions: vec![session_row("not-a-uuid"), session_row(SESSION)],
        sync_snapshot_token: Some("t".to_owned()),
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::SessionsList { sessions, .. } =
        decode_rpc_response(&sessions_call(), &bytes).unwrap()
    else {
        panic!("a SessionsList answer must decode as SessionsList");
    };
    assert_eq!(
        sessions.keys().map(|id| id.as_str()).collect::<Vec<_>>(),
        [SESSION]
    );
}

#[test]
fn a_listing_keeps_the_machines_rows_and_falls_back_to_the_asked_path() {
    let call = RpcCall::FilesListDir {
        call_id: 5,
        worker_fp: "fp-1".to_owned(),
        path: "~/src".to_owned(),
    };
    let answer = |resolved: &str| {
        FilesListDirResponse {
            entries: vec![
                FilesListDirEntry {
                    name: "lib".to_owned(),
                    is_dir: true,
                    mtime_ms: 7,
                    ..Default::default()
                },
                FilesListDirEntry {
                    name: "main.rs".to_owned(),
                    is_dir: false,
                    mtime_ms: 9,
                    ..Default::default()
                },
            ],
            resolved_path: resolved.to_owned(),
            ..Default::default()
        }
        .encode_to_vec()
    };
    let expected = |resolved: &str| RpcResult::DirectoryListed {
        call_id: 5,
        worker_fp: "fp-1".to_owned(),
        path: "~/src".to_owned(),
        resolved_path: resolved.to_owned(),
        entries: vec![BrowseEntry::dir("lib", 7), BrowseEntry::file("main.rs", 9)],
    };
    assert_eq!(
        decode_rpc_response(&call, &answer("/home/me/src")).unwrap(),
        expected("/home/me/src")
    );
    // browseDirectoryListing.ts:58 — `response.resolvedPath || dir`.
    assert_eq!(
        decode_rpc_response(&call, &answer("")).unwrap(),
        expected("~/src")
    );
}

#[test]
fn a_created_directory_reports_where_it_landed() {
    let call = RpcCall::FilesMkdir {
        call_id: 6,
        worker_fp: "fp-1".to_owned(),
        path: "/repo/new".to_owned(),
    };
    let answer = |resolved: &str| {
        FilesMkdirResponse {
            resolved_path: resolved.to_owned(),
            ..Default::default()
        }
        .encode_to_vec()
    };
    let created = |resolved: &str| RpcResult::DirectoryCreated {
        call_id: 6,
        worker_fp: "fp-1".to_owned(),
        resolved_path: resolved.to_owned(),
    };
    assert_eq!(
        decode_rpc_response(&call, &answer("/real/new")).unwrap(),
        created("/real/new")
    );
    assert_eq!(
        decode_rpc_response(&call, &answer("")).unwrap(),
        created("/repo/new")
    );
}

#[test]
fn a_search_page_maps_rows_reasons_and_the_cursor() {
    let call = RpcCall::SessionsSearchGlobal {
        call_id: 7,
        search_id: "search-1".to_owned(),
        query: "x".to_owned(),
        case_sensitive: false,
        cursor: None,
        max_sessions: 1,
        max_rows_per_session: 1,
        max_matches: 1,
    };
    let bytes = SessionsSearchGlobalResponse {
        matches: vec![
            SessionsSearchGlobalMatch {
                session_id: SESSION.to_owned(),
                row: 12,
                col: 3,
                len: 1,
                preview: "a x b".to_owned(),
                grid_epoch: "g-1".to_owned(),
                ..Default::default()
            },
            SessionsSearchGlobalMatch {
                session_id: "not-a-uuid".to_owned(),
                ..Default::default()
            },
        ],
        partials: vec![
            SessionsSearchGlobalPartial {
                session_id: SESSION.to_owned(),
                reason: PbReason::GLOBAL_SEARCH_PARTIAL_REASON_EPOCH_CHANGED.into(),
                ..Default::default()
            },
            SessionsSearchGlobalPartial {
                session_id: SESSION.to_owned(),
                reason: 99.into(),
                ..Default::default()
            },
        ],
        next_cursor: Some("cursor-2".to_owned()),
        searched_sessions: 1,
        eligible_sessions: 2,
        truncated: true,
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::SearchPage {
        call_id,
        search_id,
        page,
    } = decode_rpc_response(&call, &bytes).unwrap()
    else {
        panic!("a search answer must decode as SearchPage");
    };
    assert_eq!((call_id, search_id.as_str()), (7, "search-1"));
    assert_eq!(
        page.matches.len(),
        1,
        "a row naming no valid session is dropped"
    );
    let hit = &page.matches[0];
    assert_eq!((hit.row, hit.col, hit.len), (12, 3, 1));
    assert_eq!(
        (hit.preview.as_str(), hit.grid_epoch.as_str()),
        ("a x b", "g-1")
    );
    let reasons: Vec<_> = page.partials.iter().map(|partial| partial.reason).collect();
    assert_eq!(
        reasons,
        [
            GlobalSearchPartialReason::EpochChanged,
            GlobalSearchPartialReason::Unspecified
        ]
    );
    assert_eq!(page.next_cursor.as_deref(), Some("cursor-2"));
    assert_eq!((page.searched_sessions, page.eligible_sessions), (1, 2));
    assert!(page.truncated);
}

#[test]
fn the_acknowledgement_only_calls_answer_with_their_identity() {
    assert_eq!(
        decode_rpc_response(
            &RpcCall::RedeemPairToken {
                call_id: 8,
                token: "t".to_owned(),
                ssh_pubkey_b64: "k".to_owned(),
                label: "l".to_owned(),
            },
            &[],
        )
        .unwrap(),
        RpcResult::PairTokenRedeemed { call_id: 8 }
    );
    assert_eq!(
        decode_rpc_response(
            &RpcCall::SessionsCancelGlobalSearch {
                call_id: 9,
                search_id: "search-1".to_owned(),
            },
            &[],
        )
        .unwrap(),
        RpcResult::GlobalSearchCancelled {
            call_id: 9,
            search_id: "search-1".to_owned(),
        }
    );
    let kill = RpcCall::SessionsKill {
        call_id: 10,
        session_id: SESSION.to_owned(),
        force: true,
    };
    let refused = SessionsKillResponse {
        accepted: false,
        ..Default::default()
    };
    assert_eq!(
        decode_rpc_response(&kill, &refused.encode_to_vec()).unwrap(),
        RpcResult::SessionKillAnswered {
            call_id: 10,
            session_id: SESSION.to_owned(),
            force: true,
            accepted: false,
        }
    );
}

#[test]
fn a_body_that_is_not_the_response_is_an_error_for_every_call() {
    // Field 1, length-delimited, claiming five bytes that never arrive.
    let truncated = [0x0a, 0x05, 0x01];
    for call in every_call() {
        let method = connect_method(&call);
        match decode_rpc_response(&call, &truncated) {
            Err(RpcCodecError::MalformedResponse { method: named, .. }) => {
                assert_eq!(named, method);
            }
            other => panic!("{method}: a truncated body decoded as {other:?}"),
        }
    }
}

fn every_call() -> Vec<RpcCall> {
    vec![
        RpcCall::CoordIdentity { call_id: 1 },
        sessions_call(),
        RpcCall::WorkersList { call_id: 1 },
        RpcCall::WorkspacesList { call_id: 1 },
        RpcCall::TasksList { call_id: 1 },
        RpcCall::McpList { call_id: 1 },
        RpcCall::PairList { call_id: 1 },
        RpcCall::RedeemPairToken {
            call_id: 1,
            token: String::new(),
            ssh_pubkey_b64: String::new(),
            label: String::new(),
        },
        RpcCall::FilesListDir {
            call_id: 1,
            worker_fp: String::new(),
            path: String::new(),
        },
        RpcCall::FilesMkdir {
            call_id: 1,
            worker_fp: String::new(),
            path: String::new(),
        },
        RpcCall::SessionsSearchGlobal {
            call_id: 1,
            search_id: String::new(),
            query: String::new(),
            case_sensitive: false,
            cursor: None,
            max_sessions: 0,
            max_rows_per_session: 0,
            max_matches: 0,
        },
        RpcCall::SessionsCancelGlobalSearch {
            call_id: 1,
            search_id: String::new(),
        },
        RpcCall::SessionsKill {
            call_id: 1,
            session_id: String::new(),
            force: false,
        },
    ]
}
