//! A real coordinator, in this process, for the two RPCs an enrollment makes.
//!
//! `roost-coord` is not a dependency of `roost-worker` and never will be, so
//! "against an in-process coordinator" is served here rather than by adding the
//! edge: a Connect server bound to loopback, speaking the generated
//! `CoordinatorService` stubs, holding a token table and a worker table with
//! the same rules `auth::rpc_bootstrap` applies — a token is spent once, a
//! fingerprint is bound to the public key that redeemed it, and a registration
//! is only accepted from the key the token named.
//!
//! What calls it: `enrollment_round_trip.rs`. Depends on `roost_proto` for the
//! generated messages and `roost_protocol` for the fingerprint — nothing here
//! is specific to one test binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use connectrpc::client::{ClientConfig, HttpClient};
use connectrpc::{ConnectError, RequestContext, Response, Router, Server, handler::handler_fn};
use roost_proto::{
    AuthRedeemWorkerRequest, AuthRedeemWorkerResponse, COORDINATOR_SERVICE_SERVICE_NAME,
    CoordinatorServiceClient, WorkersRegisterRequest, WorkersRegisterResponse,
};
use sha2::Digest as _;

/// The one row a redemption writes, and the one a registration must match.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Redeemed {
    fingerprint: String,
    public_key: [u8; 32],
    label: String,
}

/// A coordinator small enough to read, large enough to refuse.
#[derive(Default)]
pub struct Coordinator {
    /// Tokens this coordinator issued, and the fingerprint each was spent by.
    spent: Mutex<BTreeMap<String, String>>,
    /// The `authorized_keys` rows the redemptions wrote.
    authorized: Mutex<BTreeMap<String, Redeemed>>,
    /// The `workers` rows the registrations wrote, by fingerprint.
    registered: Mutex<BTreeMap<String, String>>,
    /// Every credential the coordinator was asked to verify, in order.
    credentials: Mutex<Vec<String>>,
}

impl Coordinator {
    fn redeem(
        &self,
        request: AuthRedeemWorkerRequest,
    ) -> Result<AuthRedeemWorkerResponse, ConnectError> {
        let public_key: [u8; 32] = decode_public_key(&request.ssh_pubkey_b64)?;
        let fingerprint = fingerprint_of(&public_key);
        let mut spent = self.spent.lock().expect("held");
        match spent.get(&request.token) {
            // v2's rule, and the one that makes a redeploy's re-offered token
            // cost one round trip rather than a refusal: a token already spent
            // by THIS key is a success.
            Some(bound) if *bound == fingerprint => {}
            Some(_) | None => {
                return Err(ConnectError::invalid_argument("the bootstrap token is not spendable"));
            }
        }
        spent.insert(request.token.clone(), fingerprint.clone());
        self.authorized
            .lock()
            .expect("held")
            .insert(
                fingerprint.clone(),
                Redeemed {
                    fingerprint: fingerprint.clone(),
                    public_key,
                    label: request.label.clone(),
                },
            );
        Ok(AuthRedeemWorkerResponse {
            fingerprint,
            label: request.label,
            ..Default::default()
        })
    }

    fn register(
        &self,
        ctx: &RequestContext,
        request: WorkersRegisterRequest,
    ) -> Result<WorkersRegisterResponse, ConnectError> {
        let credential = bearer_of(ctx).ok_or_else(|| {
            ConnectError::unauthenticated("WorkersRegister arrived without a worker credential")
        })?;
        self.credentials
            .lock()
            .expect("held")
            .push(credential.clone());
        // The rule the whole enrollment turns on: the key that signed this
        // credential must be the one the redemption bound this token to. The
        // token's `kid` IS the fingerprint, and `authorized` is keyed by it, so
        // a credential naming a key nobody redeemed finds nothing here.
        let kid = kid_of(&credential)
            .ok_or_else(|| ConnectError::unauthenticated("the worker credential names no key"))?;
        let bound = self
            .authorized
            .lock()
            .expect("held")
            .get(&kid)
            .cloned()
            .ok_or_else(|| {
                ConnectError::unauthenticated("no redemption bound this credential's key")
            })?;
        let label = request.label.clone().unwrap_or_else(|| bound.label.clone());
        self.registered
            .lock()
            .expect("held")
            .insert(bound.fingerprint.clone(), label.clone());
        Ok(WorkersRegisterResponse {
            worker: roost_proto::buffa::MessageField::some(roost_proto::Worker {
                fp: bound.fingerprint.clone(),
                label,
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    /// The fingerprint the token was spent by, or `None` while it is unspent.
    pub fn holder_of(&self, token: &str) -> Option<String> {
        self.spent.lock().expect("held").get(token).cloned()
    }

    /// The label the worker is registered under, or `None` before it registered.
    pub fn label_of(&self, fingerprint: &str) -> Option<String> {
        self.registered.lock().expect("held").get(fingerprint).cloned()
    }

    /// Whether this coordinator issued the registration for `fingerprint`.
    pub fn is_registered(&self, fingerprint: &str) -> bool {
        self.registered.lock().expect("held").contains_key(fingerprint)
    }

    /// Every credential presented, in arrival order.
    pub fn credentials(&self) -> Vec<String> {
        self.credentials.lock().expect("held").clone()
    }
}

/// A coordinator on a loopback port, and a client pointed at it.
pub struct Fixture {
    pub address: SocketAddr,
    pub coordinator: Arc<Coordinator>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    serving: Option<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    /// Serve a coordinator holding `tokens` as unspent worker tokens.
    pub async fn start(tokens: &[&str]) -> Self {
        let coordinator = Arc::new(Coordinator::default());
        for token in tokens {
            coordinator
                .spent
                .lock()
                .expect("held")
                .insert((*token).to_string(), String::new());
        }
        let router = routes(Arc::clone(&coordinator));
        let bound = Server::bind("127.0.0.1:0")
            .await
            .expect("loopback is bindable");
        let address = bound.local_addr().expect("a bound listener has an address");
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(async move {
            let _ = bound
                .serve_with_graceful_shutdown(router, async {
                    let _ = stopped.await;
                })
                .await;
        });
        Self {
            address,
            coordinator,
            shutdown: Some(shutdown),
            serving: Some(serving),
        }
    }

    /// A Connect client for the enrollment calls, over this fixture's address.
    pub fn client(&self) -> CoordinatorServiceClient<HttpClient> {
        let uri = format!("http://{}", self.address).parse().expect("a loopback URL");
        CoordinatorServiceClient::new(HttpClient::plaintext(), ClientConfig::new(uri))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(serving) = self.serving.take() {
            serving.abort();
        }
    }
}

/// The two methods enrollment calls, and nothing else from the service.
///
/// `Router::route` rather than a `CoordinatorService` impl: the generated trait
/// has a hundred methods and this fixture answers two, and a hand-written impl
/// of the rest would be a hundred answers nobody reads.
fn routes(coordinator: Arc<Coordinator>) -> Router {
    let redeem = Arc::clone(&coordinator);
    let register = Arc::clone(&coordinator);
    Router::new()
        .route(
            COORDINATOR_SERVICE_SERVICE_NAME,
            "AuthRedeemWorker",
            handler_fn(move |_ctx, request: AuthRedeemWorkerRequest| {
                let coordinator = Arc::clone(&redeem);
                async move {
                    Response::ok(coordinator.redeem(request)?)
                }
            }),
        )
        .route(
            COORDINATOR_SERVICE_SERVICE_NAME,
            "WorkersRegister",
            handler_fn(move |ctx, request: WorkersRegisterRequest| {
                let coordinator = Arc::clone(&register);
                async move { Response::ok(coordinator.register(&ctx, request)?) }
            }),
        )
}

/// The `authorization` header's bearer, or `None` when there is not one.
fn bearer_of(ctx: &RequestContext) -> Option<String> {
    let value = ctx
        .headers()
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    Some(value.to_owned())
}

/// The fingerprint a credential names, from the JOSE header's `kid`.
///
/// The worker's minting puts the fingerprint in `kid` and in `sub`
/// (`host::jwt`), and the coordinator's rule is that they agree with the
/// `authorized_keys` row — so reading `kid` is reading the identity the
/// credential is really asserting, not a field the fixture chose.
fn kid_of(credential: &str) -> Option<String> {
    let header = credential.split('.').next()?;
    let json = base64url_decode(header)?;
    let parsed: serde_json::Value = serde_json::from_slice(&json).ok()?;
    parsed
        .get("kid")
        .and_then(|kid| kid.as_str())
        .map(str::to_owned)
}

fn decode_public_key(encoded: &str) -> Result<[u8; 32], ConnectError> {
    let raw = base64_decode(encoded)
        .ok_or_else(|| ConnectError::invalid_argument("the public key is not base64"))?;
    raw.try_into()
        .map_err(|_| ConnectError::invalid_argument("an ed25519 public key is 32 bytes"))
}

/// The same rendering the coordinator and the worker both use: SHA-256 of the
/// raw public key, lowercase hex, no prefix.
fn fingerprint_of(public_key: &[u8; 32]) -> String {
    let digest: [u8; 32] = sha2::Sha256::digest(public_key).into();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(char::from_digit(u32::from(byte >> 4), 16).expect("a nibble is a hex digit"));
        hex.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("a nibble is a hex digit"));
    }
    hex
}

fn base64_decode(encoded: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(encoded).ok()
}

fn base64url_decode(encoded: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok()
}
