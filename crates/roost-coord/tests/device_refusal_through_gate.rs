//! The auth gate's own refusal carries the marker a browser acts on.
//!
//! v2 refused inside the handler, so every handler reached
//! `requireAccountDevice` for a caller it could not resolve and stamped
//! `x-roost-auth-layer: device` (`auth-interceptor.ts:256-262`). The v3 gate
//! refuses before a handler runs, and the answer it wrote named the method and
//! nothing else — which reads to a browser as an opaque `Unauthenticated`.
//! Its bootstrap probe classifies on that header and on nothing else, so an
//! unpaired browser could not tell "re-pair" from "retry" and sat on the
//! checking screen for as long as the coordinator ran. A handler-level test
//! cannot see this: the marker is lost above the handler, so the gate is
//! driven here over HTTP.
//!
//! `protocol/spec/auth-and-pairing.md` ("Errors") is the contract these assert.
//! The Connect body cap sits in front of the gate, so it is driven here too.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod auth_device_support;
mod ws_credential_support;

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde_json::{Value, json};

use auth_device_support::{Scratch, mint_host_grant};
use roost_coord::auth::bootstrap_tokens::BootstrapTokenKind;
use roost_coord::coord_core::CoordCore;
use roost_coord::http::listener::{
    CONNECT_PATH_PREFIX, ListenerState, MAX_REQUEST_BODY_BYTES, build_router,
};
use roost_coord::http::spa::SpaMount;
use roost_coord::rpc::service::CoordinatorServiceImpl;
use ws_credential_support::{mint_coordinator_jwt, now_secs};

/// The key whose JWT the coordinator cannot resolve: it is signed correctly
/// and this process holds no public key under its `kid`, so verification
/// fails and no caller is stamped. This is the shape an unpaired browser is
/// in, not a missing header — a browser that has loaded a device key gets a
/// JWT from `AuthMintBootstrap` whether or not it has ever been paired.
const UNKNOWN_KEY_SEED: [u8; 32] = [29; 32];

/// The key a browser pairs with, as a seed.
const PAIRED_KEY_SEED: [u8; 32] = [31; 32];

/// The header, and the one value that tells a browser to re-pair.
const AUTH_LAYER: &str = "x-roost-auth-layer";

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
    /// The auth layer is read from the RESPONSE, because that is the only
    /// place it can be: a browser cannot act on a header the wire did not
    /// carry, however the refusal was built.
    async fn call(&self, method: &str, bearer: Option<&str>, body: Value) -> Answer {
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
        let auth_layer = response
            .headers()
            .get(AUTH_LAYER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let text = response.text().await.expect("a body");
        let parsed = serde_json::from_str(&text)
            .unwrap_or_else(|_| panic!("{method} answered a non-JSON body: {text}"));
        Answer {
            status,
            auth_layer,
            body: parsed,
        }
    }
}

impl Drop for GatedListener {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// What one call was answered: the status, the marker, and the Connect error.
struct Answer {
    status: u16,
    auth_layer: Option<String>,
    body: Value,
}

impl Answer {
    /// Assert the refusal, and that the marker names the `device` layer.
    fn assert_device_refusal(&self, method: &str) {
        assert_eq!(self.status, 401, "{method}: {}", self.body);
        assert_eq!(self.body["code"], "unauthenticated", "{method}");
        assert_eq!(
            self.auth_layer.as_deref(),
            Some("device"),
            "{method} refused a browser without the header it acts on: a client \
             reading this cannot tell a rejected device from a retryable failure"
        );
    }
}

/// A live JWT for a key this coordinator has never heard of.
fn unresolvable_token() -> String {
    let now = now_secs();
    mint_coordinator_jwt(UNKNOWN_KEY_SEED, now, now + 60).2
}

/// An unpaired browser's bootstrap probe is `SessionsList`, and this is the
/// only thing that lets its page leave the checking screen
/// (`roost-client-core/src/handle_sync/hydration.rs:249`). Without the marker
/// the answer classifies as retryable, the gate never closes, and the pairing
/// panel never renders — `smoke/terminal/pair-gate.spec.ts` and
/// `smoke/terminal/tv-dpad.spec.ts` both time out on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unresolvable_credential_on_a_device_method_is_answered_with_the_device_marker() {
    let scratch = Scratch::new("gate-device-marker").await;
    let listener = GatedListener::start(&scratch).await;
    let token = unresolvable_token();

    let answer = listener.call("SessionsList", Some(&token), json!({})).await;
    answer.assert_device_refusal("SessionsList");

    // The same call with NO header at all is the same refusal: a browser that
    // could not even mint a JWT is in this branch, and it is the branch that
    // must not answer silently.
    let answer = listener.call("WorkersList", None, json!({})).await;
    answer.assert_device_refusal("WorkersList");
}

/// The marker's other half: a requirement only a machine satisfies must NOT be
/// answered with the device layer, because "present a device key" is a
/// credential that worker does not hold and cannot obtain. Stamping it there
/// would send a machine's client looking for a browser to pair.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_only_requirement_carries_no_device_marker() {
    let scratch = Scratch::new("gate-worker-marker").await;
    let listener = GatedListener::start(&scratch).await;

    let answer = listener
        .call("WorkersHeartbeat", Some(&unresolvable_token()), json!({}))
        .await;
    assert_eq!(answer.status, 401, "{}", answer.body);
    assert_eq!(answer.body["code"], "unauthenticated", "{}", answer.body);
    assert_eq!(
        answer.auth_layer, None,
        "the marker names the credential that would have worked, and a \
         worker requirement is not one a browser holds"
    );
}

/// The same method, the same gate, and a credential the coordinator CAN
/// resolve. This is the other half of the pairing gate: the marker may say
/// "re-pair" only to a browser whose key does not resolve, or every paired
/// browser is sent to a ceremony it has already completed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paired_browsers_probe_is_answered_and_names_no_auth_layer() {
    let scratch = Scratch::new("gate-paired").await;
    let listener = GatedListener::start(&scratch).await;
    let now = now_secs();
    let (_, public_key, jwt) = mint_coordinator_jwt(PAIRED_KEY_SEED, now, now + 60);
    let grant = mint_host_grant(scratch.database(), BootstrapTokenKind::Browser, "grant").await;

    let answer = listener
        .call(
            "AuthRedeemBrowser",
            None,
            json!({
                "token": grant,
                "sshPubkeyB64": STANDARD_NO_PAD.encode(public_key),
                "label": "laptop",
            }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "a credential-less redemption was refused: {}",
        answer.body
    );

    let answer = listener.call("SessionsList", Some(&jwt), json!({})).await;
    assert_eq!(
        answer.status, 200,
        "a paired browser cannot list its sessions: {}",
        answer.body
    );
    assert_eq!(answer.auth_layer, None, "an answer is not a refusal");
}

/// An `AttachFileChunk` carrying `data_bytes`, with no credential at all.
fn relay_chunk_body(data_bytes: usize) -> Value {
    json!({
        "uploadId": "upload-1",
        "sessionId": "session-1",
        "filename": "chunk.bin",
        "data": STANDARD_NO_PAD.encode(vec![7u8; data_bytes]),
        "last": true,
        "seq": 0,
    })
}

/// A full 4 MiB relay chunk (`apps/web/src/lib/attachments.ts:19`) plus its
/// envelope must reach the gate rather than be refused for its size; v2 caps
/// the body at 16 MiB (`bun-coordinator-listeners.ts:48,313`). connectrpc's
/// own default is 4 MiB, which refuses every full chunk before any handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_relay_chunk_reaches_the_gate_and_an_oversized_body_does_not() {
    let scratch = Scratch::new("gate-body-cap").await;
    let listener = GatedListener::start(&scratch).await;

    let answer = listener
        .call("AttachFileChunk", None, relay_chunk_body(4 * 1024 * 1024))
        .await;
    assert_eq!(answer.status, 401, "{}", answer.body);
    assert_eq!(answer.body["code"], "unauthenticated");

    let answer = listener
        .call(
            "AttachFileChunk",
            None,
            relay_chunk_body(MAX_REQUEST_BODY_BYTES),
        )
        .await;
    assert_eq!(answer.body["code"], "resource_exhausted", "{}", answer.body);
}
