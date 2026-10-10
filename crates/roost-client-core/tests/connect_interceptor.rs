//! The two rules v2's Connect interceptor carried, a client that drops either
//! one only discovering it once a user is paired.
//!
//! The signing rule: a browser whose device key is temporarily unusable must
//! still be able to reach the pairing gate, so a signing failure dispatches the
//! request UNAUTHENTICATED rather than dropping it
//! (`apps/web/src/client/rpc/connect.ts:104-111`). A test that only asserted
//! "the transport received something" passes against an implementation that sent
//! the credential, so every case here pins the bearer AND the tab id together.
//!
//! The tab rule: `x-roost-tab-id` is on every request whether or not the body
//! needs it, because the coordinator's tab fence reads the header and refuses a
//! request that omits it differently from one that sends it empty.

use std::cell::RefCell;

use roost_client_core::RpcCall;
use roost_client_core::client::rpc::{
    AuthFailureCause, AuthFailureKind, ConnectClient, ConnectDispatcher, ConnectRequest,
    Credential, classify_auth_failure, connect_method, requires_device_auth, rpc_path,
};

const TAB: &str = "tab-7f3a";

/// The transport, reduced to the one thing a test can assert about it: what it
/// was handed. A real host implements this over `fetch`; nothing else about a
/// dispatch is observable from here, which is the point — the rules under test
/// are the ones that decide WHAT is handed over.
#[derive(Default)]
struct Recorder {
    sent: RefCell<Vec<ConnectRequest>>,
}

impl ConnectDispatcher for Recorder {
    fn dispatch(&self, request: ConnectRequest) {
        self.sent.borrow_mut().push(request);
    }
}

/// Every call the state machine can ask for. The set is closed and this
/// fixture is deliberately exhaustive: a call added to `RpcCall` and not here is
/// a call no test has ever put on the wire.
fn calls() -> [RpcCall; 13] {
    [
        RpcCall::CoordIdentity { call_id: 1 },
        RpcCall::SessionsList {
            call_id: 2,
            sync_socket_id: Some("socket-1".to_owned()),
        },
        RpcCall::WorkersList { call_id: 3 },
        RpcCall::RedeemPairToken {
            call_id: 4,
            token: "pair-token".to_owned(),
            ssh_pubkey_b64: "cHVia2V5".to_owned(),
            label: "Browser".to_owned(),
        },
        RpcCall::FilesListDir {
            call_id: 5,
            worker_fp: "worker-fp-01".to_owned(),
            path: "/repo/src".to_owned(),
        },
        RpcCall::FilesMkdir {
            call_id: 6,
            worker_fp: "worker-fp-01".to_owned(),
            path: "/repo/src/new".to_owned(),
        },
        RpcCall::SessionsSearchGlobal {
            call_id: 7,
            search_id: "search-1".to_owned(),
            query: "TODO".to_owned(),
            case_sensitive: false,
            cursor: None,
            max_sessions: 8,
            max_rows_per_session: 64,
            max_matches: 200,
        },
        RpcCall::SessionsCancelGlobalSearch {
            call_id: 8,
            search_id: "search-1".to_owned(),
        },
        RpcCall::WorkspacesList { call_id: 9 },
        RpcCall::TasksList { call_id: 10 },
        RpcCall::McpList { call_id: 11 },
        RpcCall::PairList { call_id: 12 },
        RpcCall::SessionsKill {
            call_id: 13,
            session_id: "s-1".to_owned(),
            force: false,
        },
    ]
}

fn call_id_of(call: &RpcCall) -> u64 {
    match call {
        RpcCall::CoordIdentity { call_id }
        | RpcCall::SessionsList { call_id, .. }
        | RpcCall::WorkersList { call_id }
        | RpcCall::WorkspacesList { call_id }
        | RpcCall::TasksList { call_id }
        | RpcCall::McpList { call_id }
        | RpcCall::PairList { call_id }
        | RpcCall::RedeemPairToken { call_id, .. }
        | RpcCall::FilesListDir { call_id, .. }
        | RpcCall::FilesMkdir { call_id, .. }
        | RpcCall::SessionsSearchGlobal { call_id, .. }
        | RpcCall::SessionsCancelGlobalSearch { call_id, .. }
        | RpcCall::SessionsKill { call_id, .. }
        | RpcCall::AgentChatDelete { call_id, .. }
        | RpcCall::AgentChatList { call_id } => *call_id,
    }
}

#[test]
fn a_signing_failure_still_dispatches_the_request_unauthenticated() {
    let client = ConnectClient::new(TAB);
    let recorder = Recorder::default();

    // `None` is the whole of what survives a failed signing attempt.
    let returned = client.dispatch(
        &recorder,
        &RpcCall::SessionsList {
            call_id: 9,
            sync_socket_id: None,
        },
        b"\x0a\x02hi".to_vec(),
        Credential::from_bearer(None),
    );

    let sent = recorder.sent.borrow();
    assert_eq!(sent.len(), 1, "the request must still reach the transport");
    let request = &sent[0];
    assert!(
        request.is_unauthenticated(),
        "a signing failure must leave the request with no bearer"
    );
    assert_eq!(request.bearer(), None);
    assert_eq!(
        request.tab_id(),
        TAB,
        "the unauthenticated request still carries the tab fence"
    );
    assert_eq!(request.method(), "SessionsList");
    assert_eq!(request.call_id(), 9);
    assert_eq!(request.body(), b"\x0a\x02hi");
    assert_eq!(&returned, request, "dispatch reports what it handed over");
}

#[test]
fn a_minted_credential_rides_the_request() {
    let client = ConnectClient::new(TAB);
    let recorder = Recorder::default();

    client.dispatch(
        &recorder,
        &RpcCall::WorkersList { call_id: 3 },
        Vec::new(),
        Credential::Minted("header.payload.signature".to_owned()),
    );

    let sent = recorder.sent.borrow();
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].is_unauthenticated());
    assert_eq!(sent[0].bearer(), Some("header.payload.signature"));
    assert_eq!(sent[0].tab_id(), TAB);
}

#[test]
fn every_connect_request_carries_the_tab_id() {
    let client = ConnectClient::new(TAB);

    // Every call the state machine can ask for, with a body that mentions no tab
    // at all, and with the credential both present and absent. The redemption is
    // the one that matters most: it is made BEFORE a device credential exists,
    // which is exactly the request a client that built its headers out of the
    // credential would drop the tab from.
    for credential in [
        Credential::Minted("jwt".to_owned()),
        Credential::from_bearer(None),
    ] {
        for call in calls() {
            let request = client.prepare(&call, Vec::new(), credential.clone());
            assert_eq!(
                request.tab_id(),
                TAB,
                "{} went out without the tab id",
                connect_method(&call)
            );
            assert_eq!(request.call_id(), call_id_of(&call));
        }
    }
}

#[test]
fn the_tab_id_is_the_clients_and_not_the_bodys() {
    let client = ConnectClient::new(TAB);
    // A body with bytes in it, and no tab in it anywhere.
    let request = client.prepare(
        &RpcCall::SessionsList {
            call_id: 2,
            sync_socket_id: None,
        },
        b"\x0a\x0bworker-fp-01".to_vec(),
        Credential::from_bearer(None),
    );
    assert_eq!(request.tab_id(), TAB);
    assert!(
        !String::from_utf8_lossy(request.body()).contains(TAB),
        "the tab id is a header, never something the body happens to carry"
    );
    // A second client over the same tab agrees, which is what makes one id
    // meaningful across the requests of a single tab.
    let other = ConnectClient::new(TAB).prepare(
        &RpcCall::CoordIdentity { call_id: 5 },
        Vec::new(),
        Credential::Minted("jwt".to_owned()),
    );
    assert_eq!(other.tab_id(), request.tab_id());
}

#[test]
fn each_call_goes_out_as_the_method_the_coordinator_declares() {
    let client = ConnectClient::new(TAB);
    for (call, method, path) in [
        (
            RpcCall::CoordIdentity { call_id: 1 },
            "AuthCoordIdentity",
            "/roost.v1.CoordinatorService/AuthCoordIdentity",
        ),
        (
            RpcCall::SessionsList {
                call_id: 2,
                sync_socket_id: None,
            },
            "SessionsList",
            "/roost.v1.CoordinatorService/SessionsList",
        ),
        (
            RpcCall::WorkersList { call_id: 3 },
            "WorkersList",
            "/roost.v1.CoordinatorService/WorkersList",
        ),
        (
            RpcCall::RedeemPairToken {
                call_id: 4,
                token: "t".to_owned(),
                ssh_pubkey_b64: "k".to_owned(),
                label: "l".to_owned(),
            },
            "AuthRedeemBrowser",
            "/roost.v1.CoordinatorService/AuthRedeemBrowser",
        ),
    ] {
        let request = client.prepare(&call, Vec::new(), Credential::Minted("jwt".to_owned()));
        assert_eq!(request.method(), method);
        assert_eq!(rpc_path(request.method()), path);
    }
}

#[test]
fn a_device_auth_refusal_is_the_device_and_a_proxy_refusal_is_not() {
    let device = AuthFailureCause::unauthenticated(Some("device".to_owned()));
    let proxy = AuthFailureCause::unauthenticated(Some("trusted_proxy".to_owned()));
    let no_layer = AuthFailureCause::unauthenticated(None);
    let other = AuthFailureCause::other();

    // All four conditions, each one load-bearing.
    assert_eq!(
        classify_auth_failure(&[other.clone(), device.clone()], "SessionsList"),
        AuthFailureKind::Device
    );
    assert_eq!(
        classify_auth_failure(std::slice::from_ref(&device), "WorkersList"),
        AuthFailureKind::Device
    );
    // Not the device layer.
    assert_eq!(
        classify_auth_failure(&[proxy], "SessionsList"),
        AuthFailureKind::Retryable
    );
    // Not unauthenticated at all.
    assert_eq!(
        classify_auth_failure(&[other], "SessionsList"),
        AuthFailureKind::Retryable
    );
    // No auth layer to read.
    assert_eq!(
        classify_auth_failure(&[no_layer], "SessionsList"),
        AuthFailureKind::Retryable
    );
    // A method that does not need the device cannot be a device rejection, even
    // when the refusal says it authenticated at the device layer: a wrapped call
    // carries someone else's chain, and showing the pairing page to a user whose
    // pairing is fine is the expensive mistake.
    assert_eq!(
        classify_auth_failure(std::slice::from_ref(&device), "AuthCoordIdentity"),
        AuthFailureKind::Retryable
    );
    assert_eq!(
        classify_auth_failure(&[device], "AuthRedeemBrowser"),
        AuthFailureKind::Retryable
    );
}

#[test]
fn the_cause_walk_reads_four_links_and_no_more() {
    let device = AuthFailureCause::unauthenticated(Some("device".to_owned()));
    let other = AuthFailureCause::other();

    // The last link the walk reads is still read...
    let at_the_limit = [other.clone(), other.clone(), other.clone(), device.clone()];
    assert_eq!(
        classify_auth_failure(&at_the_limit, "SessionsList"),
        AuthFailureKind::Device
    );
    // ...and the first one past it is not. A bound only checked from one side is
    // a bound that has not been checked.
    let past_the_limit = [other.clone(), other.clone(), other.clone(), other, device];
    assert_eq!(
        classify_auth_failure(&past_the_limit, "SessionsList"),
        AuthFailureKind::Retryable
    );
}

#[test]
fn only_the_methods_the_coordinator_demands_a_device_for() {
    // The session list and the worker registry answer nothing at all without the
    // device key, so an `Unauthenticated` on them is a device rejection.
    // Identity and the pairing redemption are reachable without one.
    assert!(requires_device_auth("SessionsList"));
    assert!(requires_device_auth("WorkersList"));
    assert!(requires_device_auth("PairApprovalStatus"));
    assert!(!requires_device_auth("AuthCoordIdentity"));
    assert!(!requires_device_auth("AuthRedeemBrowser"));
    assert!(!requires_device_auth("SessionsSpawn"));
}

#[test]
fn a_device_credential_is_never_built_here() {
    // The client takes a credential as a value and never mints one: signing is
    // async in every host and belongs to the auth slice, so the only thing this
    // crate can be answerable for is what happens when minting failed.
    let minted = Credential::from_bearer(Some("jwt".to_owned()));
    assert_eq!(minted.bearer(), Some("jwt"));
    assert!(!minted.is_unavailable());
    assert_eq!(minted.reason(), None);

    let unavailable = Credential::Unavailable {
        reason: "the key is not extractable".to_owned(),
    };
    assert_eq!(unavailable.bearer(), None);
    assert!(unavailable.is_unavailable());
    assert_eq!(unavailable.reason(), Some("the key is not extractable"));

    // The failure reason is for the log, and never reaches the wire.
    let request = ConnectClient::new(TAB).prepare(
        &RpcCall::SessionsList {
            call_id: 1,
            sync_socket_id: None,
        },
        Vec::new(),
        unavailable,
    );
    assert_eq!(request.bearer(), None);
    assert!(request.body().is_empty());
    assert!(
        !format!("{request:?}").contains("extractable"),
        "a signing failure's reason must not be carried by the request"
    );
}
