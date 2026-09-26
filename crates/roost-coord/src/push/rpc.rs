//! The three Web Push Connect methods: get-config, subscribe, unsubscribe.
//!
//! Owned by the push domain; `rpc/service_impl.rs` delegates to these by name.
//! Ported from `apps/coord/src/push/handlers-push.ts`.
//!
//! PROVIDER ORIGINS ARE OPERATOR CONFIG AND AN EMPTY ALLOWLIST IS OFF. Not
//! "allow anything": `PushGetConfig` reports `available: false` and
//! `PushSubscribe` refuses, so a deployment with no push service never creates
//! a VAPID identity and never accumulates subscription rows
//! (`handlers-push.ts:67-71,82-83`).
//!
//! THE SUBSCRIBE REFUSAL NAMES THE CAP. `ResourceExhausted` with a message
//! carrying the number, because the browser shows it and a support report
//! saying "limit reached" is actionable where "invalid argument" is not.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};

use roost_proto::{PushGetConfigRequest, PushGetConfigResponse, PushSubscribeRequest};
use roost_proto::{PushSubscribeResponse, PushUnsubscribeRequest, PushUnsubscribeResponse};

use crate::coord_core::{Caller, CoordCore};
use crate::push::PushRuntime;
use crate::push::endpoint_policy::{PushInputError, validate_endpoint, validate_key};
use crate::push::subscription_store::{remove_subscription, store_subscription};
use crate::rpc::service::ok_response;

/// `CoordinatorService.PushGetConfig` — the browser's VAPID public key.
///
/// A device is required even though the answer is a public key: the method
/// tells a paired browser whether Push is on, and an unpaired caller has no
/// business asking (`handlers-push.ts:66`).
pub async fn handle_push_get_config(
    core: &CoordCore,
    caller: &Caller,
    _request: PushGetConfigRequest,
) -> ServiceResult<PushGetConfigResponse> {
    require_account_device(caller)?;
    let push = push_runtime(core)?;
    if push.allowed_origins().is_empty() {
        // No VAPID state is created on this path, and that is the point: a
        // deployment that never enabled Push must not mint an identity it will
        // never use.
        return ok_response(PushGetConfigResponse {
            vapid_public_key_b64: String::new(),
            available: false,
            ..Default::default()
        });
    }
    let keys = push
        .keys(&core.services.db)
        .await
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;
    ok_response(PushGetConfigResponse {
        vapid_public_key_b64: keys.public_key,
        available: true,
        ..Default::default()
    })
}

/// `CoordinatorService.PushSubscribe` — store or refresh this device's endpoint.
///
/// The cap is enforced inside the upsert statement, not here; see
/// [`crate::push::subscription_store`].
pub async fn handle_push_subscribe(
    core: &CoordCore,
    caller: &Caller,
    request: PushSubscribeRequest,
) -> ServiceResult<PushSubscribeResponse> {
    let viewer_fp = require_account_device(caller)?;
    let push = push_runtime(core)?;
    let allowed_origins = push.allowed_origins();
    validate_endpoint(&request.endpoint, allowed_origins).map_err(refuse_input)?;
    validate_key("p256dh", &request.p256dh).map_err(refuse_input)?;
    validate_key("auth", &request.auth).map_err(refuse_input)?;

    store_subscription(
        core.services.db.pool(),
        push.dashboard_id(),
        viewer_fp,
        &request.endpoint,
        &request.p256dh,
        &request.auth,
        crate::rpc::service::now_ms(),
    )
    .await
    .map_err(refuse_input)?;

    ok_response(PushSubscribeResponse {
        ok: true,
        ..Default::default()
    })
}

/// `CoordinatorService.PushUnsubscribe` — drop this device's endpoint.
///
/// The endpoint is validated exactly as on subscribe, including the allowlist
/// membership. That is deliberate: an unsubscribe for an origin the operator
/// has since removed is a no-op either way, and refusing it tells the browser
/// the truth rather than acknowledging a delete that never applied.
pub async fn handle_push_unsubscribe(
    core: &CoordCore,
    caller: &Caller,
    request: PushUnsubscribeRequest,
) -> ServiceResult<PushUnsubscribeResponse> {
    let viewer_fp = require_account_device(caller)?;
    let push = push_runtime(core)?;
    validate_endpoint(&request.endpoint, push.allowed_origins()).map_err(refuse_input)?;

    remove_subscription(core.services.db.pool(), viewer_fp, &request.endpoint)
        .await
        .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))?;

    ok_response(PushUnsubscribeResponse {
        ok: true,
        ..Default::default()
    })
}

/// The push runtime, or a wiring error.
///
/// `CoordCore::new` installs a runtime with an empty allowlist, which reads as
/// "Push is off" rather than as a crash. Reaching the internal error means the
/// coordinator was built without a tenancy scope, which is a boot-order fault
/// and must not read to a browser as "push is merely unavailable".
fn push_runtime(core: &CoordCore) -> Result<&PushRuntime, ConnectError> {
    core.push.as_ref().ok_or_else(|| {
        ConnectError::new(
            ErrorCode::Internal,
            "push runtime is not installed: the coordinator was built without a tenancy scope",
        )
    })
}

/// Refuse anything that is not a browser, with the marker header a client
/// needs to tell "log in again" from "this method needs a device credential"
/// (`auth-interceptor.ts:265-271`).
fn require_account_device(caller: &Caller) -> Result<&str, ConnectError> {
    caller.principal.require_account_device().map_err(|_| {
        let mut error = ConnectError::new(ErrorCode::Unauthenticated, "authentication required");
        error.response_headers_mut().insert(
            axum::http::HeaderName::from_static(crate::auth::principal::AUTH_LAYER_HEADER),
            axum::http::HeaderValue::from_static(crate::auth::principal::AUTH_LAYER_DEVICE),
        );
        error
    })
}

/// Map a refused input onto its Connect status.
///
/// Three statuses and no more, because each one tells the browser something
/// different: `FailedPrecondition` means "this deployment has Push off, stop
/// asking" (and the browser must not retry), `InvalidArgument` means "fix the
/// request", and `ResourceExhausted` means "this device is full" (429, which
/// the browser backs off from). Collapsing them would make a client retry a
/// malformed request forever.
fn refuse_input(error: PushInputError) -> ConnectError {
    let code = match &error {
        PushInputError::Unavailable => ErrorCode::FailedPrecondition,
        PushInputError::InvalidEndpoint | PushInputError::InvalidKey(_) => {
            ErrorCode::InvalidArgument
        }
        PushInputError::DeviceCapReached => ErrorCode::ResourceExhausted,
        // A statement failure is the coordinator's fault, not the caller's, and
        // reporting it as `InvalidArgument` would send a browser into a retry
        // loop against a database that is already unhappy.
        PushInputError::Store(_) => {
            return ConnectError::new(ErrorCode::Internal, error.to_string());
        }
    };
    ConnectError::new(code, error.to_string())
}

/// The Connect method each handler answers, for the single delegation pass in
/// `rpc/service_impl.rs`.
pub const METHODS: &[(&str, &str)] = &[
    ("PushGetConfig", "handle_push_get_config"),
    ("PushSubscribe", "handle_push_subscribe"),
    ("PushUnsubscribe", "handle_push_unsubscribe"),
];
