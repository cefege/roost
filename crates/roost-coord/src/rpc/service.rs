//! The single `CoordinatorService` implementation's shared half: the type, the
//! replies it can already give, and the honest answer for the rest.
//!
//! Owned by the coordinator's RPC layer. ALL 103 methods live in ONE
//! `impl CoordinatorService` block in the sibling `service_impl.rs`, because Rust
//! forbids splitting a trait implementation across blocks (E0119) even when the
//! method names are disjoint. There is no per-domain `service_*.rs` and there
//! cannot be one; a domain slice's methods are wired in by a single integration
//! pass. Rust requires every method, so a missing delegation is a compile error
//! -- not the silent 501 that a second `router.service()` call produced in v2
//! (`apps/coord/src/rpc/router.ts:114-118`).
//!
//! WHY THE UNPORTED METHODS RETURN A NAMED `Unimplemented` RATHER THAN A STUB.
//! 87 of v2's 103 methods are answered by per-domain handler modules this slice
//! does not port. The reply names the owning domain from
//! [`METHOD_ROUTES`](super::method_route::METHOD_ROUTES), so a log line reads
//! `WorkersList: the workers domain is not ported in this slice` rather than a
//! bare "not implemented" -- which would be indistinguishable from the sixteen
//! methods v2 genuinely does not answer. `tests/method_route_coverage.rs` guards
//! that the set of sixteen is exactly right.
//!
//! `MiscHealth` and `MiscDbExportUrl` ARE answered, because they are the two the
//! transport layer in this crate depends on: the first is the readiness signal a
//! load balancer and `roost status` both read, and the second is the only
//! discoverable path to the export snapshot this crate's listener serves.

use connectrpc::{ConnectError, Encodable, ErrorCode, Response, ServiceResult, Spec};

use super::method_route::{AuthRequirement, all_method_routes};
use crate::auth::principal::{AUTH_LAYER_DEVICE, AUTH_LAYER_HEADER, Principal};
use crate::coord_core::CoordCore;

/// The one `CoordinatorService` implementation.
///
/// Holds what the handlers need that is not a request: the shared process
/// state, the resolved config, the process epoch, and the boot instant
/// `MiscHealth` reports. Domain state lives behind [`CoordCore`] rather than
/// here, so this stays a name rather than a second place state lives -- adding
/// a per-process singleton must not widen the file every domain slice also
/// has to edit.
#[derive(Debug)]
pub struct CoordinatorServiceImpl {
    /// The per-process state every domain handler is handed.
    pub core: CoordCore,
    /// The resolved, validated coordinator configuration.
    pub config: roost_host::CoordConfig,
    /// A fresh identity per process, so a log line can distinguish a restart
    /// from a reconnect.
    pub process_epoch: String,
    /// When this process started, in epoch milliseconds.
    pub boot_ms: i64,
    /// The coordinator's own build SHA, for `MiscHealth`.
    pub git_sha: String,
}

impl CoordinatorServiceImpl {
    /// A service over an already-resolved configuration.
    ///
    /// The config is not re-validated here: `roost_host::load_coord_config` is
    /// the one loader and it refuses before anything starts, so a second
    /// validation would be a second answer to the same question.
    #[must_use]
    pub fn new(
        core: CoordCore,
        config: roost_host::CoordConfig,
        process_epoch: String,
        boot_ms: i64,
        git_sha: String,
    ) -> Self {
        Self {
            core,
            config,
            process_epoch,
            boot_ms,
            git_sha,
        }
    }

    /// The port this coordinator listens on, from its bind.
    #[must_use]
    pub fn listen_port(&self) -> Option<u16> {
        self.config
            .bind
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
    }
}

/// The answer for a method whose domain is not in this slice.
///
/// Names the domain so the failure says who owns the work, and never claims the
/// method is retired -- sixteen of the 103 are, and those say so separately.
#[must_use]
pub fn unimplemented_for_domain(method: &str) -> ConnectError {
    let domain = super::method_route::owning_domain(method).unwrap_or("unassigned");
    ConnectError::new(
        ErrorCode::Unimplemented,
        format!("{method}: the {domain} domain is not ported in this slice"),
    )
}

/// The answer for a method v2 never answered either.
#[must_use]
pub fn unimplemented_in_v2(method: &str) -> ConnectError {
    ConnectError::new(
        ErrorCode::Unimplemented,
        format!("{method}: declared in the proto, unwired in the v2 coordinator too"),
    )
}

/// A ready error, typed to the method's response body.
///
/// The `Ready` captures neither `&self` nor the request lifetime, which is what
/// satisfies the generated `use<'a, Self>` return clause: a handler that
/// borrowed the request into its error would not compile.
pub async fn reply_error<Out: Encodable<Out> + Send + 'static>(
    error: ConnectError,
) -> ServiceResult<Out> {
    Err(error)
}

/// The reply a method that is not answered here produces.
///
/// A single funnel so the status of a method and the wording of its refusal
/// cannot drift: the status comes from the route table, which is the one place
/// it is written down.
pub async fn delegated_reply<Out: Encodable<Out> + Send + 'static>(
    method: &str,
) -> ServiceResult<Out> {
    let status = all_method_routes()
        .iter()
        .find(|route| route.method == method)
        .map(|route| route.status);
    let error = match status {
        Some(super::method_route::PortStatus::UnwiredInV2) => unimplemented_in_v2(method),
        _ => unimplemented_for_domain(method),
    };
    reply_error::<Out>(error).await
}

/// The `ConnectError` for a call whose principal is not allowed.
#[must_use]
pub fn permission_denied_for(requirement: AuthRequirement) -> ConnectError {
    let reason = match requirement {
        AuthRequirement::Public => "public method",
        AuthRequirement::Device => "device principal required",
        AuthRequirement::DeviceOnHost => "device principal required, on-host only",
        AuthRequirement::Worker => "worker principal required",
        AuthRequirement::DeviceOrOwnWorkerRecovery => {
            "device principal required, or a worker restricted to its own recovery"
        }
        AuthRequirement::DevicePlusFence => "device principal and a matching fence required",
        // Reached only if a caller reaches a method with no handler, which the
        // delegation funnel turns into `Unimplemented` before this point. The arm
        // exists so adding the variant is a compile error rather than a panic.
        AuthRequirement::Unwired => "no handler is registered for this method",
    };
    let mut error = ConnectError::new(ErrorCode::PermissionDenied, reason);
    // The marker is only meaningful for a device-layer refusal; on a worker
    // requirement it would tell the browser the wrong thing about which
    // credential to present.
    //
    // `axum::http` is the same `http` 1.x connectrpc's `HeaderMap` is built
    // on, so naming the types costs no extra dependency and no second type
    // universe.
    if matches!(
        requirement,
        AuthRequirement::Device | AuthRequirement::DeviceOnHost
    ) {
        error.response_headers_mut().insert(
            axum::http::HeaderName::from_static(AUTH_LAYER_HEADER),
            axum::http::HeaderValue::from_static(AUTH_LAYER_DEVICE),
        );
    }
    error
}

/// Whether a principal satisfies a method's recorded requirement.
#[must_use]
pub fn principal_satisfies(principal: Option<&Principal>, requirement: AuthRequirement) -> bool {
    match requirement {
        AuthRequirement::Public => true,
        AuthRequirement::Device | AuthRequirement::DeviceOnHost => {
            principal.is_some_and(Principal::is_browser)
        }
        AuthRequirement::Worker => principal.is_some_and(Principal::is_worker),
        AuthRequirement::DeviceOrOwnWorkerRecovery => {
            principal.is_some_and(Principal::is_browser)
                || principal.is_some_and(Principal::is_worker)
        }
        AuthRequirement::DevicePlusFence => principal.is_some_and(Principal::is_browser),
        // Nothing enforces a requirement nothing checks, so nothing passes it.
        AuthRequirement::Unwired => false,
    }
}

/// The `MiscHealth` reply. Public, so it needs no credential -- which is what
/// lets a load balancer probe it.
#[must_use]
pub fn misc_health_reply(
    service: &CoordinatorServiceImpl,
    now_ms: i64,
) -> roost_proto::MiscHealthResponse {
    roost_proto::MiscHealthResponse {
        ok: true,
        boot_ms: u64::try_from(service.boot_ms).unwrap_or(0),
        uptime_ms: u64::try_from(now_ms.saturating_sub(service.boot_ms).max(0)).unwrap_or(0),
        git_sha: service.git_sha.clone(),
        // Generated messages carry an unknown-field sink so a peer that sent a
        // field this build does not know survives a decode/encode round trip.
        // A reply this coordinator constructed has never had one.
        ..Default::default()
    }
}

/// The `MiscDbExportUrl` reply, the only discoverable path to the export
/// snapshot the listener serves.
#[must_use]
pub fn db_export_url(service: &CoordinatorServiceImpl) -> String {
    // A bind with no parseable port yields the bare origin rather than an empty
    // string: an empty `url` would read to a browser as "the coordinator has no
    // export route", and the truth is "this build cannot read its own bind".
    match service.listen_port() {
        Some(port) => format!("http://127.0.0.1:{port}/api/db-export"),
        None => format!("http://{}/api/db-export", service.config.bind),
    }
}

/// A method's `Spec` rendered for a log line, as `Service/Method`.
#[must_use]
pub fn spec_label(spec: &Spec) -> String {
    format!("{}/{}", spec.service(), spec.method())
}

/// Wrap a body in the response envelope the generated handlers return.
pub fn ok_response<Out: Encodable<Out> + Send + 'static>(body: Out) -> ServiceResult<Out> {
    Response::ok(body)
}

/// A ready error shaped as a server-streaming reply, for the two streaming
/// methods this crate does not answer.
///
/// `ServiceStream` is built from an empty stream, so a refusal here still opens
/// and immediately closes a stream rather than throwing before one exists. That
/// matters for `Sync` specifically: v2's reason for refusing at the FETCH layer
/// rather than in the handler was that a throwing stub still lets Connect open a
/// response stream, keeping the runtime's abort-listener crash path reachable
/// (`apps/coord/src/bun-coordinator-listeners.ts:338-340`). An empty stream
/// reaches neither failure.
pub async fn delegated_stream<Out: Encodable<Out> + Send + 'static>(
    method: &str,
) -> ServiceResult<connectrpc::ServiceStream<Out>> {
    let status = all_method_routes()
        .iter()
        .find(|route| route.method == method)
        .map(|route| route.status);
    let error = match status {
        Some(super::method_route::PortStatus::UnwiredInV2) => unimplemented_in_v2(method),
        _ => unimplemented_for_domain(method),
    };
    Err(error)
}

/// The `Sync` refusal, which names where the method moved rather than which
/// domain owns it: the live path is the WebSocket at `/ws/coord-sync`
/// (`protocol/spec/sync.md:9`).
pub async fn sync_moved_stream<Out: Encodable<Out> + Send + 'static>()
-> ServiceResult<connectrpc::ServiceStream<Out>> {
    Err(ConnectError::new(
        ErrorCode::Unimplemented,
        "sync moved to /ws/coord-sync",
    ))
}

/// The process clock, in epoch milliseconds.
///
/// One function so `MiscHealth` and the listener's `now_ms` cannot disagree, and
/// so a test can see exactly where time enters the crate: everywhere else takes
/// `now_ms` as a parameter.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
