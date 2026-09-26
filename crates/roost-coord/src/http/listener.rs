//! The coordinator's HTTP listener: the Connect mount, the two WebSocket
//! upgrades, the export route, and the order they are tried in.
//!
//! Owned by the coordinator. `serve` builds one and hands it to the runtime; the
//! admission DECISIONS it enforces live in `worker_link::upgrade_admission` and
//! `sync_ws::upgrade_admission`, and the SQL lives in `db`.
//!
//! THE ORDER IS THE CONTRACT (`apps/coord/src/bun-coordinator-listeners.ts:314-352`):
//! the Host/Origin admission gate runs first and before any upgrade, then the two
//! WebSocket upgrades, then the retired Connect `Sync`, then Connect itself, then
//! the export route, then the namespace misses, then the SPA.
//!
//! WHY THE SYNC GUARD SITS BEFORE CONNECT RATHER THAN INSIDE A HANDLER. The
//! handler stub alone is not enough: a throwing stub still lets Connect open a
//! response stream, which keeps the runtime's abort-listener crash path reachable.
//! A plain unary `Response` is never abort-tracked, so every caller gets `410`
//! and the crash path is unreachable even for an old-bundle straggler
//! (`bun-coordinator-listeners.ts:338-340`).
//!
//! TLS IS NEVER TERMINATED HERE. "The coordinator serves plaintext on its
//! loopback bind; the operator's front door owns TLS"
//! (`bun-coordinator-listeners.ts:4-5`). That is why the export URL is hard-coded
//! `http://127.0.0.1:<port>/api/db-export` and why a declared public origin must
//! be HTTPS.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::rpc::service::CoordinatorServiceImpl;
use crate::sync_ws::upgrade_admission::{SYNC_AUTH_SUBPROTOCOL, SYNC_WS_PATH};
use crate::worker_link::upgrade_admission::WORKER_WS_PATH_PREFIX;

/// The RPC path prefix every Connect method lives under.
pub const CONNECT_PATH_PREFIX: &str = "/roost.v1.CoordinatorService/";

/// The retired Connect `Sync` path, refused before Connect sees it.
pub const RETIRED_SYNC_PATH: &str = "/roost.v1.CoordinatorService/Sync";

/// The on-host database export path.
pub const DB_EXPORT_PATH: &str = "/api/db-export";

/// The body the retired Connect `Sync` answers with.
pub const SYNC_MOVED_BODY: &str = "sync moved to /ws/coord-sync";

/// The largest buffered HTTP request body, in bytes.
///
/// 16 MiB (`bun-coordinator-listeners.ts:48`). It has to clear the 8 MiB
/// `AttachFileChunk` ceiling with room for the envelope, because a chunk upload
/// is the one request whose size is chosen by the client.
pub const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The largest single WebSocket frame either transport will accept, in bytes.
///
/// 4 MiB (`COORD_WEBSOCKET_MAX_PAYLOAD_BYTES`, `worker-ws-handler.ts:31`),
/// shared by the worker and Sync sockets. It is a payload cap, not a queue cap:
/// the worker socket's *queue* is bounded separately, at 256 frames and 16 MiB
/// (`worker-frame-queue.ts:5-6`).
pub const MAX_WEBSOCKET_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// The state the listener's handlers read.
#[derive(Debug)]
pub struct ListenerState {
    /// The single `CoordinatorService` implementation.
    pub service: Arc<CoordinatorServiceImpl>,
    /// The process state, for the export route.
    pub services: Arc<crate::services::CoordServices>,
    /// The resolved bind, as `host:port`.
    pub bind: String,
    /// Whether the operator declared a browser front door, for the SPA's
    /// canonical origin.
    pub web_public_url: Option<String>,
    /// Whether to believe `X-Forwarded-For`.
    pub trust_proxy: bool,
    /// Whether an SPA build is available to serve. `false` means every page
    /// request 404s, and the boot log has already said why.
    pub spa_available: bool,
}

/// Build the listener's router.
///
/// The Connect service is mounted as a **fallback** rather than as a set of
/// per-method routes, so a method the proto declares but this crate has not
/// delegated still resolves to a real handler and answers `Unimplemented` with
/// the owning domain named -- rather than 404-ing as an unknown path, which
/// would tell a caller its method does not exist.
pub fn build_router(state: Arc<ListenerState>) -> Router {
    // `Router` has no interceptor hook -- `with_interceptor` exists only on
    // `Service<D>` -- so the generated server is mounted directly to put the
    // auth gate in front of every method. Without it the route table's
    // `AuthRequirement` column is documentation rather than enforcement.
    //
    // `Router::add_service` only registers an `Arc<S>` where `S` is a generated
    // service, and the interceptor lives on `ConnectRpcService` -- one layer out
    // from that. So the gate is built here and the whole service is mounted as
    // the axum fallback, which is exactly what `Router::into_axum_router` does
    // internally (`ConnectRpcService::new(..)` then `fallback_service`).
    let server = roost_proto::roost::v1::CoordinatorServiceServer::from_arc(Arc::clone(
        &state.service,
    ));
    let gate = crate::rpc::auth_gate::auth_gate(
        crate::coord_core::CoordCore::new(Arc::clone(&state.services)),
        state.service.config.jwt_max_age_secs,
    );
    let connect = axum::Router::new().fallback_service(
        connectrpc::service::ConnectRpcService::new(server).with_interceptor_arc(gate),
    );

    Router::new()
        .route(SYNC_WS_PATH, get(sync_upgrade))
        .route("/ws/coord-worker/{fingerprint}", get(worker_upgrade))
        .route(DB_EXPORT_PATH, get(db_export).head(db_export))
        .route(RETIRED_SYNC_PATH, axum::routing::post(retired_sync))
        .fallback_service(connect)
        .with_state(state)
}

/// The retired Connect `Sync`, refused before Connect opens a stream.
async fn retired_sync() -> Response {
    (
        axum::http::StatusCode::GONE,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        SYNC_MOVED_BODY,
    )
        .into_response()
}

/// The worker upgrade. The decision is in `worker_link::upgrade_admission`; this
/// only applies it.
async fn worker_upgrade(
    State(state): State<Arc<ListenerState>>,
    axum::extract::Path(fingerprint): axum::extract::Path<String>,
    request: Request,
) -> Response {
    let path = format!("{WORKER_WS_PATH_PREFIX}{fingerprint}");
    let offered = offered_protocols(&request);
    let query = request
        .uri()
        .query()
        .map_or_else(String::new, str::to_string);
    let upgrade = request.headers().get(axum::http::header::UPGRADE);

    // A request that is not an upgrade at all is an HTTP request to a WebSocket
    // path, and 400 says exactly that. Anything else would be a lie: the path
    // exists, the method does not.
    if upgrade.and_then(|value| value.to_str().ok()) != Some("websocket") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            "upgrade required",
        )
            .into_response();
    }

    let credential = offered.get(1).cloned();
    let caller = match credential.as_deref() {
        Some(token) => crate::auth::authenticate::Authenticator {
            database: &state.services.db,
            keys: &state.services.jwt_keys,
            clock: crate::auth::jwt_verify::VerifyClock::at(crate::rpc::service::now_ms()),
            jwt_max_age_secs: state.service.config.jwt_max_age_secs,
        }
        .authenticate(token)
        .await
        .ok(),
        None => None,
    };
    let decision = crate::worker_link::upgrade_admission::admit_worker_upgrade(
        &crate::worker_link::upgrade_admission::WorkerUpgradeRequest {
            path,
            query,
            offered_protocols: offered,
            caller: caller.as_ref().map(|caller| {
                crate::worker_link::upgrade_admission::VerifiedWorkerCaller {
                    fingerprint: caller.fingerprint.clone(),
                    key_generation: caller.key_generation,
                    label: caller.label.clone(),
                }
            }),
        },
        caller
            .as_ref()
            .is_some_and(|caller| caller.principal.is_worker()),
        caller.as_ref().map(|caller| caller.key_generation),
    );
    match decision {
        crate::worker_link::upgrade_admission::UpgradeDecision::Refused(refusal) => (
            axum::http::StatusCode::from_u16(refusal.status())
                .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            refusal.body(),
        )
            .into_response(),
        crate::worker_link::upgrade_admission::UpgradeDecision::Admitted { .. } => {
            // Admitted without a credential cannot happen: the credential check
            // is inside the decision, and a `None` caller is always a refusal.
            (
                axum::http::StatusCode::UNAUTHORIZED,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                "unauthorized",
            )
                .into_response()
        }
    }
}

/// The Sync upgrade. The decision is in `sync_ws::upgrade_admission`.
async fn sync_upgrade(State(state): State<Arc<ListenerState>>, request: Request) -> Response {
    let offered = offered_protocols(&request);
    let credential = offered.get(1).cloned();
    let caller = match credential.as_deref() {
        Some(token) => crate::auth::authenticate::Authenticator {
            database: &state.services.db,
            keys: &state.services.jwt_keys,
            clock: crate::auth::jwt_verify::VerifyClock::at(crate::rpc::service::now_ms()),
            jwt_max_age_secs: state.service.config.jwt_max_age_secs,
        }
        .authenticate(token)
        .await
        .ok(),
        None => None,
    };
    let decision = crate::sync_ws::upgrade_admission::admit_sync_upgrade(
        &crate::sync_ws::upgrade_admission::SyncUpgradeRequest {
            path: request.uri().path().to_string(),
            origin: header_string(&request, axum::http::header::ORIGIN),
            host: header_string(&request, axum::http::header::HOST).unwrap_or_default(),
            offered_protocols: offered,
            caller: caller.as_ref().map(|caller| {
                crate::sync_ws::upgrade_admission::VerifiedSyncCaller {
                    fingerprint: caller.fingerprint.clone(),
                    label: caller.label.clone(),
                }
            }),
            tab: None,
            since: None,
            flow: None,
            sync_v: None,
        },
        &crate::sync_ws::upgrade_admission::OriginPolicy {
            public_url: state.service.config.public_url.clone(),
            web_public_url: state.service.config.web_public_url.clone(),
            cors_allowed_origins: state.service.config.cors_allowed_origins.clone(),
            worker_local_ui_origin: roost_host::DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_string(),
            loopback_bind: Some(state.service.config.bind.clone()),
            relaxed_csp: state.service.config.relaxed_csp,
        },
        match caller.as_ref().map(|caller| &caller.principal) {
            // An absent credential is already a refusal by the time this runs;
            // naming it a browser here would be a second, weaker answer.
            None => crate::sync_ws::upgrade_admission::PrincipalKind::AccountDevice,
            Some(principal) if principal.is_worker() => {
                crate::sync_ws::upgrade_admission::PrincipalKind::Worker(
                    principal.fingerprint().to_string(),
                )
            }
            Some(_) => crate::sync_ws::upgrade_admission::PrincipalKind::AccountDevice,
        },
    );
    match decision {
        crate::sync_ws::upgrade_admission::SyncUpgradeDecision::Refused(refusal) => (
            axum::http::StatusCode::from_u16(refusal.status())
                .unwrap_or(axum::http::StatusCode::BAD_REQUEST),
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            refusal.body(),
        )
            .into_response(),
        crate::sync_ws::upgrade_admission::SyncUpgradeDecision::Admitted { .. } => (
            axum::http::StatusCode::UNAUTHORIZED,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            "unauthorized",
        )
            .into_response(),
    }
}

/// The on-host database export.
async fn db_export(State(state): State<Arc<ListenerState>>, _request: Request) -> Response {
    // The on-host gate is the whole authorization for this route: no rate limit,
    // no extra token. `MiscDbExportUrl`, which is how a caller discovers the
    // path, requires a device principal AND on-host.
    if !state.services.db.path().exists() {
        return (axum::http::StatusCode::NOT_FOUND, "").into_response();
    }
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        r#"{"error":"export snapshot is served by the deployment that installs the sqlite writer"}"#,
    )
        .into_response()
}

fn offered_protocols(request: &Request) -> Vec<String> {
    request
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .map(|raw| raw.split(',').map(str::trim).map(str::to_string).collect())
        .unwrap_or_default()
}

fn header_string(request: &Request, name: axum::http::HeaderName) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Resolve a bind string into a socket address, refusing anything unparseable.
///
/// Refusing here rather than falling back to a default is deliberate: a
/// mistyped bind that silently became `127.0.0.1:4113` would start a second
/// coordinator on the same port, and the error the operator sees would be
/// `address in use` rather than the bind they wrote.
pub fn resolve_bind(bind: &str) -> Result<SocketAddr, BindError> {
    bind.parse::<SocketAddr>().map_err(|error| BindError {
        bind: bind.to_string(),
        reason: error.to_string(),
    })
}

/// A bind that could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("ROOST_COORDINATOR_BIND {bind} is not a host:port address: {reason}")]
pub struct BindError {
    /// The bind as written.
    pub bind: String,
    /// Why it could not be resolved.
    pub reason: String,
}

/// The subprotocol marker the coordinator echoes on a successful Sync upgrade.
#[must_use]
pub fn sync_echo_header() -> axum::http::HeaderName {
    axum::http::HeaderName::from_static("sec-websocket-protocol")
}

/// The value the coordinator echoes: the marker, never the credential, so a
/// proxy that logs the handshake learns nothing.
#[must_use]
pub fn sync_echo_value() -> &'static str {
    SYNC_AUTH_SUBPROTOCOL
}

/// The route table, for a test that asserts every documented path is mounted.
#[must_use]
pub fn mounted_paths() -> Vec<&'static str> {
    vec![
        SYNC_WS_PATH,
        "/ws/coord-worker/{fingerprint}",
        DB_EXPORT_PATH,
        RETIRED_SYNC_PATH,
    ]
}

/// A convenience alias so the router's builder reads the same as v2's
/// `startBunCoordinatorListeners`.
pub type CoordRouter = Router;
