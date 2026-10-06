//! The `SessionsGrantLocalTerminal` boundary: authenticated tab binding,
//! durable session-route checks, digest-only worker install and ACK fencing,
//! and the capability fields a browser chooses its direct carrier from.
//! Ports `apps/coord/tests/local-terminal-grant.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod terminal_direct_core_support;
mod terminal_direct_support;

use connectrpc::ErrorCode;
use roost_coord::terminal_direct::grant_state::LOCAL_TERMINAL_GRANT_TTL_MS;
use roost_protocol::versioning::{
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
};
use sha2::{Digest, Sha256};
use terminal_direct_core_support::{
    DirectCore, LOCAL_EPOCH, LOCAL_TAB, LOCAL_WORKER, REMOTE_EPOCH, REMOTE_WORKER, STUN_URL,
};
use terminal_direct_support::{TestWorker, install_worker, yield_many};

const BOTH: [&str; 2] = [
    CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1,
];

struct Fixture {
    direct: DirectCore,
    local: TestWorker,
    remote: TestWorker,
}

impl Fixture {
    async fn new(label: &str, peer_enabled: bool) -> Self {
        let direct = DirectCore::new(label, peer_enabled).await;
        let workers = &direct.core.services.workers;
        let local = install_worker(workers, LOCAL_WORKER, Some(LOCAL_EPOCH), &BOTH);
        let remote = install_worker(workers, REMOTE_WORKER, Some(REMOTE_EPOCH), &[]);
        Self {
            direct,
            local,
            remote,
        }
    }

    async fn refused(
        &self,
        tab_header: Option<&str>,
        tab_id: &str,
        sessions: Vec<String>,
    ) -> ErrorCode {
        let caller = self.direct.caller_on(tab_header);
        let handler = self
            .direct
            .request_grant(caller, LOCAL_WORKER, tab_id, sessions);
        handler
            .await
            .unwrap()
            .expect_err("the grant is refused")
            .code
    }

    fn installs(&self) -> usize {
        self.local.grants().len() + self.remote.grants().len()
    }
}

// v2 "requires the request tab to equal the authenticated interceptor tab".
#[tokio::test]
async fn requires_the_request_tab_to_equal_the_authenticated_interceptor_tab() {
    let fixture = Fixture::new("tab", true).await;
    let session = fixture.direct.insert_session(LOCAL_WORKER, "open").await;

    let mismatch = fixture
        .refused(Some(LOCAL_TAB), "other-tab", vec![session.clone()])
        .await;
    let missing = fixture.refused(None, LOCAL_TAB, vec![session]).await;
    assert_eq!(mismatch, ErrorCode::PermissionDenied);
    assert_eq!(missing, ErrorCode::InvalidArgument);
    assert_eq!(fixture.installs(), 0);
}

// v2 "caps request strings and unique session membership before worker install".
#[tokio::test]
async fn caps_request_strings_and_unique_session_membership_before_worker_install() {
    let fixture = Fixture::new("caps", true).await;
    let session = fixture.direct.insert_session(LOCAL_WORKER, "open").await;
    let too_long = "x".repeat(129);

    let long_tab = fixture
        .refused(Some(LOCAL_TAB), &too_long, vec![session.clone()])
        .await;
    let duplicate = fixture
        .refused(Some(LOCAL_TAB), LOCAL_TAB, vec![session.clone(), session])
        .await;
    let too_many = (0..257).map(|index| format!("session-{index}")).collect();
    let too_many = fixture.refused(Some(LOCAL_TAB), LOCAL_TAB, too_many).await;
    assert_eq!(long_tab, ErrorCode::InvalidArgument);
    assert_eq!(duplicate, ErrorCode::InvalidArgument);
    assert_eq!(too_many, ErrorCode::InvalidArgument);
    assert_eq!(fixture.installs(), 0);
}

// v2 "refuses closed or cross-worker sessions without installing a grant".
#[tokio::test]
async fn refuses_closed_or_cross_worker_sessions_without_installing_a_grant() {
    let fixture = Fixture::new("routes", true).await;
    let remote_session = fixture.direct.insert_session(REMOTE_WORKER, "open").await;
    let closed_session = fixture.direct.insert_session(LOCAL_WORKER, "closed").await;

    let wrong_route = fixture
        .refused(Some(LOCAL_TAB), LOCAL_TAB, vec![remote_session])
        .await;
    let closed = fixture
        .refused(Some(LOCAL_TAB), LOCAL_TAB, vec![closed_session])
        .await;
    assert_eq!(wrong_route, ErrorCode::PermissionDenied);
    assert_eq!(closed, ErrorCode::NotFound);
    assert_eq!(fixture.installs(), 0);
}

// v2 "returns a secret only after digest-only worker install and exposes supported capabilities".
#[tokio::test]
async fn returns_a_secret_only_after_a_digest_only_install_and_exposes_capabilities() {
    let fixture = Fixture::new("secret", true).await;
    let session = fixture.direct.insert_session(LOCAL_WORKER, "open").await;
    let response = fixture
        .direct
        .grant_with_ack(&fixture.local, vec![session.clone()])
        .await;
    let frame = fixture.local.grants()[0].clone();

    assert_eq!(response.secret.len(), 64);
    assert!(response.secret.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(response.ttl_ms, LOCAL_TERMINAL_GRANT_TTL_MS);
    assert_eq!(
        frame.secret_sha256,
        hex::encode(Sha256::digest(response.secret.as_bytes()))
    );
    assert!(!format!("{frame:?}").contains(&response.secret));
    assert_eq!(frame.worker_epoch, LOCAL_EPOCH);
    assert_eq!(response.worker_epoch, LOCAL_EPOCH);
    assert!(response.peer_supported);
    assert_eq!(response.stun_urls, vec![STUN_URL]);
    assert!(response.input_route_supported);
    let leases = fixture.direct.core.services.terminal_direct.grants().list();
    let listed = format!("{leases:?}");
    assert!(!listed.contains(&response.secret));
    assert!(!listed.contains(&frame.secret_sha256));
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].grant_id, response.grant_id);
    assert_eq!(leases[0].worker_fp, LOCAL_WORKER);
    assert_eq!(leases[0].worker_epoch.as_deref(), Some(LOCAL_EPOCH));
    assert_eq!(leases[0].session_ids, vec![session]);
}

// v2 "keeps input-route support independent when peer setup is disabled".
#[tokio::test]
async fn keeps_input_route_support_independent_when_peer_setup_is_disabled() {
    let fixture = Fixture::new("peer-off", false).await;
    let session = fixture.direct.insert_session(LOCAL_WORKER, "open").await;
    let response = fixture
        .direct
        .grant_with_ack(&fixture.local, vec![session])
        .await;

    assert!(!response.peer_supported);
    assert!(response.stun_urls.is_empty());
    assert!(response.input_route_supported);
}

// v2 "keeps rolling workers on unchanged base response fields".
#[tokio::test]
async fn keeps_rolling_workers_on_unchanged_base_response_fields() {
    let fixture = Fixture::new("rolling", true).await;
    let rolling = install_worker(
        &fixture.direct.core.services.workers,
        LOCAL_WORKER,
        None,
        &BOTH,
    );
    let session = fixture.direct.insert_session(LOCAL_WORKER, "open").await;
    let response = fixture.direct.grant_with_ack(&rolling, vec![session]).await;

    assert_eq!(response.worker_epoch, "");
    assert!(!response.peer_supported);
    assert!(response.stun_urls.is_empty());
    assert!(!response.input_route_supported);
    assert_eq!(rolling.grants()[0].worker_epoch, "");
}

// v2 "rechecks durable authorization after the worker ACK before returning a secret".
#[tokio::test]
async fn rechecks_durable_authorization_after_the_worker_ack_before_returning_a_secret() {
    let fixture = Fixture::new("recheck", true).await;
    let session = fixture.direct.insert_session(LOCAL_WORKER, "open").await;
    let caller = fixture.direct.caller.clone();
    let handler =
        fixture
            .direct
            .request_grant(caller, LOCAL_WORKER, LOCAL_TAB, vec![session.clone()]);
    let frame = fixture.direct.next_install(&fixture.local, 0).await;
    sqlx::query("UPDATE sessions SET status = 'closed' WHERE id = $1")
        .bind(&session)
        .execute(fixture.direct.core.services.db.pool())
        .await
        .unwrap();
    fixture.direct.ack(&fixture.local, &frame);
    let error = handler
        .await
        .unwrap()
        .expect_err("the closed session is refused");

    assert_eq!(error.code, ErrorCode::NotFound);
    yield_many().await;
    assert!(
        fixture
            .direct
            .core
            .services
            .terminal_direct
            .grants()
            .list()
            .is_empty()
    );
}
