//! A bootstrap redemption driven through the PRODUCTION auth gate, over HTTP.
//!
//! Ports the enrollment half of `apps/coord/src/auth/handlers-auth-bootstrap.ts`
//! (`authRedeemWorker`/`authRedeemBrowser` ignore `_ctx`: the token in the body
//! is the credential), which `rpc/auth_gate.rs` admits with NO caller stamped.
//! A handler-level test with a fabricated caller cannot see an arm that reads one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod auth_device_support;
mod ws_credential_support;

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};

use auth_device_support::{
    ACCOUNT_DEVICE_COUNT, AUTHORIZED_KEY_COUNT, Scratch, UNSPENT_GRANT_COUNT, WORKER_COUNT,
    mint_host_grant,
};
use roost_coord::auth::bootstrap_tokens::BootstrapTokenKind;
use roost_coord::coord_core::CoordCore;
use roost_coord::http::listener::{CONNECT_PATH_PREFIX, ListenerState, build_router};
use roost_coord::http::spa::SpaMount;
use roost_coord::rpc::service::CoordinatorServiceImpl;
use ws_credential_support::{mint_coordinator_jwt, now_secs};

/// The key a redeeming machine or browser holds, as a seed.
const ENROLLING_SEED: [u8; 32] = [7; 32];

/// The production router, gate and all, on an ephemeral loopback port.
struct GatedListener {
    address: SocketAddr,
    server: tokio::task::JoinHandle<()>,
}

impl GatedListener {
    /// Serve `build_router` over a core on `scratch`'s database, bound to `:0`.
    async fn start(scratch: &Scratch) -> Self {
        let core: CoordCore = scratch
            .core_with("listener", |input| {
                input.bind = Some("127.0.0.1:0".to_owned());
            })
            .await;
        let config = core
            .services
            .boot
            .config
            .as_deref()
            .cloned()
            .expect("a booted config");
        let services = Arc::clone(&core.services);
        let service = Arc::new(CoordinatorServiceImpl::new(
            core,
            config.clone(),
            "epoch-1".to_owned(),
            0,
            "sha".to_owned(),
        ));
        let state = Arc::new(ListenerState {
            service,
            services,
            bind: config.bind.clone(),
            web_public_url: config.web_public_url.clone(),
            trust_proxy: config.trust_proxy,
            spa: Arc::new(SpaMount::from_dist_path(None)),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a bound listener");
        let address = listener.local_addr().expect("the bound address");
        let mounted = build_router(state);
        mounted.publish_bound_port(address.port());
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                mounted
                    .router
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
        Self { address, server }
    }

    /// One Connect unary JSON call; `bearer` is the only credential sent.
    async fn call(&self, method: &str, bearer: Option<&str>, body: Value) -> (u16, Value) {
        let url = format!("http://{}{CONNECT_PATH_PREFIX}{method}", self.address);
        let mut request = reqwest::Client::new()
            .post(url)
            .header("content-type", "application/json")
            .header("connect-protocol-version", "1")
            .body(body.to_string());
        if let Some(token) = bearer {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = request.send().await.expect("a response");
        let status = response.status().as_u16();
        let text = response.text().await.expect("a body");
        let parsed = serde_json::from_str(&text)
            .unwrap_or_else(|_| panic!("{method} answered a non-JSON body: {text}"));
        (status, parsed)
    }
}

impl Drop for GatedListener {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// The enrolling key's fingerprint, its `ssh_pubkey_b64`, and a live JWT it signs.
fn enrolling_key() -> (String, String, String) {
    let now = now_secs();
    let (fingerprint, public_key, jwt) = mint_coordinator_jwt(ENROLLING_SEED, now, now + 60);
    (fingerprint, STANDARD_NO_PAD.encode(public_key), jwt)
}

/// v2 `bootstrap-token-handlers.test.ts` "worker redemption permits only an exact
/// same-key lost-response retry" (first redemption + `workers` row scoped to the
/// tenant's dashboard), driven over the wire as `coord-e2e.test.ts` drives Connect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_redeems_with_no_credential_and_its_key_then_registers() {
    let scratch = Scratch::new("redeem-worker-gate").await;
    let listener = GatedListener::start(&scratch).await;
    let (fingerprint, pubkey_b64, jwt) = enrolling_key();
    let token = mint_host_grant(scratch.database(), BootstrapTokenKind::Worker, "grant").await;
    let register = json!({ "label": "laptop", "os": "linux" });

    // Before redemption the key is nobody: the credentialed call it will make
    // afterwards is refused by the gate.
    let (status, refused) = listener
        .call("WorkersRegister", Some(&jwt), register.clone())
        .await;
    assert_eq!(status, 401, "an unenrolled key authenticated: {refused}");

    let (status, redeemed) = listener
        .call(
            "AuthRedeemWorker",
            None,
            json!({ "token": token, "sshPubkeyB64": pubkey_b64, "label": "laptop", "os": "linux" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "a credential-less redemption was refused: {redeemed}"
    );
    assert_eq!(redeemed["fingerprint"], json!(fingerprint));
    assert_eq!(redeemed["label"], json!("laptop"));
    assert_eq!(scratch.scalar(UNSPENT_GRANT_COUNT).await, 0);
    assert_eq!(scratch.scalar(WORKER_COUNT).await, 1);
    assert_eq!(
        scratch
            .scalar(&format!(
                "SELECT count(*) FROM workers WHERE fp = '{fingerprint}' AND dashboard_id = '{}'",
                scratch.dashboard_id
            ))
            .await,
        1,
        "the redeemed worker is not scoped to the tenant's dashboard"
    );

    let (status, registered) = listener.call("WorkersRegister", Some(&jwt), register).await;
    assert_eq!(
        status, 200,
        "the redeemed key cannot register: {registered}"
    );
    assert_eq!(registered["worker"]["fp"], json!(fingerprint));
}

/// v2 `bootstrap-token-handlers.test.ts` "browser redemption permits retry but
/// not a competitor or a new grant for that principal" (first redemption answers
/// the empty response), driven over the wire as `coord-e2e.test.ts` drives Connect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_redeems_with_no_credential_and_its_key_then_lists_devices() {
    let scratch = Scratch::new("redeem-browser-gate").await;
    let listener = GatedListener::start(&scratch).await;
    let (fingerprint, pubkey_b64, jwt) = enrolling_key();
    let token = mint_host_grant(scratch.database(), BootstrapTokenKind::Browser, "grant").await;

    let (status, refused) = listener.call("DevicesList", Some(&jwt), json!({})).await;
    assert_eq!(status, 401, "an unenrolled key authenticated: {refused}");

    let (status, redeemed) = listener
        .call(
            "AuthRedeemBrowser",
            None,
            json!({ "token": token, "sshPubkeyB64": pubkey_b64, "label": "phone" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "a credential-less redemption was refused: {redeemed}"
    );
    assert_eq!(redeemed, json!({}));
    assert_eq!(scratch.scalar(UNSPENT_GRANT_COUNT).await, 0);
    assert_eq!(scratch.scalar(AUTHORIZED_KEY_COUNT).await, 1);
    assert_eq!(
        scratch
            .scalar(&format!(
                "SELECT count(*) FROM account_devices WHERE fingerprint = '{fingerprint}' \
                 AND account_id = '{}'",
                scratch.account_id
            ))
            .await,
        1,
        "the redeemed browser is not a device of the tenant's account"
    );
    assert_eq!(scratch.scalar(ACCOUNT_DEVICE_COUNT).await, 1);

    let (status, listed) = listener.call("DevicesList", Some(&jwt), json!({})).await;
    assert_eq!(
        status, 200,
        "the redeemed key cannot list devices: {listed}"
    );
    let devices = listed["devices"].as_array().expect("a device list");
    assert_eq!(devices.len(), 1, "one enrolled browser: {listed}");
    assert_eq!(devices[0]["fingerprint"], json!(fingerprint));
    assert_eq!(devices[0]["label"], json!("phone"));
    assert_eq!(devices[0]["isSelf"], json!(true));
}
