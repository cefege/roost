//! A coordinator that answers `SessionsList` to a worker principal alone, and
//! records the credential it was given.
//!
//! `roost-coord` is not a dependency of `roost-worker` and never will be, so the
//! rule this fixture stands in for is stated rather than imported: the route
//! table makes `SessionsList` `DeviceOrOwnWorkerRecovery`
//! (`crates/roost-coord/src/rpc/method_route_rows.rs:79`), which means a caller
//! with no credential is refused rather than answered anonymously.
//!
//! `Router::route` rather than a `CoordinatorService` impl, for the reason
//! `enrollment_support` gives: the generated trait has a hundred methods and
//! this fixture answers one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use connectrpc::{ConnectError, RequestContext, Response, Router, Server, handler::handler_fn};
use roost_proto::{
    COORDINATOR_SERVICE_SERVICE_NAME, Session, SessionRecoveryMetadata, SessionsListRequest,
    SessionsListResponse,
};

use roost_worker::runtime::credential::{CredentialError, CredentialSource};

/// The one open row this fixture publishes, and the recovery row that pairs
/// with it. A session id is a uuid at the protocol boundary, so a test value
/// that is not one is refused before the pairing is ever examined.
pub const SESSION_UNDER_TEST: &str = "00000000-0000-4000-8000-000000000001";

/// The bearer the last accepted call carried, or `None` when none did.
#[derive(Default)]
struct Coordinator {
    seen: Mutex<Option<String>>,
}

impl Coordinator {
    fn list(
        &self,
        ctx: &RequestContext,
        _request: SessionsListRequest,
    ) -> Result<SessionsListResponse, ConnectError> {
        let Some(bearer) = bearer_of(ctx) else {
            // The refusal is the coordinator's, not the fixture's convenience:
            // an anonymous caller here would be a caller the real coordinator
            // turns away, and a fixture that answered it would make the read look
            // authenticated when it is not.
            return Err(ConnectError::unauthenticated(
                "sessions.list requires a worker credential",
            ));
        };
        *self.seen.lock().expect("held") = Some(bearer);
        // One open row AND the recovery row that pairs with it. The read
        // refuses a set that does not pair (`assert_exact_recovery_metadata`),
        // so a fixture answering with a bare session fails admission for a
        // reason no assertion here is about.
        Ok(SessionsListResponse {
            sessions: vec![Session {
                id: SESSION_UNDER_TEST.to_owned(),
                ..Default::default()
            }],
            recovery_metadata: vec![SessionRecoveryMetadata {
                session_id: SESSION_UNDER_TEST.to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        })
    }
}

/// A coordinator on a loopback port, and a client pointed at it.
pub struct Fixture {
    pub address: SocketAddr,
    coordinator: Arc<Coordinator>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    serving: Option<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    /// Serve a coordinator holding no open rows of its own.
    pub async fn start() -> Self {
        let coordinator = Arc::new(Coordinator::default());
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

    /// The bearer the coordinator read off the wire, or `None` if it read none.
    pub fn seen_bearer(&self) -> Option<String> {
        self.coordinator.seen.lock().expect("held").clone()
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

/// The one method this coordinator answers.
fn routes(coordinator: Arc<Coordinator>) -> Router {
    Router::new().route(
        COORDINATOR_SERVICE_SERVICE_NAME,
        "SessionsList",
        handler_fn(move |ctx, request: SessionsListRequest| {
            let coordinator = Arc::clone(&coordinator);
            async move { Response::ok(coordinator.list(&ctx, request)?) }
        }),
    )
}

/// The `authorization` header's bearer, or `None` when there is not one.
fn bearer_of(ctx: &RequestContext) -> Option<String> {
    let value = ctx.headers().get("authorization")?.to_str().ok()?;
    let bearer = value.strip_prefix("Bearer ")?;
    Some(bearer.to_owned())
}

/// A credential that mints a fixed string, and one that cannot become a header.
///
/// The second is not decoration: `with_header` would drop a value it cannot
/// spell and the call would arrive with no header at all, which is the failure
/// `try_with_header` exists to prevent. The worker uses the `try_` form, so this
/// one is a refusal before the wire is touched.
pub struct Credential {
    token: Option<String>,
}

impl Credential {
    /// A credential that mints `token`.
    pub fn minting(token: &str) -> Self {
        Self {
            token: Some(token.to_owned()),
        }
    }

    /// A credential whose string no header can carry.
    pub fn unspellable() -> Self {
        Self { token: None }
    }
}

impl CredentialSource for Credential {
    fn mint(&self) -> Result<String, CredentialError> {
        self.token
            .clone()
            .ok_or_else(|| CredentialError::Unspellable {
                reason: "this fixture's credential cannot be spelled as a header".to_owned(),
            })
    }
}
