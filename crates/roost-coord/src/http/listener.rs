//! The coordinator's HTTP listener: the Connect mount, its WebSocket upgrades,
//! the export route, the middleware stack in front of all of it, and the order
//! they are tried in.
//!
//! Owned by the coordinator. `serve` builds one and hands it to the runtime; the
//! upgrade DECISIONS live in `http::upgrade`, the Host/Origin gate's decisions
//! live in `http_admission`, and the SQL lives in `db`.
//!
//! v2's `apps/coord/src/rpc/bun-handler.ts` is this mount on Bun's server:
//! `build_router` is its `makeConnectBunHandler`, and the trust-header
//! strip-then-set is `middleware::caller_origin`.
//!
//! THE ORDER IS THE CONTRACT (`apps/coord/src/bun-coordinator-listeners.ts:314-352`).
//! Outside in:
//!
//! 1. `middleware::admission_layer` -- the Host/Origin gate. Outermost, and
//!    around the whole router rather than around the upgrade routes alone,
//!    because v2 runs it first thing in the fetch handler: a request it refuses
//!    must not reach Connect either, or the same DNS-rebinding request is one
//!    RPC path removed from a refused one.
//! 2. `middleware::caller_origin` -- the caller's real address and on-host
//!    authority, resolved once for everything below. It reads a forwarded
//!    header, so it has to sit where that header is still only a claim -- and
//!    above the limiter, whose key is the address it resolves.
//! 3. `middleware::security` -- the CORS preflight and the response headers.
//!    Above the limiter because a browser sends a preflight before every
//!    non-simple request, and v2 answers `OPTIONS` before it checks a budget
//!    (`coord-factory.ts:161-163`): charging the preflight would halve every
//!    real caller's budget.
//! 4. `middleware::rate_limit_layer` -- one request budget per client, spent by
//!    Connect paths only.
//! 5. `middleware::audit_layer` -- one audit row per non-Connect response.
//! 6. `http::spa` -- the browser's front door, inside the audit mount so a page
//!    load is audited like every other non-Connect response, and OUTSIDE
//!    Connect because a path is a page before it is an RPC.
//! 7. The WebSocket upgrades, then the retired Connect `Sync`, then Connect
//!    itself, then the export route, then the namespace misses.
//!
//! `Router::layer` wraps what is already there, so the LAST layer applied is
//! the OUTERMOST one, and the mounting order in [`build_router`] is the reverse
//! of the list above on purpose. Reordering it to read the way this comment
//! reads is exactly the mistake this comment exists to prevent: putting
//! admission below the security layer decorates a refusal with CORS headers and
//! makes a refused origin look answerable, putting it below `caller_origin`
//! lets an unauthenticated prober spend rate-limit budget and write audit rows,
//! and putting the limiter above `caller_origin` charges every request to the
//! proxy's address rather than the caller's.
//!
//! These six things stay in ONE file for that reason. The order is the
//! contract, and a contract split across two files is mounted in the wrong
//! sequence by the next person who edits either half.
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
//! `http://127.0.0.1:<port>/api/db-export`, why a declared public origin must be
//! HTTPS, and why HSTS follows `trust_proxy` rather than the request's scheme.

use std::sync::Arc;

use axum::Router;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::coord_core::CoordCore;
use crate::http::db_export::db_export;
use crate::http::health::{HEALTHZ_PATH, READYZ_PATH};
use crate::http::spa::{self, SpaMount};
use crate::http::upgrade::{sync_upgrade, worker_upgrade};
use crate::middleware::admission_layer::{AdmissionLayer, admission_gate};
use crate::middleware::audit_layer::{AuditMount, audit_layer};
use crate::middleware::caller_origin::{CallerTrust, caller_origin_layer, listener_trust};
use crate::middleware::rate_limit_layer::rate_limit_layer;
use crate::middleware::security::{security_layer, security_options_for_config};
use crate::rpc::auth_gate::AuthGate;
use crate::rpc::service::CoordinatorServiceImpl;
use crate::sync_ws::upgrade_admission::{SYNC_AUTH_SUBPROTOCOL, SYNC_WS_PATH};

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
    /// The one web build this process serves, chosen at boot. `root()` is
    /// `None` when the configured path holds no `index.html`, and every page
    /// request then 404s, which is what the boot log has already said why.
    pub spa: Arc<SpaMount>,
    /// Raised once, on shutdown, so `/readyz` turns traffic away while open
    /// connections drain.
    pub draining: Arc<std::sync::atomic::AtomicBool>,
}

/// The router, plus the one thing about it that only the bind knows.
///
/// The admission gate's allowlist names the RESOLVED port, so the gate cannot be
/// finished until the listener knows which port the OS gave it. `serve` binds,
/// reads `local_addr`, and calls [`MountedListener::publish_bound_port`]; until
/// it does, the gate answers `503 listener unavailable` to everything, on both
/// WebSocket routes as much as on Connect.
#[derive(Debug, Clone)]
pub struct MountedListener {
    /// The router to serve.
    pub router: Router,
    admission: Arc<AdmissionLayer>,
}

impl MountedListener {
    /// Tell the admission gate which port the listener actually bound.
    ///
    /// NOT THE SAME INSTANT AS THE BIND, and the gap is the point: a socket is
    /// bound before its address has been read, and a request arriving in that
    /// gap must be refused rather than answered from a guess. Moving this call
    /// up next to the bind reopens exactly that hole, and it is the obvious
    /// "simplification" this method must never become.
    pub fn publish_bound_port(&self, port: u16) {
        self.admission.publish_bound_port(port);
    }
}

/// Build the listener's router, middleware stack included.
///
/// The Connect service is mounted as a **fallback** rather than as a set of
/// per-method routes, so a method the proto declares but this crate has not
/// delegated still resolves to a real handler and answers `Unimplemented` with
/// the owning domain named -- rather than 404-ing as an unknown path, which
/// would tell a caller its method does not exist.
#[must_use]
pub fn build_router(state: Arc<ListenerState>) -> MountedListener {
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
    let server =
        roost_proto::roost::v1::CoordinatorServiceServer::from_arc(Arc::clone(&state.service));
    let core = Arc::new(CoordCore::new(Arc::clone(&state.services)));
    // The gate's locality rule is chosen from the same boot setting the caller's
    // origin profile is, so the two can never disagree about whether a
    // connection is direct.
    let trust = listener_trust(state.service.config.trust_proxy);
    let gate = Arc::new(AuthGate::new(
        CoordCore::new(Arc::clone(&state.services)),
        state.service.config.jwt_max_age_secs,
        trust,
    ));
    // connectrpc's default is 4 MiB, which a full 4 MiB relay chunk plus its
    // envelope exceeds; the whole body and the one message share the cap.
    let limits = connectrpc::service::Limits::default()
        .with_max_request_body_size(MAX_REQUEST_BODY_BYTES)
        .with_max_message_size(MAX_REQUEST_BODY_BYTES);
    let connect = axum::Router::new().fallback_service(
        connectrpc::service::ConnectRpcService::new(server)
            .with_limits(limits)
            .with_interceptor_arc(gate),
    );

    let admission = Arc::new(AdmissionLayer::from_config(&state.service.config));
    let security = Arc::new(security_options_for_config(&state.service.config));
    let audit = AuditMount::new(Arc::clone(&core), state.spa.root().is_some());
    let spa = Arc::clone(&state.spa);
    let services = Arc::clone(&state.services);
    let caller_trust = CallerTrust {
        listener: trust,
        proxy_cidrs: state.service.config.trusted_proxy_cidrs.clone().into(),
    };

    // Reverse order, outermost first; see the module header. Each layer is
    // added to the router, so the LAST one added is the first one a request
    // meets.
    let router = Router::new()
        .route(SYNC_WS_PATH, get(sync_upgrade))
        .route("/ws/coord-worker/{fingerprint}", get(worker_upgrade))
        .route(DB_EXPORT_PATH, get(db_export).head(db_export))
        .route(RETIRED_SYNC_PATH, post(retired_sync))
        .merge(crate::http::health::health_routes())
        .fallback_service(connect)
        // The SPA is the fallback for the paths nothing above claims, and it is
        // mounted as a layer rather than a route so the two upgrade routes and
        // the export are still reached through the router untouched. The LAST
        // layer added is the OUTERMOST one, so this sits outside Connect and
        // inside the audit mount -- one audit row per page, as in v2.
        .layer(axum::middleware::from_fn_with_state(spa, spa::spa_layer))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(audit, audit_layer))
        .layer(axum::middleware::from_fn_with_state(
            services,
            rate_limit_layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            security,
            security_layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            caller_trust,
            caller_origin_layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&admission),
            admission_gate,
        ));

    MountedListener { router, admission }
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
        HEALTHZ_PATH,
        READYZ_PATH,
    ]
}

/// A convenience alias so the router's builder reads the same as v2's
/// `startBunCoordinatorListeners`.
pub type CoordRouter = Router;
