//! The Sync socket's fences at open: the reauth deadline (already passed, and
//! passing while live), the key-revocation check between upgrade and open, and
//! the upgrade query's negotiation reaching the socket's scope.
//!
//! Ports `apps/coord/tests/auth/ws-auth-deadline.test.ts` (the socket half) and
//! `apps/coord/tests/sync/sync-ws-keepalive-upgrade.test.ts` at the socket
//! boundary. The v2 commands a socket accepts once open are
//! `sync_ws_socket_lifecycle.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::net::SocketAddr;
use std::sync::Arc;

use roost_coord::http::upgrade::SyncQuery;
use roost_coord::services::CoordServices;
use roost_coord::sync_ws::upgrade_admission::{
    OriginPolicy, PrincipalKind, SyncScope, SyncUpgradeDecision, SyncUpgradeRequest,
    VerifiedSyncCaller, admit_sync_upgrade,
};

use sync_ws_socket_support::{EXPECT, SyncFixture, read_subscribed};
use ws_client_support::{close_code, dial};

/// Serve `serve_socket` directly on its own port, so a test can hand it the
/// reauth deadline and caller the production upgrade never passes.
async fn serve_direct(
    services: Arc<CoordServices>,
    caller: VerifiedSyncCaller,
    reauth_at_ms: Option<i64>,
) -> SocketAddr {
    let scope = SyncScope {
        owner_worker_fp: None,
        read_only: false,
        tab_id: None,
        viewer_key: None,
        flow_control: true,
        domain_generations: true,
        since_event_id: 0,
    };
    let app = axum::Router::new().route(
        "/direct",
        axum::routing::get(move |upgrade: axum::extract::ws::WebSocketUpgrade| {
            let (services, caller, scope) = (Arc::clone(&services), caller.clone(), scope.clone());
            async move {
                upgrade.on_upgrade(move |socket| {
                    roost_coord::sync_ws::socket::serve_socket(
                        socket,
                        caller,
                        scope,
                        reauth_at_ms,
                        services,
                    )
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    address
}

fn caller(key_generation: u64) -> VerifiedSyncCaller {
    VerifiedSyncCaller {
        fingerprint: "a".repeat(64),
        label: "browser".to_owned(),
        key_generation,
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

// v2 sync-ws-handler.ts:185-191 and ws-auth-deadline.test.ts: a deadline that
// has already passed closes 4003 before any frame, the barrier included.
#[tokio::test]
async fn an_expired_reauth_deadline_closes_4003_before_the_barrier() {
    let fixture = SyncFixture::start("reauth-expired").await;
    let address = serve_direct(Arc::clone(&fixture.services), caller(0), Some(now_ms() - 1)).await;
    let mut socket = dial(address, "/direct", &[]).await.socket();
    assert_eq!(close_code(&mut socket, EXPECT).await, Some(Some(4003)));
}

// v2 ws-auth-deadline.test.ts "closes at Access expiry with the reauth code":
// a live socket closes 4003 when its deadline passes.
#[tokio::test]
async fn a_live_socket_closes_4003_when_its_deadline_passes() {
    let fixture = SyncFixture::start("reauth-live").await;
    let address = serve_direct(
        Arc::clone(&fixture.services),
        caller(0),
        Some(now_ms() + 400),
    )
    .await;
    let mut socket = dial(address, "/direct", &[]).await.socket();
    read_subscribed(&mut socket).await;
    assert_eq!(close_code(&mut socket, EXPECT).await, Some(Some(4003)));
}

// v2 sync-ws-keepalive-upgrade.test.ts "revocation between accepted upgrade
// and open closes before feed registration".
#[tokio::test]
async fn a_key_revoked_after_verification_closes_4001_at_open() {
    let fixture = SyncFixture::start("revoked").await;
    let address = serve_direct(Arc::clone(&fixture.services), caller(1), None).await;
    let mut socket = dial(address, "/direct", &[]).await.socket();
    assert_eq!(close_code(&mut socket, EXPECT).await, Some(Some(4001)));
}

fn scope_for(query: &str) -> SyncScope {
    let uri: axum::http::Uri = format!("/ws/coord-sync?{query}").parse().unwrap();
    let query = SyncQuery::of_uri(&uri);
    let request = SyncUpgradeRequest {
        path: "/ws/coord-sync".to_owned(),
        origin: None,
        host: "127.0.0.1:4000".to_owned(),
        offered_protocols: vec!["roost-auth".to_owned(), "a.b.c".to_owned()],
        caller: Some(caller(0)),
        tab: query.tab,
        since: query.since,
        flow: query.flow,
        sync_v: query.sync_v,
    };
    let policy = OriginPolicy {
        public_url: None,
        web_public_url: None,
        cors_allowed_origins: Vec::new(),
        worker_local_ui_origin: "http://127.0.0.1:4101".to_owned(),
        loopback_bind: Some("127.0.0.1:4000".to_owned()),
        relaxed_csp: false,
    };
    match admit_sync_upgrade(&request, &policy, PrincipalKind::AccountDevice) {
        SyncUpgradeDecision::Admitted { scope, .. } => scope,
        SyncUpgradeDecision::Refused(refusal) => panic!("refused: {refusal}"),
    }
}

// v2 sync-ws-upgrade.ts:172-180 with URLSearchParams semantics: the query's
// decoded values reach the scope, the first of a repeated name wins, and
// only the exact `flow=1` (and `sync_v=2` on top of it) negotiates anything
// (sync-ws-keepalive-upgrade.test.ts "only exact flow=1 enables the window").
#[test]
fn the_upgrade_query_reaches_the_scope() {
    let scope = scope_for("flow=1&sync_v=2&tab=tab%201&since=42&tab=second");
    assert!(scope.flow_control && scope.domain_generations);
    assert_eq!(scope.tab_id.as_deref(), Some("tab 1"));
    assert_eq!(scope.viewer_key, Some(format!("{}:tab 1", "a".repeat(64))));
    assert_eq!(scope.since_event_id, 42);

    let scope = scope_for("flow=01&sync_v=2&tab=+");
    assert!(!scope.flow_control && !scope.domain_generations);
    assert_eq!(scope.tab_id, None, "a blank tab is no tab");
    assert!(!scope_for("flow=1&sync_v=3").domain_generations);
}
