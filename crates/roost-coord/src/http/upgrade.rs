//! The two WebSocket upgrade lifecycles: the worker's raw link and the
//! browser's Sync firehose.
//!
//! Owned by the coordinator's HTTP layer. Both handlers read the credential out
//! of `sec-websocket-protocol` and then apply a decision that belongs to
//! `worker_link::upgrade_admission` and `sync_ws::upgrade_admission`
//! respectively; this module is the transport around those decisions, and the
//! only two things it knows are the offered subprotocols and the two header
//! lookups both need.
//!
//! They are one module because they are one shape. The host/port decisions
//! above them are not here, and neither is the middleware stack in front of
//! them: a request that reaches either handler has already passed the admission
//! gate, so a Host or Origin rule restated here would be a second answer to a
//! question `http_admission` has already decided.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};

use crate::http::listener::ListenerState;
use crate::worker_link::upgrade_admission::WORKER_WS_PATH_PREFIX;

/// The worker upgrade. The decision is in `worker_link::upgrade_admission`; this
/// only applies it.
pub async fn worker_upgrade(
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
pub async fn sync_upgrade(State(state): State<Arc<ListenerState>>, request: Request) -> Response {
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

/// The subprotocols this request offers, in the order it offered them.
///
/// The credential is the SECOND entry by contract, and the order matters: a
/// client that offers one subprotocol is offering a marker, not a credential,
/// and reading position zero as one would authenticate nothing.
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
